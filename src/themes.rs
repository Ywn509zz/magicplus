//! Thèmes de couleur, communs aux deux interfaces : « Système », « Clair » et « Sombre » suivent
//! le style de la plateforme ; les autres imposent leur palette (fonds, textes, accent).

/// Couleurs d'un thème, en 0xRRGGBB (alpha à part quand il compte).
/// Le Mac n'utilise que l'apparence et le fond de la fenêtre.
#[cfg_attr(all(target_os = "macos", not(feature = "slint-on-mac")), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Fond de la fenêtre et des pages.
    pub window: u32,
    /// Barre de navigation (à gauche).
    pub nav: u32,
    /// Liste des conversations.
    pub list: u32,
    /// Zone des messages.
    pub chat: u32,
    /// Cartes, menus et fenêtres surgissantes.
    pub card: u32,
    pub border: u32,
    /// Fonds secondaires (champ de saisie, pastilles de réaction…).
    pub soft: u32,
    pub text: u32,
    pub subtle: u32,
    pub accent: u32,
}

pub struct ThemeDef {
    pub id: &'static str,
    pub name: &'static str,
    /// `None` : couleurs de la plateforme (clair ou sombre selon `dark`, ou selon le système).
    pub palette: Option<Palette>,
    /// Pour les thèmes de la plateforme : sombre, clair, ou `None` pour suivre le système.
    pub platform_dark: Option<bool>,
}

/// Raccourci pour écrire les palettes sur une ligne (dans l'ordre des champs de `Palette`).
#[allow(clippy::too_many_arguments)]
const fn p(dark: bool, window: u32, nav: u32, list: u32, chat: u32, card: u32, border: u32, soft: u32, text: u32, subtle: u32, accent: u32) -> Option<Palette> {
    Some(Palette { dark, window, nav, list, chat, card, border, soft, text, subtle, accent })
}

pub const THEMES: &[ThemeDef] = &[
    ThemeDef { id: "systeme", name: "Système", palette: None, platform_dark: None },
    ThemeDef { id: "clair", name: "Clair", palette: None, platform_dark: Some(false) },
    ThemeDef { id: "sombre", name: "Sombre", palette: None, platform_dark: Some(true) },
    ThemeDef {
        id: "graphite",
        name: "Graphite",
        palette: p(true, 0x1e1f22, 0x1e1f22, 0x2b2d31, 0x313338, 0x2b2d31, 0x3f4147, 0x383a40, 0xf2f3f5, 0xb5bac1, 0x8c95f7),
        platform_dark: None,
    },
    ThemeDef {
        id: "onyx",
        name: "Onyx",
        palette: p(true, 0x000000, 0x000000, 0x0b0b0c, 0x050505, 0x121214, 0x26262a, 0x1a1a1d, 0xffffff, 0x9b9ba3, 0x8ab4ff),
        platform_dark: None,
    },
    ThemeDef {
        id: "nuit",
        name: "Nuit bleue",
        palette: p(true, 0x080a17, 0x080a17, 0x0b0d1d, 0x0e1126, 0x151935, 0x262b50, 0x1c2145, 0xe8eaff, 0x9ea3c9, 0x8c9eff),
        platform_dark: None,
    },
    ThemeDef {
        id: "amethyste",
        name: "Améthyste",
        palette: p(true, 0x140d1b, 0x140d1b, 0x191122, 0x1e1528, 0x271c34, 0x3b2b4f, 0x2f2241, 0xf3eaff, 0xb9a7cd, 0xc58cff),
        platform_dark: None,
    },
    ThemeDef {
        id: "foret",
        name: "Forêt",
        palette: p(true, 0x0c1410, 0x0c1410, 0x101a15, 0x14201a, 0x1a2a22, 0x2a3f33, 0x213429, 0xe6f5ec, 0x9dbaa8, 0x5fd39a),
        platform_dark: None,
    },
    ThemeDef {
        id: "braise",
        name: "Braise",
        palette: p(true, 0x180e11, 0x180e11, 0x1e1215, 0x24161a, 0x2e1c21, 0x462a31, 0x3a2329, 0xffece8, 0xc9a7a3, 0xff9e7a),
        platform_dark: None,
    },
    ThemeDef {
        id: "ocean",
        name: "Océan",
        palette: p(true, 0x071a22, 0x071a22, 0x0a2029, 0x0d2630, 0x12303c, 0x1f4656, 0x173845, 0xe3f6fb, 0x93bcc8, 0x4fd1e8),
        platform_dark: None,
    },
    ThemeDef {
        id: "sakura",
        name: "Sakura",
        palette: p(false, 0xf9e3ea, 0xf9e3ea, 0xfdeef2, 0xfff7f9, 0xffffff, 0xf0d0da, 0xfbe4eb, 0x3a1f29, 0x8a6672, 0xd6336c),
        platform_dark: None,
    },
    ThemeDef {
        id: "menthe",
        name: "Menthe",
        palette: p(false, 0xdff2e8, 0xdff2e8, 0xe9f7f0, 0xf6fffb, 0xffffff, 0xcbe6d8, 0xe1f3ea, 0x15322a, 0x557a6c, 0x0f8a5f),
        platform_dark: None,
    },
    ThemeDef {
        id: "sable",
        name: "Sable",
        palette: p(false, 0xf1e9dc, 0xf1e9dc, 0xf7f0e5, 0xfdf9f2, 0xffffff, 0xe3d6c2, 0xf0e6d6, 0x352a1c, 0x7d6a52, 0xb5651d),
        platform_dark: None,
    },
];

/// Thème enregistré (identifiant), ou celui qui correspond à l'ancien réglage clair/sombre.
pub fn current(theme: Option<&str>, dark_mode: Option<bool>) -> &'static ThemeDef {
    let id = theme.unwrap_or(match dark_mode {
        Some(true) => "sombre",
        Some(false) => "clair",
        None => "systeme",
    });
    THEMES.iter().find(|t| t.id == id).unwrap_or(&THEMES[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_lookup_falls_back() {
        let mut ids: Vec<_> = THEMES.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), THEMES.len());
        assert_eq!(current(Some("onyx"), None).id, "onyx");
        assert_eq!(current(None, Some(true)).id, "sombre");
        assert_eq!(current(Some("inconnu"), Some(true)).id, "systeme");
    }
}
