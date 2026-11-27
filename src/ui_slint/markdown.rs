//! Markdown → blocs Slint (`BlockItem`, ui/markdown.slint), pour les messages et les canevas.

use super::{BlockItem, TableCellItem, TableRowItem};
use magicplus_core::markdown::{self, Align, Block, BlockKind, Span};
use slint::{ModelRc, StyledText, VecModel};

/// Police à chasse fixe du système, pour les blocs de code.
pub fn monospace_family() -> &'static str {
    if cfg!(target_os = "windows") {
        "Consolas"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}

fn styled_or_plain(commonmark: &str, plain: &str) -> StyledText {
    StyledText::from_markdown(commonmark).unwrap_or_else(|error| {
        if std::env::var_os("MAGICPLUS_DEBUG").is_some() {
            eprintln!("[markdown] refusé par Slint : {error}");
        }
        StyledText::from_plain_text(plain)
    })
}

/// Morceaux mis en forme → texte Slint.
pub fn styled(spans: &[Span]) -> StyledText {
    let plain: String = spans.iter().map(|s| s.text.as_str()).collect();
    styled_or_plain(&markdown::spans_to_commonmark(spans), &plain)
}

fn block_item(block: &Block) -> BlockItem {
    // Cases à cocher en émojis : présents dans toutes les polices du système.
    let marker = match block.marker.as_str() {
        "☐" => "⬜",
        "☑" => "✅",
        other => other,
    };
    let mut item = BlockItem {
        marker: marker.into(),
        indent: block.indent as i32,
        quote: block.quote as i32,
        ..BlockItem::default()
    };
    match block.kind {
        BlockKind::Text => item.text = styled(&block.spans),
        BlockKind::Heading(level) => {
            item.kind = 1;
            item.level = level as i32;
            let spans: Vec<Span> = block.spans.iter().cloned().map(|mut s| {
                s.bold = true;
                s
            }).collect();
            item.text = styled(&spans);
        }
        BlockKind::Subtext => {
            item.kind = 2;
            item.text = styled(&block.spans);
        }
        BlockKind::Code => {
            item.kind = 3;
            if let Some(code) = &block.code {
                item.language = code.language.clone().into();
                item.text = styled_or_plain(&markdown::code_to_commonmark(&code.lines), &code.text);
            }
        }
        BlockKind::Table => {
            item.kind = 4;
            let table = block.table.clone().unwrap_or_default();
            let columns = table.header.len().max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0)).max(1);
            item.columns = columns as i32;
            let row = |cells: &Vec<Vec<Span>>, header: bool| TableRowItem {
                header,
                cells: ModelRc::new(VecModel::from(
                    (0..columns)
                        .map(|column| {
                            let mut spans = cells.get(column).cloned().unwrap_or_default();
                            if header {
                                spans.iter_mut().for_each(|s| s.bold = true);
                            }
                            TableCellItem {
                                text: styled(&spans),
                                align: match table.align.get(column) {
                                    Some(Align::Center) => 1,
                                    Some(Align::Right) => 2,
                                    _ => 0,
                                },
                            }
                        })
                        .collect::<Vec<_>>(),
                )),
            };
            let rows: Vec<TableRowItem> =
                std::iter::once(row(&table.header, true)).chain(table.rows.iter().map(|r| row(r, false))).collect();
            item.rows = ModelRc::new(VecModel::from(rows));
        }
        BlockKind::Rule => item.kind = 5,
    }
    item
}

/// Texte Markdown → blocs à afficher.
pub fn blocks(text: &str) -> ModelRc<BlockItem> {
    ModelRc::new(VecModel::from(markdown::parse(text).iter().map(block_item).collect::<Vec<_>>()))
}

/// Comme `blocks`, avec les mentions « @nom » mises en évidence.
pub fn blocks_with_mentions(text: &str, names: &[String]) -> ModelRc<BlockItem> {
    let mut parsed = markdown::parse(text);
    magicplus_core::mentions::highlight(&mut parsed, names);
    ModelRc::new(VecModel::from(parsed.iter().map(block_item).collect::<Vec<_>>()))
}

/// Code des blocs ```…``` d'un texte (pour « Copier le code »).
pub fn code_of(text: &str) -> Option<String> {
    let code: Vec<String> = markdown::parse(text).into_iter().filter_map(|block| block.code.map(|c| c.text)).collect();
    (!code.is_empty()).then(|| code.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use magicplus_core::markdown::{self, BlockKind};
    use slint::StyledText;

    /// Aucun message, même piégé, ne doit faire échouer l'analyseur Markdown de Slint.
    #[test]
    fn slint_accepte_tous_les_messages() {
        let messages = [
            "Salut **B** 👋",
            "# titre\n## sous-titre\n-# petit\n- liste\n1. numéro\n> citation\n---\n===",
            "<b>html</b> <u>souligné</u> <font color=red>x</font> <font color=\"#12ab34\">y</font> &amp; &#169;",
            "**gras _italique ~~barré~~_** `code` [lien](https://exemple.fr/a_(b)) https://nu.fr/x?y=1&z=2.",
            "    bloc indenté\n\tavec tabulation",
            "```rust\nlet x = `a` + \"**\"; // {}\n```\naprès",
            "{} {0} \u{E541} \\ \\\\ * _ ~ ` [ ] ( ) < > | !",
            "**(parenthèses)**a*b*c **  espaces  ** ~~~triple~~~ __souligné__",
            "| a | **b** |\n|---|:-:|\n| <font color=\"vert\">c</font> | `d` |",
            "> [!WARNING]\n> attention\n\n- [x] fait\n- [ ] pas fait",
            "a\n\n\nb\r\nc",
            "[](https://vide.fr) [texte]( https://espace.fr ) [x](javascript:alert(1)) ![image](https://x.fr/a.png)",
            "",
            "🎉🎉",
        ];
        for message in messages {
            for block in markdown::parse(message) {
                let commonmark = match block.kind {
                    BlockKind::Code => markdown::code_to_commonmark(&block.code.unwrap().lines),
                    BlockKind::Table => {
                        let table = block.table.unwrap();
                        for cell in table.header.iter().chain(table.rows.iter().flatten()) {
                            let text = markdown::spans_to_commonmark(cell);
                            assert!(StyledText::from_markdown(&text).is_ok(), "cellule refusée : {message:?} → {text:?}");
                        }
                        continue;
                    }
                    _ => markdown::spans_to_commonmark(&block.spans),
                };
                assert!(StyledText::from_markdown(&commonmark).is_ok(), "refusé : {message:?} → {commonmark:?}");
            }
        }
    }
}
