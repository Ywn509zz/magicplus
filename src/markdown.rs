//! Markdown des messages et des canevas : CommonMark complet + extensions GitHub (tableaux,
//! cases à cocher, barré, encadrés `> [!NOTE]`) + habitudes de Discord (`__souligné__`, `-# petit
//! texte`, retours à la ligne conservés) + couleurs (`<font color="rouge">…</font>`).
//!
//! [`parse`] produit une liste **plate** de blocs (paragraphe, titre, code, tableau…) avec leur
//! profondeur de liste et de citation : les interfaces (AppKit sur Mac, Slint sous Windows)
//! n'ont qu'à les empiler. Le HTML n'est jamais interprété (sauf couleur et soulignement), les liens
//! ne sont gardés que s'ils sont en `http`, `https` ou `mailto`, et aucune image n'est chargée
//! depuis Internet.

use crate::highlight::{highlight, Token};
use pulldown_cmark::{Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
    /// Couleur du texte (0xRRGGBB).
    pub color: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Text,
    /// Titre de niveau 1 à 3 (`####` et plus deviennent 3).
    Heading(u8),
    /// Petit texte gris (`-# …`, comme sur Discord).
    Subtext,
    Code,
    Table,
    Rule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeBlock {
    pub language: String,
    pub text: String,
    /// Le code découpé en lignes de morceaux colorés.
    pub lines: Vec<Vec<Token>>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    pub align: Vec<Align>,
    pub header: Vec<Vec<Span>>,
    pub rows: Vec<Vec<Vec<Span>>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    pub spans: Vec<Span>,
    /// Puce ou numéro de liste (« • », « 2. », « ☐ », « ☑ »), vide sinon.
    pub marker: String,
    /// Nombre de listes englobantes.
    pub indent: u8,
    /// Nombre de citations englobantes.
    pub quote: u8,
    pub code: Option<CodeBlock>,
    pub table: Option<Table>,
}

impl Block {
    fn new(kind: BlockKind, indent: u8, quote: u8, marker: String) -> Self {
        Self { kind, spans: Vec::new(), marker, indent, quote, code: None, table: None }
    }

    /// Texte brut du bloc.
    pub fn plain(&self) -> String {
        match self.kind {
            BlockKind::Code => self.code.as_ref().map(|c| c.text.clone()).unwrap_or_default(),
            BlockKind::Table => {
                let table = self.table.clone().unwrap_or_default();
                std::iter::once(&table.header)
                    .chain(table.rows.iter())
                    .map(|row| row.iter().map(|cell| spans_text(cell)).collect::<Vec<_>>().join(" | "))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            BlockKind::Rule => "—".to_string(),
            _ => {
                let text = spans_text(&self.spans);
                if self.marker.is_empty() { text } else { format!("{} {text}", self.marker) }
            }
        }
    }
}

fn spans_text(spans: &[Span]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}

// ------------------------------------------------------------------ couleurs

/// Couleurs reconnues dans `<font color="…">` : noms français ou anglais, ou `#rgb` / `#rrggbb`.
pub fn parse_color(value: &str) -> Option<u32> {
    let value = value.trim().trim_matches(['"', '\'']).trim().to_lowercase();
    let named = match value.as_str() {
        "rouge" | "red" => Some(0xE5484D),
        "orange" => Some(0xF76B15),
        "jaune" | "yellow" => Some(0xE2A300),
        "vert" | "green" => Some(0x30A46C),
        "cyan" | "turquoise" => Some(0x12A5B5),
        "bleu" | "blue" => Some(0x3E7BFA),
        "violet" | "purple" | "mauve" => Some(0x8E4EC6),
        "rose" | "pink" => Some(0xD6409F),
        "marron" | "brown" => Some(0xA0714F),
        "gris" | "gray" | "grey" => Some(0x8B8D98),
        "noir" | "black" => Some(0x1C1C1C),
        "blanc" | "white" => Some(0xF5F5F5),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let hex = value.strip_prefix('#')?;
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok(),
        3 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xF, (v >> 4) & 0xF, v & 0xF);
            Some((r * 17) << 16 | (g * 17) << 8 | (b * 17))
        }
        _ => None,
    }
}

fn safe_link(url: &str) -> Option<String> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")).then(|| url.to_string())
}

// ------------------------------------------------------------------ analyse

#[derive(Default, Clone)]
struct Style {
    bold: u32,
    italic: u32,
    underline: u32,
    strike: u32,
    links: Vec<Option<String>>,
    colors: Vec<u32>,
}

/// Une liste en cours : numéro du prochain élément (`None` : liste à puces).
struct ListState {
    next: Option<u64>,
}

struct TableBuilder {
    table: Table,
    in_head: bool,
    row: Vec<Vec<Span>>,
    cell: Option<Vec<Span>>,
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    current: Option<Block>,
    quote: u8,
    lists: Vec<ListState>,
    pending_marker: Option<String>,
    style: Style,
    /// Styles ouverts, pour savoir quoi refermer (Strong peut être un soulignement `__`).
    strong_is_underline: Vec<bool>,
    code: Option<(String, String)>,
    html_block: bool,
    table: Option<TableBuilder>,
    image_depth: u32,
}

impl Builder {
    fn indent(&self) -> u8 {
        self.lists.len().min(8) as u8
    }

    fn start_block(&mut self, kind: BlockKind) {
        self.flush();
        let marker = self.pending_marker.take().unwrap_or_default();
        self.current = Some(Block::new(kind, self.indent(), self.quote, marker));
    }

    fn flush(&mut self) {
        if let Some(mut block) = self.current.take() {
            // `-# petit texte` (Discord).
            if block.kind == BlockKind::Text {
                if let Some(first) = block.spans.first_mut() {
                    if let Some(rest) = first.text.strip_prefix("-# ") {
                        first.text = rest.to_string();
                        block.kind = BlockKind::Subtext;
                    }
                }
            }
            // Retours à la ligne en fin de bloc : inutiles.
            while block.spans.last().is_some_and(|s| s.text.ends_with('\n')) {
                let last = block.spans.last_mut().expect("dernier morceau");
                last.text.pop();
                if last.text.is_empty() {
                    block.spans.pop();
                }
            }
            if !block.spans.is_empty() || !block.marker.is_empty() {
                self.blocks.push(block);
            }
        }
    }

    fn push_block(&mut self, mut block: Block) {
        self.flush();
        if let Some(marker) = self.pending_marker.take() {
            block.marker = marker;
        }
        self.blocks.push(block);
    }

    fn span(&self, text: &str, code: bool, link: Option<String>) -> Span {
        Span {
            text: text.to_string(),
            bold: self.style.bold > 0,
            italic: self.style.italic > 0,
            underline: self.style.underline > 0,
            strike: self.style.strike > 0,
            code,
            link,
            color: self.style.colors.last().copied(),
        }
    }

    /// Ajoute du texte à l'endroit courant (cellule de tableau ou bloc).
    fn push_span(&mut self, span: Span) {
        if span.text.is_empty() {
            return;
        }
        let target: &mut Vec<Span> = if let Some(cell) = self.table.as_mut().and_then(|t| t.cell.as_mut()) {
            cell
        } else {
            if self.current.is_none() {
                let marker = self.pending_marker.take().unwrap_or_default();
                self.current = Some(Block::new(BlockKind::Text, self.indent(), self.quote, marker));
            }
            &mut self.current.as_mut().expect("bloc courant").spans
        };
        if let Some(last) = target.last_mut() {
            let same_style = last.bold == span.bold
                && last.italic == span.italic
                && last.underline == span.underline
                && last.strike == span.strike
                && last.code == span.code
                && last.link == span.link
                && last.color == span.color;
            if same_style {
                last.text.push_str(&span.text);
                return;
            }
        }
        target.push(span);
    }

    /// Texte normal : les adresses web nues deviennent des liens.
    fn push_text(&mut self, text: &str) {
        if self.image_depth > 0 {
            let span = self.span(text, false, self.style.links.last().cloned().flatten());
            self.push_span(span);
            return;
        }
        let link = self.style.links.last().cloned().flatten();
        if link.is_some() || self.style.links.last().is_some() {
            let span = self.span(text, false, link);
            self.push_span(span);
            return;
        }
        let mut rest = text;
        while !rest.is_empty() {
            let found = ["https://", "http://"].iter().filter_map(|p| rest.find(p)).min();
            let Some(start) = found.filter(|&i| i == 0 || rest[..i].ends_with(|c: char| c.is_whitespace() || c == '(')) else {
                let span = self.span(rest, false, None);
                self.push_span(span);
                break;
            };
            if start > 0 {
                let span = self.span(&rest[..start], false, None);
                self.push_span(span);
            }
            let after = &rest[start..];
            let end = after.find(char::is_whitespace).unwrap_or(after.len());
            let mut url = &after[..end];
            while url.ends_with(['.', ',', ')', '!', '?', ';', ':']) {
                url = &url[..url.len() - 1];
            }
            let span = self.span(url, false, safe_link(url));
            self.push_span(span);
            rest = &after[url.len()..];
        }
    }

    fn inline_html(&mut self, html: &str) {
        let tag = html.trim().to_ascii_lowercase();
        let compact: String = tag.chars().filter(|c| !c.is_whitespace()).collect();
        if compact == "<u>" {
            self.style.underline += 1;
        } else if compact == "</u>" {
            self.style.underline = self.style.underline.saturating_sub(1);
        } else if compact == "</font>" || compact == "</span>" {
            self.style.colors.pop();
        } else if compact == "<br>" || compact == "<br/>" {
            let span = self.span("\n", false, None);
            self.push_span(span);
        } else if let Some(color) = html_color(html) {
            self.style.colors.push(color);
        } else {
            self.push_text(html);
        }
    }
}

/// `<font color="rouge">` ou `<span style="color: #e33">`.
fn html_color(html: &str) -> Option<u32> {
    let lower = html.to_ascii_lowercase();
    let value = if lower.starts_with("<font") {
        let at = lower.find("color")?;
        let rest = &html[at + 5..];
        rest.trim_start().strip_prefix('=')?
    } else if lower.starts_with("<span") {
        let at = lower.find("color")?;
        let rest = &html[at + 5..];
        rest.trim_start().strip_prefix(':')?
    } else {
        return None;
    };
    let value = value.trim_start();
    let value: String = if let Some(quoted) = value.strip_prefix(['"', '\'']) {
        quoted.chars().take_while(|c| *c != '"' && *c != '\'').collect()
    } else {
        value.chars().take_while(|c| !c.is_whitespace() && *c != '>' && *c != ';').collect()
    };
    parse_color(&value)
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        _ => 3,
    }
}

/// Découpe un texte Markdown en blocs prêts à afficher.
pub fn parse(markdown: &str) -> Vec<Block> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_GFM;
    let mut b = Builder::default();
    for (event, range) in Parser::new_ext(markdown, options).into_offset_iter() {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    if b.table.is_none() {
                        b.start_block(BlockKind::Text);
                    }
                }
                Tag::Heading { level, .. } => b.start_block(BlockKind::Heading(heading_level(level))),
                Tag::BlockQuote(kind) => {
                    b.flush();
                    b.quote = b.quote.saturating_add(1);
                    if let Some(kind) = kind {
                        let (label, color) = match kind {
                            BlockQuoteKind::Note => ("ℹ️ Remarque", 0x3E7BFA),
                            BlockQuoteKind::Tip => ("💡 Astuce", 0x30A46C),
                            BlockQuoteKind::Important => ("❗ Important", 0x8E4EC6),
                            BlockQuoteKind::Warning => ("⚠️ Attention", 0xE2A300),
                            BlockQuoteKind::Caution => ("⛔ Prudence", 0xE5484D),
                        };
                        let mut block = Block::new(BlockKind::Text, b.indent(), b.quote, String::new());
                        block.spans.push(Span { text: label.into(), bold: true, color: Some(color), ..Span::default() });
                        b.push_block(block);
                    }
                }
                Tag::CodeBlock(kind) => {
                    b.flush();
                    let language = match kind {
                        CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or("").to_string(),
                        CodeBlockKind::Indented => String::new(),
                    };
                    b.code = Some((language, String::new()));
                }
                Tag::HtmlBlock => {
                    b.start_block(BlockKind::Text);
                    b.html_block = true;
                }
                Tag::List(start) => {
                    b.flush();
                    b.lists.push(ListState { next: start });
                }
                Tag::Item => {
                    b.flush();
                    let marker = match b.lists.last_mut() {
                        Some(ListState { next: Some(number) }) => {
                            let marker = format!("{number}.");
                            *number += 1;
                            marker
                        }
                        _ => ["•", "◦", "▪"][(b.lists.len().max(1) - 1) % 3].to_string(),
                    };
                    b.pending_marker = Some(marker);
                }
                Tag::Table(align) => {
                    b.flush();
                    let align = align
                        .iter()
                        .map(|a| match a {
                            Alignment::Center => Align::Center,
                            Alignment::Right => Align::Right,
                            _ => Align::Left,
                        })
                        .collect();
                    b.table = Some(TableBuilder { table: Table { align, ..Table::default() }, in_head: false, row: Vec::new(), cell: None });
                }
                Tag::TableHead => {
                    if let Some(table) = b.table.as_mut() {
                        table.in_head = true;
                        table.row.clear();
                    }
                }
                Tag::TableRow => {
                    if let Some(table) = b.table.as_mut() {
                        table.row.clear();
                    }
                }
                Tag::TableCell => {
                    if let Some(table) = b.table.as_mut() {
                        table.cell = Some(Vec::new());
                    }
                }
                Tag::Emphasis => b.style.italic += 1,
                Tag::Strong => {
                    let underline = markdown.get(range.start..).is_some_and(|s| s.starts_with("__"));
                    if underline {
                        b.style.underline += 1;
                    } else {
                        b.style.bold += 1;
                    }
                    b.strong_is_underline.push(underline);
                }
                Tag::Strikethrough | Tag::Subscript => b.style.strike += 1,
                Tag::Link { dest_url, .. } => b.style.links.push(safe_link(&dest_url)),
                Tag::Image { dest_url, .. } => {
                    // Pas d'image venue d'Internet : le texte, avec le lien s'il est sûr.
                    b.image_depth += 1;
                    b.style.links.push(safe_link(&dest_url));
                    let span = b.span("🖼 ", false, b.style.links.last().cloned().flatten());
                    b.push_span(span);
                }
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph | TagEnd::Heading(_) => {
                    if b.table.is_none() {
                        b.flush();
                    }
                }
                TagEnd::BlockQuote(_) => {
                    b.flush();
                    b.quote = b.quote.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    if let Some((language, mut text)) = b.code.take() {
                        if text.ends_with('\n') {
                            text.pop();
                        }
                        let text = text.replace('\t', "    ");
                        let lines = highlight(&language, &text);
                        let mut block = Block::new(BlockKind::Code, b.indent(), b.quote, String::new());
                        block.code = Some(CodeBlock { language, text, lines });
                        b.push_block(block);
                    }
                }
                TagEnd::HtmlBlock => {
                    b.html_block = false;
                    b.flush();
                }
                TagEnd::List(_) => {
                    b.flush();
                    b.lists.pop();
                    b.pending_marker = None;
                }
                TagEnd::Item => {
                    b.flush();
                    // Élément vide : garder sa puce.
                    if let Some(marker) = b.pending_marker.take() {
                        b.blocks.push(Block::new(BlockKind::Text, b.indent(), b.quote, marker));
                    }
                }
                TagEnd::TableCell => {
                    if let Some(table) = b.table.as_mut() {
                        let cell = table.cell.take().unwrap_or_default();
                        table.row.push(cell);
                    }
                }
                TagEnd::TableHead => {
                    if let Some(table) = b.table.as_mut() {
                        table.table.header = std::mem::take(&mut table.row);
                        table.in_head = false;
                    }
                }
                TagEnd::TableRow => {
                    if let Some(table) = b.table.as_mut() {
                        let row = std::mem::take(&mut table.row);
                        table.table.rows.push(row);
                    }
                }
                TagEnd::Table => {
                    if let Some(table) = b.table.take() {
                        let mut block = Block::new(BlockKind::Table, b.indent(), b.quote, String::new());
                        block.table = Some(table.table);
                        b.push_block(block);
                    }
                }
                TagEnd::Emphasis => b.style.italic = b.style.italic.saturating_sub(1),
                TagEnd::Strong => {
                    if b.strong_is_underline.pop().unwrap_or(false) {
                        b.style.underline = b.style.underline.saturating_sub(1);
                    } else {
                        b.style.bold = b.style.bold.saturating_sub(1);
                    }
                }
                TagEnd::Strikethrough | TagEnd::Subscript => b.style.strike = b.style.strike.saturating_sub(1),
                TagEnd::Link => {
                    b.style.links.pop();
                }
                TagEnd::Image => {
                    b.style.links.pop();
                    b.image_depth = b.image_depth.saturating_sub(1);
                }
                _ => {}
            },
            Event::Text(text) => {
                if let Some((_, code)) = b.code.as_mut() {
                    code.push_str(&text);
                } else {
                    b.push_text(&text);
                }
            }
            Event::Code(code) => {
                let link = b.style.links.last().cloned().flatten();
                let span = b.span(&code, true, link);
                b.push_span(span);
            }
            Event::Html(html) => {
                if b.html_block {
                    let span = b.span(&html, false, None);
                    b.push_span(span);
                } else {
                    b.push_text(&html);
                }
            }
            Event::InlineHtml(html) => b.inline_html(&html),
            Event::SoftBreak | Event::HardBreak => {
                let span = b.span("\n", false, None);
                b.push_span(span);
            }
            Event::Rule => {
                let block = Block::new(BlockKind::Rule, b.indent(), b.quote, String::new());
                b.push_block(block);
            }
            Event::TaskListMarker(checked) => {
                let marker = if checked { "☑" } else { "☐" }.to_string();
                match b.current.as_mut() {
                    Some(block) => block.marker = marker,
                    None => b.pending_marker = Some(marker),
                }
            }
            Event::InlineMath(text) | Event::DisplayMath(text) | Event::FootnoteReference(text) => b.push_text(&text),
        }
    }
    b.flush();
    b.blocks
}

/// Texte brut (aperçus, copie, extraits de réponse).
pub fn plain(markdown: &str) -> String {
    parse(markdown).iter().map(Block::plain).collect::<Vec<_>>().join("\n")
}

// ------------------------------------------------------------------ pour Slint

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

/// Réécrit des morceaux mis en forme en Markdown strict pour `StyledText` (Slint) : le texte est
/// entièrement échappé, seules nos mises en forme deviennent des balises, chaque `\n` reste un
/// retour à la ligne. Les couleurs et le soulignement utilisent `<font color>` et `<u>`.
pub fn spans_to_commonmark(spans: &[Span]) -> String {
    let mut lines = vec![String::new()];
    for span in spans {
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
            let styled = span.bold || span.italic || span.strike || span.underline || span.link.is_some() || span.code || span.color.is_some();
            for (on, marker) in [(span.italic, "*"), (span.bold, "**"), (span.strike, "~~")] {
                if on {
                    out = format!("{marker}{ZERO_WIDTH}{out}{ZERO_WIDTH}{marker}");
                }
            }
            if span.underline {
                out = format!("<u>{out}</u>");
            }
            if let Some(color) = span.color {
                out = format!("<font color=\"#{color:06x}\">{out}</font>");
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

/// Lignes de code colorées, en Markdown strict pour `StyledText` (une ligne par ligne de code).
pub fn code_to_commonmark(lines: &[Vec<Token>]) -> String {
    let spans: Vec<Span> = lines
        .iter()
        .enumerate()
        .flat_map(|(index, line)| {
            let newline = (index > 0).then(|| Span { text: "\n".into(), ..Span::default() });
            newline.into_iter().chain(line.iter().map(|token| Span {
                text: token.text.clone(),
                italic: token.kind == crate::highlight::TokenKind::Comment,
                color: Some(token.kind.color()),
                ..Span::default()
            }))
        })
        .collect();
    spans_to_commonmark(&spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(blocks: &[Block]) -> Vec<String> {
        blocks.iter().map(Block::plain).collect()
    }

    #[test]
    fn inline_styles() {
        let blocks = parse("un **gras**, *italique*, __souligné__, ~~barré~~ et `code`");
        assert_eq!(blocks.len(), 1);
        let spans = &blocks[0].spans;
        assert!(spans.iter().any(|s| s.text == "gras" && s.bold));
        assert!(spans.iter().any(|s| s.text == "italique" && s.italic));
        assert!(spans.iter().any(|s| s.text == "souligné" && s.underline && !s.bold));
        assert!(spans.iter().any(|s| s.text == "barré" && s.strike));
        assert!(spans.iter().any(|s| s.text == "code" && s.code));
        assert_eq!(plain("un **gras** et *italique*"), "un gras et italique");
    }

    #[test]
    fn line_breaks_are_kept_like_discord() {
        assert_eq!(plain("ligne 1\nligne 2"), "ligne 1\nligne 2");
    }

    #[test]
    fn headings_lists_quotes_and_rules() {
        let blocks = parse("# Titre\n## Sous-titre\n-# petit\n- a\n- b\n  1. un\n  2. deux\n- [x] fait\n- [ ] à faire\n> cité\n\n---");
        let kinds: Vec<BlockKind> = blocks.iter().map(|b| b.kind).collect();
        assert_eq!(kinds[0], BlockKind::Heading(1));
        assert_eq!(kinds[1], BlockKind::Heading(2));
        assert_eq!(kinds[2], BlockKind::Subtext);
        assert_eq!(blocks[2].plain(), "petit");
        assert_eq!(texts(&blocks[3..9]), ["• a", "• b", "1. un", "2. deux", "☑ fait", "☐ à faire"]);
        assert_eq!(blocks[5].indent, 2);
        assert_eq!(blocks[9].quote, 1);
        assert_eq!(blocks.last().unwrap().kind, BlockKind::Rule);
    }

    #[test]
    fn code_blocks_keep_text_and_language() {
        let blocks = parse("avant\n```rust\nfn main() {\n\tlet x = \"**1**\"; // note\n}\n```");
        assert_eq!(blocks[1].kind, BlockKind::Code);
        let code = blocks[1].code.as_ref().unwrap();
        assert_eq!(code.language, "rust");
        assert_eq!(code.text, "fn main() {\n    let x = \"**1**\"; // note\n}");
        assert_eq!(code.lines.len(), 3);
    }

    #[test]
    fn tables() {
        let blocks = parse("| Nom | Âge |\n|:--|--:|\n| Léa | **30** |\n| Max | 25 |");
        assert_eq!(blocks.len(), 1);
        let table = blocks[0].table.as_ref().unwrap();
        assert_eq!(table.align, [Align::Left, Align::Right]);
        assert_eq!(spans_text(&table.header[1]), "Âge");
        assert_eq!(table.rows.len(), 2);
        assert!(table.rows[0][1][0].bold);
    }

    #[test]
    fn colors_links_and_html() {
        let blocks = parse("<font color=\"rouge\">alerte</font> et <span style=\"color:#00ff00\">vert</span> https://exemple.fr. <b>non</b> [x](javascript:alert(1))");
        let spans = &blocks[0].spans;
        assert!(spans.iter().any(|s| s.text == "alerte" && s.color == Some(0xE5484D)));
        assert!(spans.iter().any(|s| s.text == "vert" && s.color == Some(0x00FF00)));
        assert!(spans.iter().any(|s| s.link.as_deref() == Some("https://exemple.fr")));
        assert!(spans.iter().any(|s| s.text.contains("<b>")));
        assert!(spans.iter().all(|s| s.link.as_deref().is_none_or(|l| !l.starts_with("javascript"))));
        assert_eq!(parse_color("#f00"), Some(0xFF0000));
    }

    #[test]
    fn commonmark_for_slint_escapes_text() {
        let z = ZERO_WIDTH;
        let spans = |md: &str| parse(md).remove(0).spans;
        assert_eq!(spans_to_commonmark(&spans("\\# pas un titre")), "\\# pas un titre");
        assert_eq!(spans_to_commonmark(&spans("**gras**")), format!("{z}**{z}gras{z}**{z}"));
        assert_eq!(spans_to_commonmark(&spans("a\nb")), "a\nb");
        assert_eq!(code_commonmark("a`b"), "``a`b``");
        assert!(spans_to_commonmark(&spans("<font color=\"bleu\">x</font>")).contains("<font color=\"#3e7bfa\">x</font>"));
        assert!(spans_to_commonmark(&spans("__x__")).contains("<u>x</u>"));
    }
}
