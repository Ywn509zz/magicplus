//! Markdown des messages : un sous-ensemble simple et sûr. L'interface Mac (AppKit) rend
//! directement les morceaux de [`parse`] ; l'interface Slint (Windows) reçoit un Markdown
//! réécrit par [`to_commonmark`], que l'analyseur de `StyledText` accepte sans erreur.
//!
//! Pris en charge : `**gras**`, `__gras__`, `*italique*`, `_italique_`, `~~barré~~`, `` `code` ``,
//! blocs de code entre ```` ``` ````, `[texte](https://…)`, liens `http(s)://…` nus, `\*` pour
//! échapper. Les retours à la ligne sont conservés.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
}

#[derive(Clone, Copy, Default)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
}

fn push(spans: &mut Vec<Span>, text: &str, style: Style, code: bool, link: Option<String>) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut() {
        if last.bold == style.bold && last.italic == style.italic && last.strike == style.strike && last.code == code && last.link == link {
            last.text.push_str(text);
            return;
        }
    }
    spans.push(Span { text: text.to_string(), bold: style.bold, italic: style.italic, strike: style.strike, code, link });
}

fn safe_link(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")).then(|| url.to_string())
}

/// Découpe un message en morceaux mis en forme.
pub fn parse(markdown: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut in_block = false;
    for (index, line) in markdown.split('\n').enumerate() {
        if index > 0 {
            push(&mut spans, "\n", Style::default(), in_block, None);
        }
        if line.trim_start().starts_with("```") {
            in_block = !in_block;
            continue;
        }
        if in_block {
            push(&mut spans, line, Style::default(), true, None);
        } else {
            parse_inline(line, &mut spans);
        }
    }
    // Retire le retour à la ligne laissé par une clôture de bloc en fin de message.
    if let Some(last) = spans.last_mut() {
        while last.text.ends_with('\n') && markdown.trim_end().ends_with("```") {
            last.text.pop();
        }
    }
    spans.retain(|span| !span.text.is_empty());
    spans
}

fn parse_inline(line: &str, spans: &mut Vec<Span>) {
    let chars: Vec<char> = line.chars().collect();
    let mut style = Style::default();
    let mut buffer = String::new();
    let mut i = 0;
    let flush = |buffer: &mut String, spans: &mut Vec<Span>, style: Style| {
        push(spans, buffer, style, false, None);
        buffer.clear();
    };
    let starts_with = |i: usize, pattern: &str| {
        let pattern: Vec<char> = pattern.chars().collect();
        chars.len() >= i + pattern.len() && chars[i..i + pattern.len()] == pattern[..]
    };
    let find_from = |from: usize, pattern: &str| -> Option<usize> {
        let pattern: Vec<char> = pattern.chars().collect();
        (from..chars.len().saturating_sub(pattern.len() - 1)).find(|&j| chars[j..j + pattern.len()] == pattern[..])
    };

    while i < chars.len() {
        let c = chars[i];
        // Échappement.
        if c == '\\' && i + 1 < chars.len() && "\\`*_~[]()".contains(chars[i + 1]) {
            buffer.push(chars[i + 1]);
            i += 2;
            continue;
        }
        // Code en ligne.
        if c == '`' {
            if let Some(end) = find_from(i + 1, "`") {
                flush(&mut buffer, spans, style);
                let code: String = chars[i + 1..end].iter().collect();
                push(spans, &code, style, true, None);
                i = end + 1;
                continue;
            }
        }
        // Lien [texte](url).
        if c == '[' {
            if let Some(close) = find_from(i + 1, "](") {
                if let Some(end) = find_from(close + 2, ")") {
                    let label: String = chars[i + 1..close].iter().collect();
                    let url: String = chars[close + 2..end].iter().collect();
                    if let Some(url) = safe_link(url.trim()) {
                        flush(&mut buffer, spans, style);
                        push(spans, &label, style, false, Some(url));
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        // Lien nu.
        if (starts_with(i, "http://") || starts_with(i, "https://")) && (i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '(') {
            let end = (i..chars.len()).find(|&j| chars[j].is_whitespace()).unwrap_or(chars.len());
            let mut url: String = chars[i..end].iter().collect();
            while url.ends_with(['.', ',', ')', '!', '?', ';', ':']) {
                url.pop();
            }
            let consumed = url.chars().count();
            flush(&mut buffer, spans, style);
            push(spans, &url, style, false, Some(url.clone()));
            i += consumed;
            continue;
        }
        // Gras, barré, italique : seulement si le marqueur se referme plus loin sur la ligne.
        let toggles: [(&str, u8); 5] = [("**", 0), ("__", 0), ("~~", 2), ("*", 1), ("_", 1)];
        let mut toggled = false;
        for (marker, kind) in toggles {
            if !starts_with(i, marker) {
                continue;
            }
            let length = marker.chars().count();
            let active = match kind {
                0 => style.bold,
                1 => style.italic,
                _ => style.strike,
            };
            // `_` au milieu d'un mot (nom_de_fichier) reste un caractère normal.
            if marker == "_" && i > 0 && chars[i - 1].is_alphanumeric() && !active {
                break;
            }
            if active || find_from(i + length, marker).is_some_and(|end| end > i + length) {
                flush(&mut buffer, spans, style);
                match kind {
                    0 => style.bold = !style.bold,
                    1 => style.italic = !style.italic,
                    _ => style.strike = !style.strike,
                }
                i += length;
                toggled = true;
            }
            break;
        }
        if toggled {
            continue;
        }
        buffer.push(c);
        i += 1;
    }
    flush(&mut buffer, spans, style);
}

/// Espace sans chasse : sépare nos balises du texte pour que l'analyseur CommonMark les reconnaisse
/// toujours comme ouvrantes et fermantes, quoi qu'il y ait autour.
const ZERO_WIDTH: char = '\u{200B}';
/// Caractère réservé par Slint pour l'interpolation : retiré du texte.
const SLINT_PLACEHOLDER: char = '\u{E541}';

fn escape_commonmark(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            SLINT_PLACEHOLDER | '\r' => {}
            c if c.is_ascii_punctuation() => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
}

fn code_commonmark(code: &str) -> String {
    let code: String = code.chars().filter(|c| *c != SLINT_PLACEHOLDER && *c != '\r').collect();
    let mut longest = 0;
    let mut run = 0;
    for c in code.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest + 1);
    let pad = if code.starts_with(['`', ' ']) || code.ends_with(['`', ' ']) { " " } else { "" };
    format!("{fence}{pad}{code}{pad}{fence}")
}

/// Réécrit un message en Markdown strict pour `StyledText` (Slint) : le texte est entièrement
/// échappé, seules nos mises en forme deviennent des balises, et chaque ligne reste une ligne
/// (titres, listes, citations ou HTML tapés par l'auteur s'affichent tels quels).
pub fn to_commonmark(markdown: &str) -> String {
    let mut lines = vec![String::new()];
    for span in parse(markdown) {
        for (index, piece) in span.text.split('\n').enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            if piece.is_empty() {
                continue;
            }
            let mut out = String::new();
            if span.code && !piece.trim().is_empty() {
                out = code_commonmark(piece);
            } else {
                escape_commonmark(piece, &mut out);
            }
            let styled = span.bold || span.italic || span.strike || span.link.is_some() || span.code;
            for (on, marker) in [(span.italic, "*"), (span.bold, "**"), (span.strike, "~~")] {
                if on {
                    out = format!("{marker}{ZERO_WIDTH}{out}{ZERO_WIDTH}{marker}");
                }
            }
            if let Some(url) = &span.link {
                let mut target = String::new();
                for c in url.chars().filter(|c| !c.is_control() && *c != SLINT_PLACEHOLDER) {
                    if matches!(c, '<' | '>' | '\\') {
                        target.push('\\');
                    }
                    target.push(c);
                }
                out = format!("[{out}](<{target}>)");
            }
            let line = lines.last_mut().expect("au moins une ligne");
            if styled {
                line.push(ZERO_WIDTH);
                line.push_str(&out);
                line.push(ZERO_WIDTH);
            } else {
                line.push_str(&out);
            }
        }
    }
    lines
        .into_iter()
        .map(|line| {
            // Une ligne vide garde sa place ; une indentation ne devient pas un bloc de code.
            if line.is_empty() {
                return ZERO_WIDTH.to_string();
            }
            let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
            let spaces: String = line[..indent].chars().map(|c| if c == '\t' { "\u{A0}\u{A0}\u{A0}\u{A0}" } else { "\u{A0}" }).collect();
            spaces + &line[indent..]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Texte brut (pour les aperçus et le presse-papiers).
pub fn plain(markdown: &str) -> String {
    parse(markdown).into_iter().map(|span| span.text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Span {
        Span { text: text.into(), ..Span::default() }
    }

    #[test]
    fn plain_text_is_kept() {
        assert_eq!(parse("bonjour à tous 👋"), vec![s("bonjour à tous 👋")]);
    }

    #[test]
    fn bold_italic_strike_code() {
        let spans = parse("un **gras** et *italique*, ~~barré~~ `code`");
        assert_eq!(spans[1], Span { text: "gras".into(), bold: true, ..Span::default() });
        assert_eq!(spans[3], Span { text: "italique".into(), italic: true, ..Span::default() });
        assert_eq!(spans[5], Span { text: "barré".into(), strike: true, ..Span::default() });
        assert_eq!(spans[7], Span { text: "code".into(), code: true, ..Span::default() });
        assert_eq!(plain("un **gras** et *italique*"), "un gras et italique");
    }

    #[test]
    fn unclosed_markers_and_snake_case_stay_literal() {
        assert_eq!(plain("2 * 3 = 6"), "2 * 3 = 6");
        assert_eq!(plain("mon_fichier_final.txt"), "mon_fichier_final.txt");
        assert_eq!(plain(r"\*pas italique\*"), "*pas italique*");
    }

    #[test]
    fn links_are_only_http_or_mail() {
        let spans = parse("voir [le site](https://exemple.fr) ou https://autre.fr.");
        assert_eq!(spans[1].link.as_deref(), Some("https://exemple.fr"));
        assert_eq!(spans[1].text, "le site");
        assert_eq!(spans[3].link.as_deref(), Some("https://autre.fr"));
        assert_eq!(plain("[piège](javascript:alert(1))"), "[piège](javascript:alert(1))");
    }

    #[test]
    fn commonmark_escapes_everything_but_our_styles() {
        let z = ZERO_WIDTH;
        assert_eq!(to_commonmark("# pas un titre"), "\\# pas un titre");
        assert_eq!(to_commonmark("1. a\n- b\n> c"), "1\\. a\n\\- b\n\\> c");
        assert_eq!(to_commonmark("**gras**"), format!("{z}**{z}gras{z}**{z}"));
        assert_eq!(to_commonmark("a\n\nb"), format!("a\n{z}\nb"));
        assert_eq!(to_commonmark("    retrait"), "\u{A0}\u{A0}\u{A0}\u{A0}retrait");
        assert_eq!(to_commonmark("`a`b`"), format!("{z}`a`{z}b\\`"));
        assert_eq!(code_commonmark("a`b"), "``a`b``");
        assert_eq!(code_commonmark("`"), "`` ` ``");
        assert_eq!(
            to_commonmark("[site](https://exemple.fr/a_b)"),
            format!("{z}[site](<https://exemple.fr/a_b>){z}")
        );
        assert_eq!(to_commonmark("<b>{}\u{E541}</b>"), "\\<b\\>\\{\\}\\<\\/b\\>");
    }

    #[test]
    fn code_blocks_keep_lines() {
        let spans = parse("avant\n```\nlet x = **1**;\n```");
        assert_eq!(spans[0], s("avant\n"));
        assert!(spans[1].code);
        assert!(spans[1].text.contains("let x = **1**;"));
    }
}
