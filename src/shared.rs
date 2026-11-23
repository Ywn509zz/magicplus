//! Code commun aux deux interfaces (AppKit sur macOS, Slint ailleurs).

use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use magicplus_core::{markdown, Engine, UpdateState, UpdateView};
use std::{path::Path, process::Command, thread};

/// Initiales affichées dans un avatar (« Mac de Léa » → « ML »).
pub fn initials(name: &str) -> String {
    let letters: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .collect();
    if letters.is_empty() { "?".into() } else { letters.to_uppercase() }
}

/// Teinte stable (0 à 1) d'un avatar, tirée de l'identifiant.
pub fn avatar_hue(seed: &str) -> f32 {
    let hash = seed.bytes().fold(7_u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    (hash % 360) as f32 / 360.0
}

/// Message composé uniquement d'émojis (affiché en grand, comme sur Discord).
pub fn jumbo(text: &str) -> bool {
    let visible: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    !visible.is_empty() && visible.len() <= 8 && visible.iter().all(|c| !c.is_ascii() && !c.is_alphanumeric())
}

/// « Aujourd'hui à 14:05 », « Hier à 09:12 », « 03/11 à 18:40 », à l'heure locale.
pub fn time_label(ms: u64) -> String {
    use chrono::{Local, TimeZone};
    let Some(when) = Local.timestamp_millis_opt(ms as i64).single() else { return String::new() };
    let today = Local::now().date_naive();
    let clock = when.format("%H:%M");
    match (today - when.date_naive()).num_days() {
        0 => format!("Aujourd'hui à {clock}"),
        1 => format!("Hier à {clock}"),
        days if (2..7).contains(&days) => format!("Il y a {days} jours à {clock}"),
        _ => format!("{} à {clock}", when.format("%d/%m/%Y")),
    }
}

/// Textes de la carte « Mises à jour » : description et bouton d'action (aucun si `None`).
pub fn update_texts(update: &UpdateView, auto: bool) -> (String, Option<&'static str>) {
    let current = &update.current;
    let latest = &update.latest;
    let notes: String = {
        let plain = markdown::plain(&update.notes);
        let plain = plain.trim();
        if plain.is_empty() {
            String::new()
        } else if plain.chars().count() > 600 {
            format!("\n\n{}…", plain.chars().take(600).collect::<String>())
        } else {
            format!("\n\n{plain}")
        }
    };
    let manual = if update.can_install {
        ""
    } else {
        "\n\nCette copie ne peut pas se remplacer seule (lancée hors de MagicPlus.app) : téléchargez la nouvelle version depuis sa page."
    };
    match &update.state {
        UpdateState::Idle if auto => (
            format!("Version {current}. MagicPlus cherche les nouvelles versions sur GitHub au démarrage, puis toutes les 6 heures."),
            None,
        ),
        UpdateState::Idle => (format!("Version {current}. Recherche automatique désactivée."), None),
        UpdateState::Checking => ("Recherche d'une nouvelle version…".to_string(), None),
        UpdateState::UpToDate => (format!("MagicPlus {current} est à jour."), None),
        UpdateState::Available => (
            format!("MagicPlus {latest} est disponible (vous avez la {current}).{notes}{manual}"),
            update.can_install.then_some("Télécharger et installer"),
        ),
        UpdateState::Downloading(progress) => {
            (format!("Téléchargement de MagicPlus {latest}… {} %", (progress * 100.0).round() as u32), None)
        }
        UpdateState::Ready => (
            format!("MagicPlus {latest} est téléchargé et vérifié. MagicPlus va redémarrer pour l'installer."),
            Some("Redémarrer et installer"),
        ),
        UpdateState::Failed(error) if !latest.is_empty() => {
            (format!("Mise à jour vers {latest} impossible : {error}"), update.can_install.then_some("Réessayer"))
        }
        UpdateState::Failed(error) => (format!("Version {current}. {error}"), None),
    }
}

/// Une mise à jour attend l'utilisateur (pastille sur « Paramètres »).
pub fn update_pending(update: &UpdateView) -> bool {
    matches!(update.state, UpdateState::Available | UpdateState::Ready)
}

/// Bouton d'action de la carte « Mises à jour » : télécharger, ou installer et relancer.
/// Vrai si l'application doit quitter (la nouvelle version démarre).
pub fn update_action(engine: &Engine) -> bool {
    match engine.update().state {
        UpdateState::Ready => match engine.install_update() {
            Ok(()) => true,
            Err(error) => {
                engine.set_status(format!("Mise à jour impossible : {error}"));
                false
            }
        },
        _ => {
            engine.download_update();
            false
        }
    }
}

/// Ouvre un fichier (avec l'application par défaut) ou un lien web.
pub fn open_path(target: &str) {
    let _ = if cfg!(target_os = "macos") {
        Command::new("open").arg(target).spawn()
    } else if cfg!(target_os = "windows") {
        Command::new("explorer").arg(target).spawn()
    } else {
        Command::new("xdg-open").arg(target).spawn()
    };
}

pub fn platform_label(platform: &str) -> String {
    match platform {
        "macos" => "macOS".to_string(),
        "windows" => "Windows".to_string(),
        "linux" => "Linux".to_string(),
        other => other.to_string(),
    }
}

/// Ouvre le dossier du fichier dans l'explorateur du système.
pub fn reveal(path: &str) {
    let path = Path::new(path);
    let _ = if cfg!(target_os = "macos") {
        Command::new("open").arg("-R").arg(path).spawn()
    } else if cfg!(target_os = "windows") {
        Command::new("explorer").arg(format!("/select,{}", path.display())).spawn()
    } else {
        Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn()
    };
}

fn main_modifier() -> (&'static str, Modifiers) {
    if cfg!(target_os = "macos") {
        ("Cmd", Modifiers::SUPER)
    } else {
        ("Ctrl", Modifiers::CONTROL)
    }
}

/// Textes affichés pour les raccourcis « copier et envoyer » et « coller le dernier reçu ».
pub fn hotkey_labels() -> (String, String) {
    let modifier = main_modifier().0;
    let alt = if cfg!(target_os = "macos") { "Option" } else { "Alt" };
    (
        format!("{modifier} + Shift + {alt} + C"),
        format!("{modifier} + Shift + {alt} + V"),
    )
}

/// Enregistre les raccourcis globaux. À appeler depuis le fil principal ; garder le résultat en vie.
pub fn setup_hotkeys(engine: &Engine) -> Option<GlobalHotKeyManager> {
    let modifiers = main_modifier().1 | Modifiers::SHIFT | Modifiers::ALT;
    let send_key = HotKey::new(Some(modifiers), Code::KeyC);
    let paste_key = HotKey::new(Some(modifiers), Code::KeyV);

    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(_) => {
            engine.set_status("Raccourcis globaux indisponibles sur ce système");
            return None;
        }
    };
    if let Err(error) = manager.register(send_key).and_then(|_| manager.register(paste_key)) {
        engine.set_status(format!("Raccourcis globaux indisponibles : {error}"));
    }

    let engine = engine.clone();
    let (send_id, paste_id) = (send_key.id(), paste_key.id());
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state() != HotKeyState::Pressed {
            return;
        }
        let engine = engine.clone();
        if event.id() == send_id {
            thread::spawn(move || engine.send_clipboard(true));
        } else if event.id() == paste_id {
            engine.paste_last_received();
        }
    }));
    Some(manager)
}

#[cfg(test)]
mod tests {
    #[test]
    fn heure_locale_stable() {
        use chrono::{Local, Offset, TimeZone};
        let ms = 1_795_000_000_000_u64;
        let first = super::time_label(ms);
        for _ in 0..2000 {
            assert_eq!(super::time_label(ms), first);
        }
        let offset = Local.timestamp_millis_opt(ms as i64).single().unwrap().offset().fix();
        println!("{first} · décalage {offset}");
    }
}
