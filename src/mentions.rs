//! Mentions « @nom » dans les messages, comme sur Discord : suggestions pendant la frappe,
//! mise en évidence dans les messages, et message surligné quand il nous mentionne.
//!
//! Le texte envoyé reste lisible partout : « @Pseudo » (le nom public de l'appareil : pseudo,
//! sinon nom de l'ordinateur), « @tous » pour tout le groupe. La détection côté réception
//! (`presence::mentions`, préférences de notification) lit le même texte.

use crate::{engine::Engine, markdown::{Block, BlockKind, Span}};

/// Couleur des mentions dans les messages (lisible sur fond clair et sombre).
pub const MENTION_COLOR: u32 = 0x5B7CFF;

/// Une suggestion : nom inséré (public) et ce qu'on affiche.
#[derive(Debug, Clone, PartialEq)]
pub struct MentionChoice {
    pub id: String,
    /// Inséré après « @ ».
    pub name: String,
    /// Nom affiché ici (surnom, pseudo…).
    pub label: String,
    /// Précision (nom de l'appareil, « tout le groupe »…).
    pub detail: String,
}

/// Longueur (en octets de `hay`) du préfixe de `hay` égal à `needle` sans tenir compte des
/// majuscules, sinon `None`.
fn prefix_ci(hay: &str, needle: &str) -> Option<usize> {
    let mut hay_chars = hay.char_indices();
    for wanted in needle.chars() {
        let (_, got) = hay_chars.next()?;
        if !got.to_lowercase().eq(wanted.to_lowercase()) {
            return None;
        }
    }
    Some(hay_chars.next().map(|(index, _)| index).unwrap_or(hay.len()))
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

/// Plages (début, fin, en octets) des mentions de `text` : « @ » en début de mot suivi d'un des
/// `names` (le plus long d'abord), de « tous » ou de « everyone ».
pub fn find_mentions(text: &str, names: &[String]) -> Vec<(usize, usize)> {
    let mut candidates: Vec<&str> = names.iter().map(|n| n.trim()).filter(|n| !n.is_empty()).collect();
    candidates.extend(["tous", "everyone"]);
    candidates.sort_by_key(|n| std::cmp::Reverse(n.chars().count()));
    let mut found = Vec::new();
    let mut previous: Option<char> = None;
    let mut skip_until = 0;
    for (index, c) in text.char_indices() {
        if index < skip_until {
            previous = Some(c);
            continue;
        }
        if c == '@' && previous.is_none_or(|p| !is_name_char(p)) {
            let rest = &text[index + 1..];
            for name in &candidates {
                if let Some(length) = prefix_ci(rest, name) {
                    let end = index + 1 + length;
                    if text[end..].chars().next().is_none_or(|next| !is_name_char(next)) {
                        found.push((index, end));
                        skip_until = end;
                        break;
                    }
                }
            }
        }
        previous = Some(c);
    }
    found
}

/// Met en évidence les mentions dans des blocs Markdown (gras, couleur des mentions).
pub fn highlight(blocks: &mut [Block], names: &[String]) {
    for block in blocks.iter_mut() {
        if matches!(block.kind, BlockKind::Code | BlockKind::Table | BlockKind::Rule) {
            continue;
        }
        let mut spans = Vec::with_capacity(block.spans.len());
        for span in block.spans.drain(..) {
            let ranges = if span.code || span.link.is_some() { Vec::new() } else { find_mentions(&span.text, names) };
            if ranges.is_empty() {
                spans.push(span);
                continue;
            }
            let mut last = 0;
            for (start, end) in ranges {
                if start > last {
                    spans.push(Span { text: span.text[last..start].to_string(), ..span.clone() });
                }
                spans.push(Span { text: span.text[start..end].to_string(), bold: true, color: Some(MENTION_COLOR), ..span.clone() });
                last = end;
            }
            if last < span.text.len() {
                spans.push(Span { text: span.text[last..].to_string(), ..span.clone() });
            }
        }
        block.spans = spans;
    }
}

/// Mention en cours de frappe : position du « @ » (octets) et texte tapé après, si le curseur
/// (`cursor`, en octets) suit « @requête » au début d'un mot.
pub fn pending_query(text: &str, cursor: usize) -> Option<(usize, String)> {
    let before = text.get(..cursor.min(text.len()))?;
    let at = before.rfind('@')?;
    if before[..at].chars().next_back().is_some_and(is_name_char) {
        return None;
    }
    let query = &before[at + 1..];
    if query.contains('\n') || query.chars().count() > 32 || query.starts_with(' ') {
        return None;
    }
    Some((at, query.to_string()))
}

/// Remplace la requête qui suit le « @ » à `at` (jusqu'à `cursor`) par « @nom ». Renvoie le
/// nouveau texte et la position du curseur (octets).
pub fn complete(text: &str, at: usize, cursor: usize, name: &str) -> (String, usize) {
    let cursor = cursor.min(text.len()).max(at);
    let inserted = format!("@{name} ");
    let mut out = String::with_capacity(text.len() + inserted.len());
    out.push_str(&text[..at]);
    out.push_str(&inserted);
    let rest = text[cursor..].trim_start_matches(' ');
    out.push_str(rest);
    (out, at + inserted.len())
}

impl Engine {
    /// Tous les noms qu'on peut mentionner dans `conversation` (pour les surligner).
    pub fn mention_names(&self, conversation: &str) -> Vec<String> {
        let inner = &self.inner;
        let mut names = Vec::new();
        for id in inner.conversation_member_ids(conversation) {
            names.push(inner.public_name(&id));
            if let Some(display) = inner.display_name(&id) {
                names.push(display);
            }
            if let Some(member) = inner.group_member(conversation, &id) {
                names.push(member.name);
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// Suggestions pour « @requête » dans `conversation` (sans nous ; « tous » dans un groupe).
    pub fn mention_choices(&self, conversation: &str, query: &str) -> Vec<MentionChoice> {
        let inner = &self.inner;
        let query = query.trim().to_lowercase();
        let me = inner.own_id();
        let matches = |text: &str| query.is_empty() || text.to_lowercase().split_whitespace().any(|word| word.starts_with(&query)) || text.to_lowercase().starts_with(&query);
        let ids = inner.conversation_member_ids(conversation);
        let group = ids.len() > 2;
        let mut choices: Vec<MentionChoice> = ids
            .into_iter()
            .filter(|id| *id != me)
            .filter_map(|id| {
                let name = inner.public_name(&id);
                let label = inner.display_name(&id).unwrap_or_else(|| name.clone());
                let device = inner.group_member(conversation, &id).map(|m| m.name).unwrap_or_default();
                (matches(&name) || matches(&label) || matches(&device)).then(|| MentionChoice {
                    detail: if device != label { device } else { String::new() },
                    id,
                    name,
                    label,
                })
            })
            .collect();
        choices.sort_by_key(|c| c.label.to_lowercase());
        if group && matches("tous") {
            choices.push(MentionChoice {
                id: String::new(),
                name: "tous".to_string(),
                label: "tous".to_string(),
                detail: "prévient tout le groupe".to_string(),
            });
        }
        choices.truncate(8);
        choices
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_mentions_with_spaces_and_case() {
        let names = vec!["Léa".to_string(), "Portable de Léa".to_string(), "Max".to_string()];
        let text = "Salut @portable de léa et @Max, mail@max.fr, @tous !";
        let found: Vec<&str> = find_mentions(text, &names).into_iter().map(|(a, b)| &text[a..b]).collect();
        assert_eq!(found, ["@portable de léa", "@Max", "@tous"]);
        assert!(find_mentions("@Maxime", &names).is_empty(), "pas une mention de Max");
    }

    #[test]
    fn highlight_splits_spans() {
        let mut blocks = crate::markdown::parse("Coucou **@Léa** et @Max `@Max`");
        highlight(&mut blocks, &["Léa".to_string(), "Max".to_string()]);
        let spans = &blocks[0].spans;
        let mentioned: Vec<&str> = spans.iter().filter(|s| s.color == Some(MENTION_COLOR)).map(|s| s.text.as_str()).collect();
        assert_eq!(mentioned, ["@Léa", "@Max"]);
        assert!(spans.iter().any(|s| s.code && s.text == "@Max" && s.color.is_none()));
    }

    #[test]
    fn pending_query_and_completion() {
        assert_eq!(pending_query("Salut @Lé", "Salut @Lé".len()), Some((6, "Lé".to_string())));
        assert_eq!(pending_query("mail@ex", 7), None);
        assert_eq!(pending_query("@", 1), Some((0, String::new())));
        assert_eq!(pending_query("@ a", 3), None);
        let (text, cursor) = complete("Salut @Lé", 6, "Salut @Lé".len(), "Léa");
        assert_eq!(text, "Salut @Léa ");
        assert_eq!(cursor, text.len());
        let (text, _) = complete("@ma suite", 0, 3, "Max");
        assert_eq!(text, "@Max suite");
    }
}
