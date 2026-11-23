//! Affichage AppKit du Markdown (`magicplus_core::markdown`) : une pile de vues, une par bloc
//! (paragraphe, titre, liste, citation, code coloré, tableau, séparateur). Sert aux messages et
//! aux canevas.

use super::*;
use magicplus_core::{
    highlight::{TokenKind, CODE_BACKGROUND},
    markdown::{self, Align, Block, BlockKind, Span},
};
use objc2::{AnyThread, Message};
use objc2_app_kit::{
    NSBackgroundColorAttributeName, NSFontAttributeName, NSFontManager, NSFontTraitMask, NSFontWeightRegular,
    NSForegroundColorAttributeName, NSLinkAttributeName, NSStackViewDistribution, NSStrikethroughStyleAttributeName,
    NSTextAlignment, NSUnderlineStyleAttributeName,
};
use objc2_foundation::{NSAttributedString, NSMutableAttributedString, NSNumber, NSRange, NSURL};
use std::cell::Cell;

/// Retrait d'un niveau de liste, largeur d'une barre de citation.
const LIST_INDENT: f64 = 20.0;
const QUOTE_STEP: f64 = 14.0;

#[derive(Default)]
pub(super) struct FillIvars {
    /// 0 : couleur fixe `rgb` ; 1 : gris discret du système ; 2 : contour seulement ;
    /// 3 : couleur du fond de la fenêtre (anneau autour d'une photo) ; 4 : carte (fond + contour).
    mode: Cell<u8>,
    rgb: Cell<u32>,
    radius: Cell<f64>,
}

define_class!(
    /// Rectangle arrondi plein (fond de code, barre de citation) ou contour (tableau).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "MagicPlusFillView"]
    #[ivars = FillIvars]
    pub(super) struct FillView;

    impl FillView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let ivars = self.ivars();
            let radius = ivars.radius.get();
            match ivars.mode.get() {
                2 => {
                    let rect = self.bounds();
                    let inset = NSRect::new(
                        NSPoint::new(rect.origin.x + 0.5, rect.origin.y + 0.5),
                        NSSize::new(rect.size.width - 1.0, rect.size.height - 1.0),
                    );
                    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(inset, radius, radius);
                    NSColor::separatorColor().setStroke();
                    path.stroke();
                }
                4 => {
                    let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), radius, radius);
                    NSColor::controlBackgroundColor().setFill();
                    path.fill();
                    NSColor::separatorColor().setStroke();
                    path.stroke();
                }
                mode => {
                    let color = match mode {
                        1 => NSColor::tertiaryLabelColor(),
                        3 => NSColor::windowBackgroundColor(),
                        _ => rgb_color(ivars.rgb.get()),
                    };
                    color.setFill();
                    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), radius, radius).fill();
                }
            }
        }
    }
);

pub(super) fn fill_view(mode: u8, rgb: u32, radius: f64, mtm: MainThreadMarker) -> Retained<FillView> {
    let ivars = FillIvars::default();
    ivars.mode.set(mode);
    ivars.rgb.set(rgb);
    ivars.radius.set(radius);
    let view: Retained<FillView> = unsafe { msg_send![super(FillView::alloc(mtm).set_ivars(ivars)), init] };
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view
}

pub(super) fn rgb_color(rgb: u32) -> Retained<NSColor> {
    let channel = |shift: u32| ((rgb >> shift) & 0xFF) as f64 / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(channel(16), channel(8), channel(0), 1.0)
}

/// Morceaux mis en forme → texte AppKit.
pub(super) fn attributed_spans(spans: &[Span], size: f64, base_color: Option<&NSColor>, mtm: MainThreadMarker) -> Retained<NSAttributedString> {
    let result = NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &ns(""));
    let manager = NSFontManager::sharedFontManager(mtm);
    let base = NSFont::systemFontOfSize(size);
    let mut location = 0;
    for span in spans {
        let piece = NSAttributedString::initWithString(NSAttributedString::alloc(), &ns(&span.text));
        result.appendAttributedString(&piece);
        let length = span.text.encode_utf16().count();
        let range = NSRange::new(location, length);
        location += length;

        let mut font = if span.code {
            NSFont::monospacedSystemFontOfSize_weight(size - 1.0, unsafe { NSFontWeightRegular })
        } else {
            base.clone()
        };
        if span.bold {
            font = manager.convertFont_toHaveTrait(&font, NSFontTraitMask::BoldFontMask);
        }
        if span.italic {
            font = manager.convertFont_toHaveTrait(&font, NSFontTraitMask::ItalicFontMask);
        }
        let color = if span.link.is_some() {
            NSColor::linkColor()
        } else if let Some(rgb) = span.color {
            rgb_color(rgb)
        } else {
            base_color.map(|c| c.retain()).unwrap_or_else(NSColor::labelColor)
        };
        unsafe {
            result.addAttribute_value_range(NSFontAttributeName, &font, range);
            result.addAttribute_value_range(NSForegroundColorAttributeName, &color, range);
            if span.strike {
                result.addAttribute_value_range(NSStrikethroughStyleAttributeName, &NSNumber::numberWithInt(1), range);
            }
            if span.underline {
                result.addAttribute_value_range(NSUnderlineStyleAttributeName, &NSNumber::numberWithInt(1), range);
            }
            if span.code {
                result.addAttribute_value_range(NSBackgroundColorAttributeName, &NSColor::quaternaryLabelColor(), range);
            }
            if let Some(url) = span.link.as_deref().and_then(|url| NSURL::URLWithString(&ns(url))) {
                result.addAttribute_value_range(NSLinkAttributeName, &url, range);
            }
        }
    }
    Retained::into_super(result)
}

fn text_field(text: &NSAttributedString, width: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = NSTextField::wrappingLabelWithString(&ns(""), mtm);
    field.setAttributedStringValue(text);
    field.setSelectable(true);
    // Indispensable pour que les liens soient cliquables.
    field.setAllowsEditingTextAttributes(true);
    field.setPreferredMaxLayoutWidth(width.max(40.0));
    field
}

fn code_view(block: &Block, width: f64, mtm: MainThreadMarker) -> Retained<NSView> {
    let code = block.code.clone().unwrap_or_else(|| markdown::CodeBlock { language: String::new(), text: String::new(), lines: Vec::new() });
    let spans: Vec<Span> = code
        .lines
        .iter()
        .enumerate()
        .flat_map(|(index, line)| {
            let newline = (index > 0).then(|| Span { text: "\n".into(), ..Span::default() });
            newline.into_iter().chain(line.iter().map(|token| Span {
                text: token.text.clone(),
                italic: token.kind == TokenKind::Comment,
                color: Some(token.kind.color()),
                ..Span::default()
            }))
        })
        .collect();
    let attributed = NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &ns(""));
    attributed.appendAttributedString(&attributed_spans(&spans, 12.0, Some(&rgb_color(TokenKind::Plain.color())), mtm));
    // Police à chasse fixe partout (les morceaux sont en police normale par défaut).
    let manager = NSFontManager::sharedFontManager(mtm);
    let mono = NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular });
    let mono_italic = manager.convertFont_toHaveTrait(&mono, NSFontTraitMask::ItalicFontMask);
    let mut location = 0;
    for span in &spans {
        let length = span.text.encode_utf16().count();
        let font = if span.italic { &mono_italic } else { &mono };
        unsafe { attributed.addAttribute_value_range(NSFontAttributeName, font, NSRange::new(location, length)) };
        location += length;
    }

    let background = fill_view(0, CODE_BACKGROUND, 6.0, mtm);
    let inner = stack(true, 4.0, mtm);
    if !code.language.is_empty() {
        let language = label(&code.language, mtm);
        language.setFont(Some(&NSFont::systemFontOfSize(10.5)));
        language.setTextColor(Some(&rgb_color(TokenKind::Comment.color())));
        inner.addArrangedSubview(&language);
    }
    let text = text_field(&Retained::into_super(attributed), width - 20.0, mtm);
    inner.addArrangedSubview(&text);
    background.addSubview(&inner);
    pin(&inner, &background, 8.0, 10.0, 8.0);
    background.widthAnchor().constraintEqualToConstant(width).setActive(true);
    Retained::into_super(background)
}

fn table_view(block: &Block, width: f64, mtm: MainThreadMarker) -> Retained<NSView> {
    let table = block.table.clone().unwrap_or_default();
    let columns = table.header.len().max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0)).max(1);
    let width = width.min(columns as f64 * 180.0).max(120.0);
    let cell_width = width / columns as f64;
    let frame = fill_view(2, 0, 6.0, mtm);
    let rows = stack(true, 0.0, mtm);
    let all: Vec<(bool, &Vec<Vec<Span>>)> =
        std::iter::once((true, &table.header)).chain(table.rows.iter().map(|row| (false, row))).collect();
    for (index, (header, cells)) in all.into_iter().enumerate() {
        if index > 0 {
            let line = NSBox::new(mtm);
            line.setBoxType(NSBoxType::Separator);
            line.setTranslatesAutoresizingMaskIntoConstraints(false);
            full_width(&rows, &line);
        }
        let row = stack(false, 0.0, mtm);
        row.setDistribution(NSStackViewDistribution::FillEqually);
        row.setAlignment(NSLayoutAttribute::Top);
        for column in 0..columns {
            let mut spans = cells.get(column).cloned().unwrap_or_default();
            if header {
                for span in &mut spans {
                    span.bold = true;
                }
            }
            let field = text_field(&attributed_spans(&spans, 12.5, None, mtm), cell_width - 14.0, mtm);
            field.setAlignment(match table.align.get(column) {
                Some(Align::Center) => NSTextAlignment::Center,
                Some(Align::Right) => NSTextAlignment::Right,
                _ => NSTextAlignment::Left,
            });
            let cell = NSView::new(mtm);
            cell.setTranslatesAutoresizingMaskIntoConstraints(false);
            cell.addSubview(&field);
            pin(&field, &cell, 5.0, 7.0, 5.0);
            row.addArrangedSubview(&cell);
        }
        full_width(&rows, &row);
    }
    frame.addSubview(&rows);
    pin(&rows, &frame, 1.0, 1.0, 1.0);
    frame.widthAnchor().constraintEqualToConstant(width).setActive(true);
    Retained::into_super(frame)
}

fn block_view(block: &Block, size: f64, width: f64, mtm: MainThreadMarker) -> Retained<NSView> {
    let offset = block.indent.saturating_sub(1) as f64 * LIST_INDENT + block.quote as f64 * QUOTE_STEP;
    let marker_width = if block.marker.is_empty() && block.indent == 0 { 0.0 } else { LIST_INDENT };
    let content_width = (width - offset - marker_width).max(80.0);
    let content: Retained<NSView> = match block.kind {
        BlockKind::Code => code_view(block, content_width, mtm),
        BlockKind::Table => table_view(block, content_width, mtm),
        BlockKind::Rule => {
            let line = NSBox::new(mtm);
            line.setBoxType(NSBoxType::Separator);
            line.setTranslatesAutoresizingMaskIntoConstraints(false);
            line.widthAnchor().constraintEqualToConstant(content_width).setActive(true);
            Retained::into_super(line)
        }
        kind => {
            let (font_size, color) = match kind {
                BlockKind::Heading(1) => (size * 1.6, None),
                BlockKind::Heading(2) => (size * 1.35, None),
                BlockKind::Heading(_) => (size * 1.15, None),
                BlockKind::Subtext => (size * 0.82, Some(NSColor::secondaryLabelColor())),
                _ => (size, None),
            };
            let heading = matches!(kind, BlockKind::Heading(_));
            let spans: Vec<Span> = block.spans.iter().cloned().map(|mut s| {
                s.bold |= heading;
                s
            }).collect();
            Retained::into_super(Retained::into_super(text_field(&attributed_spans(&spans, font_size, color.as_deref(), mtm), content_width, mtm)))
        }
    };
    if offset == 0.0 && marker_width == 0.0 {
        return content;
    }

    let row = stack(false, 0.0, mtm);
    row.setAlignment(NSLayoutAttribute::Top);
    for _ in 0..block.quote {
        let bar = fill_view(1, 0, 1.5, mtm);
        bar.widthAnchor().constraintEqualToConstant(3.0).setActive(true);
        row.addArrangedSubview(&bar);
        bar.heightAnchor().constraintEqualToAnchor(&row.heightAnchor()).setActive(true);
        let gap = NSView::new(mtm);
        gap.setTranslatesAutoresizingMaskIntoConstraints(false);
        gap.widthAnchor().constraintEqualToConstant(QUOTE_STEP - 3.0).setActive(true);
        row.addArrangedSubview(&gap);
    }
    let indent = block.indent.saturating_sub(1) as f64 * LIST_INDENT;
    if indent > 0.0 {
        let gap = NSView::new(mtm);
        gap.setTranslatesAutoresizingMaskIntoConstraints(false);
        gap.widthAnchor().constraintEqualToConstant(indent).setActive(true);
        row.addArrangedSubview(&gap);
    }
    if marker_width > 0.0 {
        let marker = label(&block.marker, mtm);
        marker.setFont(Some(&NSFont::systemFontOfSize(size)));
        marker.setTextColor(Some(&NSColor::secondaryLabelColor()));
        marker.setAlignment(NSTextAlignment::Left);
        marker.widthAnchor().constraintEqualToConstant(marker_width).setActive(true);
        row.addArrangedSubview(&marker);
    }
    row.addArrangedSubview(&content);
    Retained::into_super(row)
}

/// Message ou canevas en Markdown → pile de vues de largeur `width` au plus.
pub(super) fn markdown_view(text: &str, size: f64, width: f64, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let view = stack(true, 5.0, mtm);
    for block in markdown::parse(text) {
        view.addArrangedSubview(&block_view(&block, size, width, mtm));
    }
    view
}

/// Code des blocs ```…``` d'un texte (pour « Copier le code »).
pub(super) fn code_of(text: &str) -> Option<String> {
    let code: Vec<String> = markdown::parse(text).into_iter().filter_map(|block| block.code.map(|c| c.text)).collect();
    (!code.is_empty()).then(|| code.join("\n\n"))
}
