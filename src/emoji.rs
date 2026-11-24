//! Émojis, comme sur Discord : tout le catalogue Unicode rangé par catégories, émojis favoris et
//! récents (propres à cet appareil), et émojis personnalisés (images fixes ou animées).
//!
//! Un émoji personnalisé appartient au profil de son auteur : il part avec la photo et la
//! bannière vers ses contacts (voir `profile.rs`), qui peuvent donc l'afficher. Dans un message
//! ou une réaction, il s'écrit `<:nom:id>` (`id` : 12 chiffres hexadécimaux de son SHA-256) ; la
//! zone de saisie montre `:nom:`, transformé à l'envoi.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Taille (pixels) d'un émoji personnalisé enregistré.
pub const CUSTOM_SIZE: u32 = 96;
/// Poids maximal d'un émoji personnalisé (après préparation).
pub const MAX_CUSTOM_BYTES: usize = 512 * 1024;
/// Nombre maximal d'émojis personnalisés par profil.
pub const MAX_CUSTOM: usize = 60;
pub const MAX_RECENT: usize = 36;
pub const MAX_FAVORITES: usize = 72;

// ------------------------------------------------------------------ catalogue Unicode

/// Catégorie du sélecteur : nom affiché, émoji qui la représente et ses émojis (texte, nom).
pub struct Category {
    pub name: &'static str,
    pub icon: &'static str,
    pub emojis: Vec<(&'static str, &'static str)>,
}

/// Tout le catalogue, par catégories (sans les variantes de couleur de peau). Les émojis plus
/// récents qu'Unicode 15 sont laissés de côté : les polices de Windows et macOS ne les ont pas
/// toujours.
pub fn catalogue() -> Vec<Category> {
    use emojis::Group;
    emojis::Group::iter()
        .map(|group| {
            let (name, icon) = match group {
                Group::SmileysAndEmotion => ("Émotions", "😀"),
                Group::PeopleAndBody => ("Personnes", "👋"),
                Group::AnimalsAndNature => ("Nature", "🐻"),
                Group::FoodAndDrink => ("Nourriture", "🍔"),
                Group::TravelAndPlaces => ("Voyages", "✈️"),
                Group::Activities => ("Activités", "⚽"),
                Group::Objects => ("Objets", "💡"),
                Group::Symbols => ("Symboles", "🔣"),
                Group::Flags => ("Drapeaux", "🚩"),
            };
            let emojis = group
                .emojis()
                .filter(|e| {
                    let v = e.unicode_version();
                    (v.major(), v.minor()) <= (15, 0)
                })
                .map(|e| (e.as_str(), e.name()))
                .collect();
            Category { name, icon, emojis }
        })
        .collect()
}

/// Nom (anglais, celui d'Unicode) d'un émoji, pour la recherche et l'aperçu.
pub fn name_of(emoji: &str) -> Option<&'static str> {
    emojis::get(emoji).map(|e| e.name())
}

/// Codes courts (`:joy:`…) d'un émoji, pour la recherche.
pub fn shortcodes_of(emoji: &str) -> Vec<&'static str> {
    emojis::get(emoji).map(|e| e.shortcodes().collect()).unwrap_or_default()
}

// ------------------------------------------------------------------ émojis personnalisés

/// Émoji personnalisé décrit dans un profil (ses octets suivent l'en-tête du message `profile`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EmojiInfo {
    pub name: String,
    /// 12 chiffres hexadécimaux du SHA-256 de l'image.
    pub id: String,
    /// « png » ou « gif ».
    pub ext: String,
    pub size: u64,
}

/// Émoji personnalisé connu (le nôtre ou celui d'un contact), pour l'interface.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomEmojiView {
    pub id: String,
    pub name: String,
    pub path: std::path::PathBuf,
    pub animated: bool,
    /// Le nôtre (sinon : celui d'un contact, qu'on peut utiliser mais pas supprimer).
    pub mine: bool,
    /// Nom de l'appareil qui l'a créé.
    pub owner: String,
}

impl CustomEmojiView {
    /// Jeton à placer dans un message ou une réaction.
    pub fn token(&self) -> String {
        format!("<:{}:{}>", self.name, self.id)
    }
}

/// Nom valide : 2 à 32 lettres sans accent, chiffres ou `_`.
pub fn valid_name(name: &str) -> bool {
    (2..=32).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Nom proposé à partir d'un nom de fichier (« Pepe Content!.gif » → « Pepe_Content »).
pub fn name_from_file(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut name: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    name.truncate(32);
    if name.len() < 2 {
        name = "emoji".to_string();
    }
    name
}

/// Identifiant d'une image : 12 chiffres hexadécimaux de son SHA-256.
pub fn id_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().take(6).map(|b| format!("{b:02x}")).collect()
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 12 && id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Prépare une image choisie : carrée (recadrée au centre), 96 px, en PNG ; une animation (GIF,
/// PNG animé) reste animée, en GIF.
pub fn prepare(source: &Path) -> Result<(Vec<u8>, &'static str)> {
    use image::imageops::FilterType;
    let bytes = std::fs::read(source).with_context(|| format!("« {} » est illisible", source.display()))?;
    if bytes.len() > 16 * 1024 * 1024 {
        bail!("image trop lourde (16 Mo au plus)");
    }
    if let Some(frames) = crate::animation::frames(&bytes) {
        let base_step = frames.len().div_ceil(80).max(1);
        for size in [CUSTOM_SIZE, 72, 48] {
            for step in [base_step, base_step * 2, base_step * 3] {
                let gif = crate::animation::encode_gif(&frames, size, size, step)?;
                if gif.len() <= MAX_CUSTOM_BYTES {
                    return Ok((gif, "gif"));
                }
            }
        }
        bail!("animation trop lourde pour un émoji");
    }
    let image = image::load_from_memory(&bytes).context("ce fichier n'est pas une image reconnue")?.to_rgba8();
    let square = crate::animation::cover(&image, image.width().min(image.height()).max(1), image.width().min(image.height()).max(1));
    let resized = if square.width() > CUSTOM_SIZE { image::imageops::resize(&square, CUSTOM_SIZE, CUSTOM_SIZE, FilterType::Lanczos3) } else { square };
    let mut out = std::io::Cursor::new(Vec::new());
    resized.write_to(&mut out, image::ImageFormat::Png)?;
    Ok((out.into_inner(), "png"))
}

/// Vérifie un émoji reçu (description et octets).
pub fn check_received(info: &EmojiInfo, bytes: &[u8]) -> Result<()> {
    if !valid_name(&info.name) || !valid_id(&info.id) || !matches!(info.ext.as_str(), "png" | "gif") {
        bail!("émoji invalide");
    }
    if bytes.len() > MAX_CUSTOM_BYTES || id_of(bytes) != info.id {
        return Err(anyhow!("émoji « {} » altéré", info.name));
    }
    image::load_from_memory(bytes).context("émoji illisible")?;
    Ok(())
}

// ------------------------------------------------------------------ texte

/// Morceau d'un texte : du texte, ou un émoji personnalisé (`<:nom:id>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece<'a> {
    Text(&'a str),
    Custom { name: &'a str, id: &'a str },
}

/// Découpe `text` autour des jetons `<:nom:id>`.
pub fn pieces(text: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<:") {
        let after = &rest[start + 2..];
        let parsed = after.find('>').and_then(|end| {
            let inner = &after[..end];
            let (name, id) = inner.split_once(':')?;
            (valid_name(name) && valid_id(id)).then_some((name, id, end))
        });
        match parsed {
            Some((name, id, end)) => {
                if start > 0 {
                    out.push(Piece::Text(&rest[..start]));
                }
                out.push(Piece::Custom { name, id });
                rest = &after[end + 1..];
            }
            None => {
                out.push(Piece::Text(&rest[..start + 2]));
                rest = after;
            }
        }
    }
    if !rest.is_empty() {
        out.push(Piece::Text(rest));
    }
    // Recolle les morceaux de texte voisins.
    let mut merged: Vec<Piece> = Vec::with_capacity(out.len());
    for piece in out {
        match (merged.last_mut(), piece) {
            (Some(Piece::Text(previous)), Piece::Text(next)) => {
                let start = previous.as_ptr() as usize - text.as_ptr() as usize;
                let end = next.as_ptr() as usize - text.as_ptr() as usize + next.len();
                *previous = &text[start..end];
            }
            (_, piece) => merged.push(piece),
        }
    }
    merged
}

/// Texte lisible partout (aperçus, notifications, Markdown) : `<:nom:id>` → `:nom:`.
pub fn display_text(text: &str) -> String {
    if !text.contains("<:") {
        return text.to_string();
    }
    pieces(text)
        .into_iter()
        .map(|piece| match piece {
            Piece::Text(text) => text.to_string(),
            Piece::Custom { name, .. } => format!(":{name}:"),
        })
        .collect()
}

/// À l'envoi : chaque `:nom:` d'un émoji personnalisé connu devient `<:nom:id>` (les nôtres
/// d'abord). Le reste du texte est inchangé.
pub fn encode_shortcodes(text: &str, known: &[CustomEmojiView]) -> String {
    if !text.contains(':') || known.is_empty() {
        return text.to_string();
    }
    let find = |name: &str| known.iter().filter(|e| e.name == name).max_by_key(|e| e.mine).map(|e| e.id.clone());
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(':') {
        let after = &rest[start + 1..];
        let candidate = after.find(':').map(|end| &after[..end]).filter(|name| valid_name(name));
        // Déjà un jeton `<:nom:id>` : laissé tel quel.
        let in_token = rest[..start].ends_with('<');
        match (candidate, in_token) {
            (Some(name), false) if find(name).is_some() => {
                out.push_str(&rest[..start]);
                out.push_str(&format!("<:{name}:{}>", find(name).unwrap_or_default()));
                rest = &after[name.len() + 1..];
            }
            _ => {
                out.push_str(&rest[..start + 1]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Vrai si le texte n'est fait que d'émojis (Unicode ou personnalisés), pas trop nombreux :
/// affiché en grand, comme sur Discord.
pub fn only_emojis(text: &str) -> bool {
    let mut custom = 0;
    let mut symbols = 0;
    for piece in pieces(text.trim()) {
        match piece {
            Piece::Custom { .. } => custom += 1,
            Piece::Text(text) => {
                for c in text.chars().filter(|c| !c.is_whitespace()) {
                    if c.is_ascii() || c.is_alphanumeric() {
                        return false;
                    }
                    symbols += 1;
                }
            }
        }
    }
    // Un émoji Unicode compte souvent plusieurs caractères (variantes, assemblages).
    custom + symbols > 0 && custom <= 27 && symbols <= 27 && custom * 3 + symbols <= 81
}

// ------------------------------------------------------------------ préférences

/// Récents et favoris (sur cet appareil seulement) : textes Unicode ou jetons `<:nom:id>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EmojiPrefs {
    pub recent: Vec<String>,
    pub favorites: Vec<String>,
}

impl EmojiPrefs {
    pub fn used(&mut self, emoji: &str) {
        self.recent.retain(|e| e != emoji);
        self.recent.insert(0, emoji.to_string());
        self.recent.truncate(MAX_RECENT);
    }

    /// Ajoute ou retire des favoris ; vrai s'il en fait partie ensuite.
    pub fn toggle_favorite(&mut self, emoji: &str) -> bool {
        if let Some(index) = self.favorites.iter().position(|e| e == emoji) {
            self.favorites.remove(index);
            false
        } else {
            self.favorites.push(emoji.to_string());
            if self.favorites.len() > MAX_FAVORITES {
                self.favorites.remove(0);
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(name: &str, id: &str, mine: bool) -> CustomEmojiView {
        CustomEmojiView { id: id.into(), name: name.into(), path: "x.png".into(), animated: false, mine, owner: String::new() }
    }

    #[test]
    fn pieces_split_tokens_and_keep_text() {
        let text = "salut <:pepe:0123456789ab> et <:pas un jeton> <:x:0123456789ab> fin";
        let got = pieces(text);
        assert_eq!(
            got,
            vec![
                Piece::Text("salut "),
                Piece::Custom { name: "pepe", id: "0123456789ab" },
                Piece::Text(" et <:pas un jeton> <:x:0123456789ab> fin"),
            ]
        );
        assert_eq!(display_text(text), "salut :pepe: et <:pas un jeton> <:x:0123456789ab> fin");
    }

    #[test]
    fn shortcodes_become_tokens_preferring_mine() {
        let known = [custom("pepe", "aaaaaaaaaaaa", false), custom("pepe", "bbbbbbbbbbbb", true), custom("ok_ok", "cccccccccccc", false)];
        assert_eq!(encode_shortcodes("a :pepe: b :ok_ok::inconnu: 12:30", &known), "a <:pepe:bbbbbbbbbbbb> b <:ok_ok:cccccccccccc>:inconnu: 12:30");
        // Un jeton déjà écrit n'est pas réencodé.
        assert_eq!(encode_shortcodes("<:pepe:aaaaaaaaaaaa>", &known), "<:pepe:aaaaaaaaaaaa>");
    }

    #[test]
    fn only_emojis_counts_custom_and_unicode() {
        assert!(only_emojis("<:pepe:0123456789ab> 🎉🎉"));
        assert!(only_emojis(" 👍 "));
        assert!(!only_emojis("<:pepe:0123456789ab> salut"));
        assert!(!only_emojis(""));
    }

    #[test]
    fn names_and_prefs() {
        assert_eq!(name_from_file(Path::new("C:/x/Pepe Content!!.gif")), "Pepe_Content");
        assert_eq!(name_from_file(Path::new("é.png")), "emoji");
        assert!(valid_name("ok_2") && !valid_name("a") && !valid_name("é_é"));
        let mut prefs = EmojiPrefs::default();
        prefs.used("😀");
        prefs.used("🎉");
        prefs.used("😀");
        assert_eq!(prefs.recent, ["😀", "🎉"]);
        assert!(prefs.toggle_favorite("🎉"));
        assert!(!prefs.toggle_favorite("🎉"));
        assert!(prefs.favorites.is_empty());
    }

    #[test]
    fn webp_is_refused() {
        let dir = std::env::temp_dir().join(format!("magicplus-webp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.webp");
        // En-tête WebP (RIFF … WEBPVP8 ) : reconnu, mais plus pris en charge.
        std::fs::write(&path, b"RIFF\x24\x00\x00\x00WEBPVP8 \x18\x00\x00\x00\x30\x01\x00\x9d\x01\x2a\x01\x00\x01\x00").unwrap();
        assert!(prepare(&path).is_err());
        assert!(crate::animation::frames(&std::fs::read(&path).unwrap()).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn catalogue_is_large_and_grouped() {
        let all = catalogue();
        assert_eq!(all.len(), 9);
        assert!(all.iter().map(|c| c.emojis.len()).sum::<usize>() > 1500);
        assert_eq!(name_of("😀"), Some("grinning face"));
    }
}
