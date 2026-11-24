//! Émojis pour Slint : lignes du sélecteur (favoris, récents, personnalisés, catégories,
//! recherche) et affichage des émojis personnalisés dans les messages et les réactions.

use super::{images, EmojiCell, EmojiRow, InlinePiece};
use magicplus_core::{
    emoji::{self, Category, Piece},
    CustomEmojiView, Engine,
};
use slint::{ModelRc, SharedString, VecModel};
use std::{collections::HashMap, sync::OnceLock};

/// Cases par ligne du sélecteur.
const PER_ROW: usize = 9;
/// Taille (pixels) des images d'émojis préparées pour l'affichage.
const IMAGE_SIZE: u32 = 64;

/// Émojis personnalisés connus, par identifiant.
pub type Customs = HashMap<String, CustomEmojiView>;

pub fn customs(engine: &Engine) -> Customs {
    engine.custom_emojis().into_iter().map(|e| (e.id.clone(), e)).collect()
}

fn catalogue() -> &'static [Category] {
    static CATALOGUE: OnceLock<Vec<Category>> = OnceLock::new();
    CATALOGUE.get_or_init(emoji::catalogue)
}

/// Onglets : favoris et récents, personnalisés, puis les catégories Unicode.
pub fn tabs() -> Vec<SharedString> {
    let mut tabs: Vec<SharedString> = vec!["⭐".into(), "🖼️".into()];
    tabs.extend(catalogue().iter().map(|c| SharedString::from(c.icon)));
    tabs
}

fn frames_model(frames: Vec<slint::Image>) -> ModelRc<slint::Image> {
    ModelRc::new(VecModel::from(frames))
}

fn unicode_cell(text: &str, favorite: bool) -> EmojiCell {
    let shortcode = emoji::shortcodes_of(text).first().map(|s| format!("  :{s}:")).unwrap_or_default();
    EmojiCell {
        text: text.into(),
        token: text.into(),
        name: format!("{}{shortcode}", emoji::name_of(text).unwrap_or(text)).into(),
        favorite,
        ..EmojiCell::default()
    }
}

/// Case d'un émoji personnalisé (`None` si son image est illisible).
pub fn custom_cell(custom: &CustomEmojiView, favorite: bool) -> Option<EmojiCell> {
    let picture = images::picture(&custom.path, IMAGE_SIZE)?;
    let owner = if custom.mine || custom.owner.is_empty() { String::new() } else { format!("  ·  de {}", custom.owner) };
    Some(EmojiCell {
        text: SharedString::default(),
        image: picture.still,
        frames: frames_model(picture.frames),
        frame_ms: picture.frame_ms,
        token: custom.token().into(),
        name: format!(":{}:{owner}", custom.name).into(),
        favorite,
        mine: custom.mine,
    })
}

/// Case d'un jeton enregistré (favori ou récent) : Unicode, ou personnalisé encore connu.
fn token_cell(token: &str, customs: &Customs, favorite: bool) -> Option<EmojiCell> {
    match emoji::pieces(token).as_slice() {
        [Piece::Custom { id, .. }] => customs.get(*id).and_then(|c| custom_cell(c, favorite)),
        _ if !token.trim().is_empty() && !token.contains('<') => Some(unicode_cell(token, favorite)),
        _ => None,
    }
}

fn push_section(rows: &mut Vec<EmojiRow>, header: &str, cells: Vec<EmojiCell>) {
    rows.push(EmojiRow { header: header.into(), cells: ModelRc::default() });
    for chunk in cells.chunks(PER_ROW) {
        rows.push(EmojiRow { header: SharedString::default(), cells: ModelRc::new(VecModel::from(chunk.to_vec())) });
    }
}

/// Lignes du sélecteur pour l'onglet `tab` ou la recherche `search` (prioritaire).
pub fn rows(engine: &Engine, tab: usize, search: &str) -> Vec<EmojiRow> {
    let prefs = engine.emoji_prefs();
    let customs = customs(engine);
    let is_favorite = |token: &str| prefs.favorites.iter().any(|f| f == token);
    let mut rows = Vec::new();
    let search = search.trim().to_lowercase();

    if !search.is_empty() {
        let mut custom: Vec<&CustomEmojiView> = customs.values().filter(|c| c.name.to_lowercase().contains(&search)).collect();
        custom.sort_by(|a, b| b.mine.cmp(&a.mine).then(a.name.cmp(&b.name)));
        let custom_cells: Vec<EmojiCell> = custom.iter().filter_map(|c| custom_cell(c, is_favorite(&c.token()))).collect();
        if !custom_cells.is_empty() {
            push_section(&mut rows, "ÉMOJIS PERSONNALISÉS", custom_cells);
        }
        let unicode: Vec<EmojiCell> = catalogue()
            .iter()
            .flat_map(|c| c.emojis.iter())
            .filter(|(text, name)| name.contains(&search) || emoji::shortcodes_of(text).iter().any(|s| s.contains(&search)))
            .take(PER_ROW * 30)
            .map(|(text, _)| unicode_cell(text, is_favorite(text)))
            .collect();
        if !unicode.is_empty() {
            push_section(&mut rows, "RÉSULTATS", unicode);
        }
        if rows.is_empty() {
            rows.push(EmojiRow { header: "Aucun émoji ne correspond.".into(), cells: ModelRc::default() });
        }
        return rows;
    }

    match tab {
        0 => {
            let favorites: Vec<EmojiCell> = prefs.favorites.iter().filter_map(|t| token_cell(t, &customs, true)).collect();
            let recent: Vec<EmojiCell> = prefs.recent.iter().filter_map(|t| token_cell(t, &customs, is_favorite(t))).collect();
            if favorites.is_empty() {
                rows.push(EmojiRow { header: "FAVORIS — clic droit sur un émoji (ou ☆) pour l'ajouter".into(), cells: ModelRc::default() });
            } else {
                push_section(&mut rows, "FAVORIS", favorites);
            }
            if !recent.is_empty() {
                push_section(&mut rows, "UTILISÉS RÉCEMMENT", recent);
            }
        }
        1 => {
            let mut all: Vec<&CustomEmojiView> = customs.values().collect();
            all.sort_by(|a, b| a.name.cmp(&b.name));
            let mine: Vec<EmojiCell> = all.iter().filter(|c| c.mine).filter_map(|c| custom_cell(c, is_favorite(&c.token()))).collect();
            let others: Vec<EmojiCell> = all.iter().filter(|c| !c.mine).filter_map(|c| custom_cell(c, is_favorite(&c.token()))).collect();
            if mine.is_empty() {
                rows.push(EmojiRow {
                    header: "MES ÉMOJIS — bouton + : une image ou un GIF animé, envoyé à vos contacts avec votre profil".into(),
                    cells: ModelRc::default(),
                });
            } else {
                push_section(&mut rows, &format!("MES ÉMOJIS ({}/{})", mine.len(), emoji::MAX_CUSTOM), mine);
            }
            if !others.is_empty() {
                push_section(&mut rows, "DE VOS CONTACTS", others);
            }
        }
        _ => {
            if let Some(category) = catalogue().get(tab - 2) {
                let cells = category.emojis.iter().map(|(text, _)| unicode_cell(text, is_favorite(text))).collect();
                push_section(&mut rows, &category.name.to_uppercase(), cells);
            }
        }
    }
    rows
}

/// Message fait seulement d'émojis, dont au moins un personnalisé : ses émojis en grand.
pub fn big_emojis(text: &str, customs: &Customs) -> Vec<EmojiCell> {
    if !text.contains("<:") || !emoji::only_emojis(text) {
        return Vec::new();
    }
    let mut cells = Vec::new();
    for piece in emoji::pieces(text.trim()) {
        match piece {
            Piece::Custom { id, name } => match customs.get(id).and_then(|c| custom_cell(c, false)) {
                Some(cell) => cells.push(cell),
                None => cells.push(EmojiCell { text: format!(":{name}:").into(), ..EmojiCell::default() }),
            },
            Piece::Text(text) => {
                for word in text.split_whitespace() {
                    cells.push(EmojiCell { text: word.into(), ..EmojiCell::default() });
                }
            }
        }
    }
    cells
}

/// Texte simple (un seul paragraphe sans mise en forme) avec émojis personnalisés : ses
/// morceaux, pour l'afficher avec les images dans la ligne. Vide sinon.
pub fn inline_pieces(text: &str, customs: &Customs) -> Vec<InlinePiece> {
    if !text.contains("<:") || emoji::only_emojis(text) {
        return Vec::new();
    }
    let pieces = emoji::pieces(text.trim());
    if !pieces.iter().any(|p| matches!(p, Piece::Custom { id, .. } if customs.contains_key(*id))) {
        return Vec::new();
    }
    // Seulement du texte brut : sinon le Markdown (avec « :nom: ») reste plus lisible.
    let plain_text = pieces.iter().all(|p| match p {
        Piece::Text(t) => !t.contains('\n') && !t.chars().any(|c| matches!(c, '*' | '_' | '`' | '~' | '[' | '#' | '>' | '|')),
        Piece::Custom { .. } => true,
    });
    if !plain_text || text.chars().count() > 160 {
        return Vec::new();
    }
    pieces
        .into_iter()
        .map(|piece| match piece {
            Piece::Text(text) => InlinePiece { text: text.into(), ..InlinePiece::default() },
            Piece::Custom { id, name } => match customs.get(id).and_then(|c| images::picture(&c.path, IMAGE_SIZE)) {
                Some(picture) => InlinePiece {
                    text: SharedString::default(),
                    is_image: true,
                    image: picture.still,
                    frames: frames_model(picture.frames),
                    frame_ms: picture.frame_ms,
                },
                None => InlinePiece { text: format!(":{name}:").into(), ..InlinePiece::default() },
            },
        })
        .collect()
}

/// Image d'une réaction avec un émoji personnalisé (`None` pour un émoji Unicode).
pub fn reaction_picture(token: &str, customs: &Customs) -> Option<images::Picture> {
    match emoji::pieces(token).as_slice() {
        [Piece::Custom { id, .. }] => customs.get(*id).and_then(|c| images::picture(&c.path, IMAGE_SIZE)),
        _ => None,
    }
}
