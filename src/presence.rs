//! Présence, comme sur Discord : en ligne, inactif, ne pas déranger, invisible ; passage
//! automatique en « inactif » quand l'ordinateur n'est plus utilisé. Règles des notifications
//! (préférences, mentions), appliquées par `Inner::notify`.
//!
//! La présence part dans chaque annonce (`Hello.presence`, toutes les 2 s). « Invisible »
//! s'annonce « hors ligne » : les autres nous voient hors ligne, mais les messages passent.

use crate::{
    engine::{lock, Engine},
    store::Settings,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Présence choisie (réglage) ou annoncée par un autre appareil.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Presence {
    #[default]
    Online,
    Idle,
    /// Ne pas déranger : aucune notification.
    Dnd,
    /// Choisi seulement : annoncé `Offline`.
    Invisible,
    /// Annoncé par un appareil invisible (ou pas vu depuis un moment).
    Offline,
}

impl Presence {
    pub fn label(self) -> &'static str {
        match self {
            Presence::Online => "En ligne",
            Presence::Idle => "Inactif",
            Presence::Dnd => "Ne pas déranger",
            Presence::Invisible => "Invisible",
            Presence::Offline => "Hors ligne",
        }
    }

    /// Ce que les autres voient.
    pub fn announced(self) -> Presence {
        if self == Presence::Invisible { Presence::Offline } else { self }
    }

    /// 0 hors ligne, 1 en ligne, 2 inactif, 3 ne pas déranger (pastilles de l'interface).
    pub fn code(self) -> i32 {
        match self {
            Presence::Online => 1,
            Presence::Idle => 2,
            Presence::Dnd => 3,
            Presence::Invisible | Presence::Offline => 0,
        }
    }

    /// Présences proposées dans le menu.
    pub const CHOICES: [Presence; 4] = [Presence::Online, Presence::Idle, Presence::Dnd, Presence::Invisible];
}

/// Quels messages déclenchent une notification d'un groupe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroupNotifications {
    #[default]
    All,
    Mentions,
    Never,
}

/// Préférences des notifications (`settings.json` : `notifications`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NotificationPrefs {
    pub enabled: bool,
    /// Messages des discussions à deux.
    pub direct: bool,
    pub groups: GroupNotifications,
    /// Montrer le texte du message (sinon : « Nouveau message »).
    pub preview: bool,
    pub sound: bool,
    /// Aussi quand MagicPlus est au premier plan sur cette discussion.
    pub when_focused: bool,
}

impl Default for NotificationPrefs {
    fn default() -> Self {
        Self { enabled: true, direct: true, groups: GroupNotifications::All, preview: true, sound: true, when_focused: false }
    }
}

/// Décide si une notification mérite d'être montrée (préférences et présence).
pub fn should_notify(kind: crate::engine::NoticeKind, prefs: &NotificationPrefs, presence: Presence) -> bool {
    use crate::engine::NoticeKind;
    if !prefs.enabled || presence == Presence::Dnd {
        return false;
    }
    match kind {
        NoticeKind::Direct => prefs.direct,
        NoticeKind::Group { mention } => match prefs.groups {
            GroupNotifications::All => true,
            GroupNotifications::Mentions => mention,
            GroupNotifications::Never => false,
        },
        NoticeKind::Other => true,
    }
}

/// Lit les préférences, ou l'ancien réglage (un booléen : activées ou non).
pub fn prefs_or_bool<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<NotificationPrefs, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Prefs(NotificationPrefs),
        Enabled(bool),
    }
    Ok(match Either::deserialize(deserializer)? {
        Either::Prefs(prefs) => prefs,
        Either::Enabled(enabled) => NotificationPrefs { enabled, ..NotificationPrefs::default() },
    })
}

/// Vrai si `text` mentionne l'un de `names` (« @Léa », sans tenir compte des majuscules).
pub fn mentions(text: &str, names: &[String]) -> bool {
    let lower = text.to_lowercase();
    lower.contains("@everyone")
        || lower.contains("@tous")
        || names.iter().filter(|n| !n.trim().is_empty()).any(|name| lower.contains(&format!("@{}", name.trim().to_lowercase())))
}

// ------------------------------------------------------------------ inactivité du système

/// Temps écoulé depuis la dernière action au clavier ou à la souris (tout le système).
#[cfg(target_os = "windows")]
pub fn system_idle() -> Option<Duration> {
    #[repr(C)]
    struct LastInputInfo {
        size: u32,
        time: u32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn GetLastInputInfo(info: *mut LastInputInfo) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetTickCount() -> u32;
    }
    let mut info = LastInputInfo { size: std::mem::size_of::<LastInputInfo>() as u32, time: 0 };
    // SAFETY : structure de la taille annoncée, remplie par Windows.
    let ok = unsafe { GetLastInputInfo(&mut info) } != 0;
    // SAFETY : fonction sans argument.
    let now = unsafe { GetTickCount() };
    ok.then(|| Duration::from_millis(now.wrapping_sub(info.time) as u64))
}

#[cfg(target_os = "macos")]
pub fn system_idle() -> Option<Duration> {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    }
    // État combiné de la session, n'importe quel événement d'entrée.
    // SAFETY : fonction sans effet de bord.
    let seconds = unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) };
    (seconds.is_finite() && seconds >= 0.0).then(|| Duration::from_secs_f64(seconds))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn system_idle() -> Option<Duration> {
    None
}

/// Présence effective : le choix, sauf « en ligne » qui devient « inactif » après
/// `auto_idle_minutes` sans utiliser l'ordinateur (0 : jamais).
pub fn effective(settings: &Settings, idle: Option<Duration>) -> Presence {
    match settings.presence {
        Presence::Online if settings.auto_idle_minutes > 0 => match idle {
            Some(idle) if idle >= Duration::from_secs(settings.auto_idle_minutes as u64 * 60) => Presence::Idle,
            _ => Presence::Online,
        },
        Presence::Offline => Presence::Invisible,
        chosen => chosen,
    }
}

impl Engine {
    /// Présence effective (choix et inactivité).
    pub fn presence(&self) -> Presence {
        effective(&lock(&self.inner.settings), system_idle())
    }

    pub fn set_presence(&self, presence: Presence) -> anyhow::Result<()> {
        self.update_settings(|settings| settings.presence = presence)?;
        self.set_status(format!("Statut : {}", presence.label()));
        Ok(())
    }

    /// Dossier des données de cet appareil.
    pub fn data_dir(&self) -> std::path::PathBuf {
        self.inner.store.dir().to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_presence_goes_idle_and_keeps_choices() {
        let mut settings = Settings::default();
        assert_eq!(effective(&settings, Some(Duration::from_secs(5))), Presence::Online);
        assert_eq!(effective(&settings, Some(Duration::from_secs(11 * 60))), Presence::Idle);
        settings.auto_idle_minutes = 0;
        assert_eq!(effective(&settings, Some(Duration::from_secs(3600))), Presence::Online);
        settings.presence = Presence::Dnd;
        assert_eq!(effective(&settings, Some(Duration::from_secs(3600))), Presence::Dnd);
        assert_eq!(Presence::Invisible.announced(), Presence::Offline);
    }

    #[test]
    fn notification_rules() {
        use crate::engine::NoticeKind;
        let prefs = NotificationPrefs::default();
        assert!(should_notify(NoticeKind::Direct, &prefs, Presence::Online));
        assert!(!should_notify(NoticeKind::Direct, &prefs, Presence::Dnd));
        let mentions_only = NotificationPrefs { groups: GroupNotifications::Mentions, ..prefs.clone() };
        assert!(!should_notify(NoticeKind::Group { mention: false }, &mentions_only, Presence::Idle));
        assert!(should_notify(NoticeKind::Group { mention: true }, &mentions_only, Presence::Idle));
        let off = NotificationPrefs { enabled: false, ..prefs };
        assert!(!should_notify(NoticeKind::Other, &off, Presence::Online));
        assert!(mentions("coucou @lea et @Max", &["Léa".into(), "max".into()]));
        assert!(!mentions("coucou lea", &["Léa".into()]));
    }

    #[test]
    fn old_boolean_setting_is_read() {
        let settings: Settings = serde_json::from_str(r#"{"notifications": false}"#).unwrap();
        assert!(!settings.notifications.enabled);
        let settings: Settings = serde_json::from_str(r#"{"notifications": {"groups": "mentions"}}"#).unwrap();
        assert!(settings.notifications.enabled);
        assert_eq!(settings.notifications.groups, GroupNotifications::Mentions);
    }
}
