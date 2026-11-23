//! Page « Discussions » de l'interface macOS (AppKit), dans l'esprit de Discord :
//! liste des conversations à gauche ; à droite, messages groupés par auteur (avatar, nom, heure),
//! bulles en Markdown, réponses, réactions, modification, pièces jointes, accusés de lecture,
//! « en train d'écrire », autocollants et GIF. Rien ne dépend d'Internet.

use super::*;
use crate::shared::{avatar_hue, initials, jumbo, open_path, time_label};
use magicplus_core::{markdown, AttachmentKind, ConversationKind, ConversationView, MessageStatus, MessageView};
use objc2::{runtime::NSObjectProtocol as _, AnyThread};
use objc2_app_kit::{
    NSBackgroundColorAttributeName, NSClickGestureRecognizer, NSControlSize, NSEventModifierFlags,
    NSFontAttributeName, NSFontManager, NSFontTraitMask, NSFontWeightRegular, NSForegroundColorAttributeName,
    NSLayoutConstraintOrientation, NSLineBreakMode, NSLinkAttributeName, NSPasteboard,
    NSStrikethroughStyleAttributeName,
};
use objc2_foundation::{NSAttributedString, NSMutableAttributedString, NSNumber, NSRange, NSURL};
use std::{
    cell::Cell,
    path::PathBuf,
    time::Instant,
};

/// Position de la page dans la barre latérale.
pub(super) const CHAT_PAGE: usize = 1;
pub(super) const QUICK_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🎉"];
const MESSAGE_LIMIT: usize = 300;
const MAX_BUBBLE: f64 = 520.0;
/// Au-delà de cet écart, un nouveau message de la même personne affiche à nouveau son nom.
const GROUP_WINDOW_MS: u64 = 5 * 60 * 1000;

// ------------------------------------------------------------------ vues dessinées

#[derive(Default)]
pub(super) struct ShapeIvars {
    /// Bulle : 0 neutre, 1 la nôtre, 2 ligne sélectionnée, 3 transparente. Avatar : teinte.
    style: Cell<u8>,
    hue: Cell<f64>,
}

define_class!(
    /// Fond arrondi (bulle de message, ligne de conversation).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "MagicPlusBubbleView"]
    #[ivars = ShapeIvars]
    pub(super) struct BubbleView;

    impl BubbleView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let color = match self.ivars().style.get() {
                1 => NSColor::controlAccentColor().colorWithAlphaComponent(0.24),
                2 => NSColor::controlAccentColor().colorWithAlphaComponent(0.16),
                3 => return,
                _ => NSColor::quaternaryLabelColor(),
            };
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), 12.0, 12.0);
            color.setFill();
            path.fill();
        }
    }
);

define_class!(
    /// Pastille ronde colorée avec les initiales.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "MagicPlusAvatarView"]
    #[ivars = ShapeIvars]
    pub(super) struct AvatarView;

    impl AvatarView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let color = NSColor::colorWithHue_saturation_brightness_alpha(self.ivars().hue.get(), 0.45, 0.72, 1.0);
            color.setFill();
            NSBezierPath::bezierPathWithOvalInRect(self.bounds()).fill();
        }
    }
);

fn bubble(style: u8, top: f64, side: f64, mtm: MainThreadMarker) -> (Retained<BubbleView>, Retained<NSStackView>) {
    let ivars = ShapeIvars::default();
    ivars.style.set(style);
    let view: Retained<BubbleView> = unsafe { msg_send![super(BubbleView::alloc(mtm).set_ivars(ivars)), init] };
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    let inner = stack(true, 4.0, mtm);
    view.addSubview(&inner);
    pin(&inner, &view, top, side, top);
    (view, inner)
}

fn avatar(name: &str, seed: &str, size: f64, mtm: MainThreadMarker) -> Retained<AvatarView> {
    let ivars = ShapeIvars::default();
    ivars.hue.set(avatar_hue(seed) as f64);
    let view: Retained<AvatarView> = unsafe { msg_send![super(AvatarView::alloc(mtm).set_ivars(ivars)), init] };
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.widthAnchor().constraintEqualToConstant(size).setActive(true);
    view.heightAnchor().constraintEqualToConstant(size).setActive(true);
    let letters = label(&initials(name), mtm);
    letters.setFont(Some(&semibold(size * 0.38)));
    letters.setTextColor(Some(&NSColor::whiteColor()));
    letters.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.addSubview(&letters);
    letters.centerXAnchor().constraintEqualToAnchor(&view.centerXAnchor()).setActive(true);
    letters.centerYAnchor().constraintEqualToAnchor(&view.centerYAnchor()).setActive(true);
    view
}

/// Markdown → texte mis en forme AppKit.
fn attributed(text: &str, size: f64, mtm: MainThreadMarker) -> Retained<NSAttributedString> {
    let result = NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &ns(""));
    let manager = NSFontManager::sharedFontManager(mtm);
    let base = NSFont::systemFontOfSize(size);
    let mut location = 0;
    for span in markdown::parse(text) {
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
        let color = if span.link.is_some() { NSColor::linkColor() } else { NSColor::labelColor() };
        unsafe {
            result.addAttribute_value_range(NSFontAttributeName, &font, range);
            result.addAttribute_value_range(NSForegroundColorAttributeName, &color, range);
            if span.strike {
                result.addAttribute_value_range(NSStrikethroughStyleAttributeName, &NSNumber::numberWithInt(1), range);
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

// ------------------------------------------------------------------ état de la page

pub(super) struct ChatWidgets {
    list: Retained<NSStackView>,
    list_empty: Retained<NSTextField>,
    title: Retained<NSTextField>,
    subtitle: Retained<NSTextField>,
    group_button: Retained<NSButton>,
    scroll: Retained<NSScrollView>,
    document: Retained<FlippedView>,
    messages: Retained<NSStackView>,
    typing: Retained<NSTextField>,
    banner: Retained<NSStackView>,
    banner_label: Retained<NSTextField>,
    pending: Retained<NSStackView>,
    closed_label: Retained<NSTextField>,
    composer: Retained<NSStackView>,
    input: Retained<NSTextView>,
    stickers_button: Retained<NSButton>,
    empty: Retained<NSTextField>,
    content: Vec<Retained<NSView>>,
}

#[derive(Default)]
pub(super) struct ChatUi {
    current: Option<String>,
    seen_revision: u64,
    list_ids: Vec<String>,
    list_signature: String,
    /// Lignes affichées : identifiant du message, empreinte de son rendu, vue.
    rows: Vec<(String, String, Retained<NSView>)>,
    row_messages: Vec<MessageView>,
    reaction_tags: Vec<(String, String)>,
    attachment_paths: Vec<PathBuf>,
    /// Réponse en préparation : identifiant, auteur, extrait.
    reply_to: Option<(String, String, String)>,
    editing: Option<String>,
    pending: Vec<PathBuf>,
    direct_choices: Vec<String>,
    sticker_ids: Vec<String>,
    has_progress: bool,
    last_refresh: Option<Instant>,
    /// Largeur de texte utilisée pour les bulles (refaites si la fenêtre change de taille).
    bubble_width: f64,
    current_conversation: Option<ConversationView>,
}

// ------------------------------------------------------------------ construction de la page

/// Liste défilante dont le contenu part du haut.
fn scrolling_stack(spacing: f64, top: f64, side: f64, mtm: MainThreadMarker) -> (Retained<NSScrollView>, Retained<FlippedView>, Retained<NSStackView>) {
    let scroll = NSScrollView::new(mtm);
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setDrawsBackground(false);
    scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
    let document = FlippedView::new(mtm);
    document.setTranslatesAutoresizingMaskIntoConstraints(false);
    scroll.setDocumentView(Some(&document));
    let clip = scroll.contentView();
    document.leadingAnchor().constraintEqualToAnchor(&clip.leadingAnchor()).setActive(true);
    document.topAnchor().constraintEqualToAnchor(&clip.topAnchor()).setActive(true);
    document.widthAnchor().constraintEqualToAnchor(&clip.widthAnchor()).setActive(true);
    let content = stack(true, spacing, mtm);
    document.addSubview(&content);
    pin(&content, &document, top, side, top);
    (scroll, document, content)
}

fn icon_button(symbol_name: &str, tooltip: &str, action: Sel, controller: &Controller, mtm: MainThreadMarker) -> Retained<NSButton> {
    let item = button("", action, controller, mtm);
    item.setBordered(false);
    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(symbol_name), Some(&ns(tooltip))) {
        item.setImage(Some(&image));
        item.setImagePosition(NSCellImagePosition::ImageOnly);
    }
    item.setToolTip(Some(&ns(tooltip)));
    item.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    item.setTranslatesAutoresizingMaskIntoConstraints(false);
    item.widthAnchor().constraintEqualToConstant(28.0).setActive(true);
    item.heightAnchor().constraintEqualToConstant(28.0).setActive(true);
    item
}

fn horizontal_separator(mtm: MainThreadMarker) -> Retained<NSBox> {
    let line = NSBox::new(mtm);
    line.setBoxType(NSBoxType::Separator);
    line.setTranslatesAutoresizingMaskIntoConstraints(false);
    line
}

pub(super) fn build(controller: &Controller, mtm: MainThreadMarker) -> (Retained<NSView>, ChatWidgets) {
    let root = NSView::new(mtm);
    root.setTranslatesAutoresizingMaskIntoConstraints(false);

    // ---- colonne des conversations
    // Les marges passent par une boîte : `full_width` aligne les enfants sur la largeur de la pile.
    let left_box = NSView::new(mtm);
    left_box.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(&left_box);
    left_box.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    left_box.leadingAnchor().constraintEqualToAnchor(&root.leadingAnchor()).setActive(true);
    left_box.bottomAnchor().constraintEqualToAnchor(&root.bottomAnchor()).setActive(true);
    left_box.widthAnchor().constraintEqualToConstant(286.0).setActive(true);
    let left = stack(true, 8.0, mtm);
    left_box.addSubview(&left);
    pin(&left, &left_box, 22.0, 12.0, 10.0);

    let heading = stack(false, 8.0, mtm);
    let title = label("Discussions", mtm);
    title.setFont(Some(&NSFont::boldSystemFontOfSize(20.0)));
    heading.addArrangedSubview(&title);
    let new_button = icon_button("square.and.pencil", "Nouvelle discussion ou nouveau groupe", sel!(chatNew:), controller, mtm);
    heading.addView_inGravity(&new_button, NSStackViewGravity::Trailing);
    full_width(&left, &heading);

    let (list_scroll, _, list) = scrolling_stack(4.0, 0.0, 0.0, mtm);
    full_width(&left, &list_scroll);
    list_scroll.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);
    list_scroll.heightAnchor().constraintGreaterThanOrEqualToConstant(120.0).setActive(true);
    let list_empty = paragraph(
        "Aucune discussion. Cliquez sur le crayon pour écrire à un appareil associé ou créer un groupe.",
        mtm,
    );
    full_width(&list, &list_empty);

    let separator = NSBox::new(mtm);
    separator.setBoxType(NSBoxType::Separator);
    separator.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(&separator);
    separator.leadingAnchor().constraintEqualToAnchor(&left_box.trailingAnchor()).setActive(true);
    separator.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    separator.bottomAnchor().constraintEqualToAnchor(&root.bottomAnchor()).setActive(true);
    separator.widthAnchor().constraintEqualToConstant(1.0).setActive(true);

    // ---- conversation ouverte
    let panel = NSView::new(mtm);
    panel.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(&panel);
    panel.leadingAnchor().constraintEqualToAnchor(&separator.trailingAnchor()).setActive(true);
    panel.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    panel.trailingAnchor().constraintEqualToAnchor(&root.trailingAnchor()).setActive(true);
    panel.bottomAnchor().constraintEqualToAnchor(&root.bottomAnchor()).setActive(true);

    let header = stack(false, 10.0, mtm);
    header.setEdgeInsets(NSEdgeInsets { top: 18.0, left: 20.0, bottom: 10.0, right: 16.0 });
    let header_texts = stack(true, 2.0, mtm);
    let conversation_title = strong("", 16.0, mtm);
    let subtitle = secondary("", mtm);
    subtitle.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    subtitle.setContentCompressionResistancePriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    header_texts.addArrangedSubview(&conversation_title);
    header_texts.addArrangedSubview(&subtitle);
    header.addArrangedSubview(&header_texts);
    let group_button = icon_button("ellipsis.circle", "Options du groupe", sel!(chatGroupMenu:), controller, mtm);
    header.addView_inGravity(&group_button, NSStackViewGravity::Trailing);
    panel.addSubview(&header);
    header.topAnchor().constraintEqualToAnchor(&panel.topAnchor()).setActive(true);
    header.leadingAnchor().constraintEqualToAnchor(&panel.leadingAnchor()).setActive(true);
    header.trailingAnchor().constraintEqualToAnchor(&panel.trailingAnchor()).setActive(true);

    let header_line = horizontal_separator(mtm);
    panel.addSubview(&header_line);
    header_line.topAnchor().constraintEqualToAnchor(&header.bottomAnchor()).setActive(true);
    header_line.leadingAnchor().constraintEqualToAnchor(&panel.leadingAnchor()).setActive(true);
    header_line.trailingAnchor().constraintEqualToAnchor(&panel.trailingAnchor()).setActive(true);
    header_line.heightAnchor().constraintEqualToConstant(1.0).setActive(true);

    let (scroll, document, messages) = scrolling_stack(2.0, 12.0, 18.0, mtm);
    panel.addSubview(&scroll);
    scroll.topAnchor().constraintEqualToAnchor(&header_line.bottomAnchor()).setActive(true);
    scroll.leadingAnchor().constraintEqualToAnchor(&panel.leadingAnchor()).setActive(true);
    scroll.trailingAnchor().constraintEqualToAnchor(&panel.trailingAnchor()).setActive(true);

    // ---- bas : frappe, réponse/modification, pièces jointes en attente, zone de saisie
    let bottom_box = NSView::new(mtm);
    bottom_box.setTranslatesAutoresizingMaskIntoConstraints(false);
    panel.addSubview(&bottom_box);
    bottom_box.leadingAnchor().constraintEqualToAnchor(&panel.leadingAnchor()).setActive(true);
    bottom_box.trailingAnchor().constraintEqualToAnchor(&panel.trailingAnchor()).setActive(true);
    bottom_box.bottomAnchor().constraintEqualToAnchor(&panel.bottomAnchor()).setActive(true);
    scroll.bottomAnchor().constraintEqualToAnchor(&bottom_box.topAnchor()).setActive(true);
    let bottom = stack(true, 6.0, mtm);
    bottom_box.addSubview(&bottom);
    pin(&bottom, &bottom_box, 6.0, 18.0, 12.0);

    let typing = secondary("", mtm);
    typing.setHidden(true);
    bottom.addArrangedSubview(&typing);

    let banner = stack(false, 8.0, mtm);
    let banner_label = secondary("", mtm);
    banner_label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    banner_label.setContentCompressionResistancePriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    banner.addArrangedSubview(&banner_label);
    let cancel_banner = icon_button("xmark.circle.fill", "Annuler (Échap)", sel!(chatCancelBanner:), controller, mtm);
    banner.addView_inGravity(&cancel_banner, NSStackViewGravity::Trailing);
    banner.setHidden(true);
    full_width(&bottom, &banner);

    let pending = stack(false, 6.0, mtm);
    pending.setHidden(true);
    bottom.addArrangedSubview(&pending);

    let closed_label = secondary("Vous avez quitté ce groupe : la discussion est en lecture seule.", mtm);
    closed_label.setHidden(true);
    bottom.addArrangedSubview(&closed_label);

    let composer = stack(false, 8.0, mtm);
    composer.setAlignment(NSLayoutAttribute::CenterY);
    let attach = icon_button("plus.circle.fill", "Joindre des fichiers (15 au plus, 1 Go chacun)", sel!(chatAttach:), controller, mtm);
    composer.addArrangedSubview(&attach);
    let (input_scroll, input) = text_area(true, mtm);
    input_scroll.heightAnchor().constraintEqualToConstant(54.0).setActive(true);
    input.setFont(Some(&NSFont::systemFontOfSize(13.5)));
    input.setAllowsUndo(true);
    input.setAutomaticQuoteSubstitutionEnabled(false);
    input.setDelegate(Some(ProtocolObject::from_ref(controller)));
    input_scroll.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    composer.addArrangedSubview(&input_scroll);
    let emoji = icon_button("face.smiling", "Émojis", sel!(chatEmoji:), controller, mtm);
    composer.addArrangedSubview(&emoji);
    let stickers_button = icon_button("sparkles.rectangle.stack", "Autocollants et GIF", sel!(chatStickers:), controller, mtm);
    composer.addArrangedSubview(&stickers_button);
    let send = button("Envoyer", sel!(chatSend:), controller, mtm);
    composer.addArrangedSubview(&send);
    full_width(&bottom, &composer);
    full_width(
        &bottom,
        &secondary("Entrée pour envoyer, Maj+Entrée pour aller à la ligne. **gras**, *italique*, `code`, ~~barré~~.", mtm),
    );

    let empty = label("Choisissez une discussion à gauche, ou créez-en une avec le crayon.", mtm);
    empty.setTextColor(Some(&NSColor::secondaryLabelColor()));
    empty.setTranslatesAutoresizingMaskIntoConstraints(false);
    panel.addSubview(&empty);
    empty.centerXAnchor().constraintEqualToAnchor(&panel.centerXAnchor()).setActive(true);
    empty.centerYAnchor().constraintEqualToAnchor(&panel.centerYAnchor()).setActive(true);

    let content: Vec<Retained<NSView>> = vec![
        Retained::into_super(header),
        Retained::into_super(header_line),
        Retained::into_super(scroll.clone()),
        bottom_box,
    ];
    for view in &content {
        view.setHidden(true);
    }

    let widgets = ChatWidgets {
        list,
        list_empty,
        title: conversation_title,
        subtitle,
        group_button,
        scroll,
        document,
        messages,
        typing,
        banner,
        banner_label,
        pending,
        closed_label,
        composer,
        input,
        stickers_button,
        empty,
        content,
    };
    (root, widgets)
}

// ------------------------------------------------------------------ logique de la page

impl Controller {
    fn chat_widgets(&self) -> &ChatWidgets {
        self.ivars().chat_widgets.get().expect("page Discussions construite")
    }

    pub(super) fn chat_visible(&self) -> bool {
        self.ivars().current_page.get() == CHAT_PAGE
    }

    /// Appelé toutes les 150 ms : ne redessine que si les discussions ont changé.
    pub(super) fn chat_tick(&self) {
        let revision = self.engine().chat_revision();
        let (seen, progress, last) = {
            let ui = self.ivars().chat.borrow();
            (ui.seen_revision, ui.has_progress, ui.last_refresh)
        };
        let progress_due = progress && last.is_none_or(|at| at.elapsed().as_millis() > 700);
        let resized = self.ivars().chat.borrow().current.is_some()
            && (self.bubble_text_width() - self.ivars().chat.borrow().bubble_width).abs() > 24.0;
        if resized {
            self.refresh_chat(true);
        } else if revision != seen || progress_due {
            self.refresh_chat(false);
        }
    }

    pub(super) fn refresh_chat(&self, force_messages: bool) {
        let mtm = self.mtm();
        let engine = self.engine();
        let conversations = engine.conversations();

        // Pastille des non-lus dans la barre latérale.
        let unread: usize = conversations.iter().map(|c| c.unread).sum();
        if let Some(nav) = self.widgets().nav.get(CHAT_PAGE) {
            let title = if unread > 0 { format!("  Discussions  ({unread})") } else { "  Discussions".to_string() };
            nav.setTitle(&ns(&title));
        }

        // Conversation affichée : toujours une qui existe ; à l'ouverture de la page, la plus récente.
        let (current, force_messages) = {
            let mut ui = self.ivars().chat.borrow_mut();
            if ui.current.as_ref().is_some_and(|id| !conversations.iter().any(|c| &c.id == id)) {
                ui.current = None;
            }
            let mut force = force_messages;
            if ui.current.is_none() && self.chat_visible() {
                if let Some(first) = conversations.first() {
                    ui.current = Some(first.id.clone());
                    force = true;
                }
            }
            (ui.current.clone(), force)
        };
        self.refresh_conversation_list(&conversations, current.as_deref(), mtm);

        let widgets = self.chat_widgets();
        let Some(conversation) = current.and_then(|id| conversations.into_iter().find(|c| c.id == id)) else {
            for view in &widgets.content {
                view.setHidden(true);
            }
            widgets.empty.setHidden(false);
            let mut ui = self.ivars().chat.borrow_mut();
            ui.current_conversation = None;
            ui.seen_revision = engine.chat_revision();
            ui.last_refresh = Some(Instant::now());
            return;
        };
        widgets.empty.setHidden(true);
        for view in &widgets.content {
            view.setHidden(false);
        }

        widgets.title.setStringValue(&ns(&conversation.title));
        let others: Vec<&magicplus_core::MemberView> = conversation.members.iter().filter(|m| !m.me).collect();
        let subtitle = match conversation.kind {
            ConversationKind::Direct => match others.first() {
                Some(member) if member.online => "En ligne · chiffré de bout en bout".to_string(),
                Some(_) => "Hors ligne · les messages partiront à son retour".to_string(),
                None => String::new(),
            },
            ConversationKind::Group => {
                let names: Vec<String> = conversation
                    .members
                    .iter()
                    .map(|m| if m.me { "vous".to_string() } else if m.online { m.name.clone() } else { format!("{} (hors ligne)", m.name) })
                    .collect();
                format!("{} membres · {}", conversation.members.len(), names.join(", "))
            }
        };
        widgets.subtitle.setStringValue(&ns(&subtitle));
        widgets.group_button.setHidden(conversation.kind != ConversationKind::Group);

        if conversation.typing.is_empty() {
            widgets.typing.setHidden(true);
        } else {
            let verb = if conversation.typing.len() > 1 { "écrivent" } else { "écrit" };
            widgets.typing.setStringValue(&ns(&format!("{} {verb}…", conversation.typing.join(", "))));
            widgets.typing.setHidden(false);
        }
        widgets.closed_label.setHidden(!conversation.closed);
        widgets.composer.setHidden(conversation.closed);

        let messages = engine.messages(&conversation.id, MESSAGE_LIMIT);
        let appended = self.refresh_messages(&messages, force_messages, mtm);

        // Lu dès que la conversation est affichée dans la fenêtre active.
        let window_active = widgets.input.window().is_some_and(|window| window.isKeyWindow());
        if conversation.unread > 0 && self.chat_visible() && window_active {
            engine.mark_read(&conversation.id);
        }

        if appended {
            self.scroll_messages_to_bottom();
        }
        let mut ui = self.ivars().chat.borrow_mut();
        ui.has_progress = messages.iter().any(|m| m.attachments.iter().any(|a| a.path.is_none()));
        ui.current_conversation = Some(conversation);
        ui.seen_revision = engine.chat_revision();
        ui.last_refresh = Some(Instant::now());
    }

    fn refresh_conversation_list(&self, conversations: &[ConversationView], current: Option<&str>, mtm: MainThreadMarker) {
        let signature: String = conversations
            .iter()
            .map(|c| {
                let online = c.members.iter().any(|m| !m.me && m.online);
                format!("{}|{}|{}|{}|{}|{}|{}\n", c.id, c.title, c.unread, c.last_preview, c.typing.join(","), online, current == Some(c.id.as_str()))
            })
            .collect();
        if self.ivars().chat.borrow().list_signature == signature {
            return;
        }
        let widgets = self.chat_widgets();
        clear(&widgets.list);
        let mut ids = Vec::new();
        if conversations.is_empty() {
            full_width(&widgets.list, &widgets.list_empty);
        }
        for (index, conversation) in conversations.iter().enumerate() {
            ids.push(conversation.id.clone());
            let selected = current == Some(conversation.id.as_str());
            let (row, inner) = bubble(if selected { 2 } else { 3 }, 7.0, 8.0, mtm);
            let line = stack(false, 10.0, mtm);
            let seed = conversation.members.iter().find(|m| !m.me).map(|m| m.id.clone()).unwrap_or_else(|| conversation.id.clone());
            line.addArrangedSubview(&avatar(&conversation.title, if conversation.kind == ConversationKind::Group { &conversation.id } else { &seed }, 34.0, mtm));
            let texts = stack(true, 2.0, mtm);
            let prefix = match conversation.kind {
                ConversationKind::Group => "👥 ",
                ConversationKind::Direct if conversation.members.iter().any(|m| !m.me && m.online) => "● ",
                ConversationKind::Direct => "",
            };
            let title = strong(&format!("{prefix}{}", conversation.title), 13.0, mtm);
            title.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            let preview_text = if conversation.typing.is_empty() { conversation.last_preview.clone() } else { "écrit…".to_string() };
            let preview = secondary(&preview_text, mtm);
            preview.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            for field in [&title, &preview] {
                field.setContentCompressionResistancePriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
                field.widthAnchor().constraintLessThanOrEqualToConstant(170.0).setActive(true);
            }
            texts.addArrangedSubview(&title);
            texts.addArrangedSubview(&preview);
            line.addArrangedSubview(&texts);
            if conversation.unread > 0 {
                let badge = strong(&conversation.unread.to_string(), 12.0, mtm);
                badge.setTextColor(Some(&NSColor::controlAccentColor()));
                line.addView_inGravity(&badge, NSStackViewGravity::Trailing);
            }
            full_width(&inner, &line);

            // Toute la ligne est cliquable.
            let hit = button("", sel!(chatSelect:), self, mtm);
            hit.setBordered(false);
            hit.setTransparent(true);
            hit.setTag(index as isize);
            row.addSubview(&hit);
            pin(&hit, &row, 0.0, 0.0, 0.0);
            full_width(&widgets.list, &row);
        }
        let mut ui = self.ivars().chat.borrow_mut();
        ui.list_ids = ids;
        ui.list_signature = signature;
    }

    /// Met à jour les lignes de messages (ajouts et changements seulement). Vrai si des messages
    /// ont été ajoutés et qu'il faut défiler en bas.
    fn refresh_messages(&self, messages: &[MessageView], force: bool, mtm: MainThreadMarker) -> bool {
        let widgets = self.chat_widgets();
        let last_mine = messages.iter().rposition(|m| m.mine && !m.system);
        let headers: Vec<bool> = messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                i == 0 || {
                    let previous = &messages[i - 1];
                    previous.system
                        || m.system
                        || previous.author_id != m.author_id
                        || m.sent_ms.saturating_sub(previous.sent_ms) > GROUP_WINDOW_MS
                        || m.reply.is_some()
                }
            })
            .collect();
        let signatures: Vec<String> = messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let attachments: String = m
                    .attachments
                    .iter()
                    .map(|a| format!("{}:{}:{}", a.id, a.path.is_some(), a.progress.map(|p| (p * 20.0) as u32).unwrap_or(0)))
                    .collect();
                let reactions: String = m.reactions.iter().map(|r| format!("{}{}{}", r.emoji, r.count, r.mine)).collect();
                format!(
                    "{}|{}|{}|{}|{:?}|{}|{}|{}|{}",
                    m.text, m.edited, m.deleted, reactions, m.status, attachments, headers[i], Some(i) == last_mine,
                    m.reply.as_ref().map(|r| r.excerpt.as_str()).unwrap_or("")
                )
            })
            .collect();

        let at_bottom = {
            let clip = widgets.scroll.contentView();
            let visible = clip.bounds();
            visible.origin.y + visible.size.height >= widgets.document.frame().size.height - 60.0
        };

        let width = self.bubble_text_width();
        let mut ui = self.ivars().chat.borrow_mut();
        ui.bubble_width = width;
        let previous_count = ui.rows.len();
        let same_prefix = !force
            && ui.rows.len() <= messages.len()
            && ui.rows.iter().zip(messages).all(|(row, message)| row.0 == message.id);
        if !same_prefix {
            clear(&widgets.messages);
            ui.rows.clear();
            ui.reaction_tags.clear();
            ui.attachment_paths.clear();
        }
        for (index, message) in messages.iter().enumerate() {
            if index < ui.rows.len() {
                if ui.rows[index].1 == signatures[index] {
                    continue;
                }
                let view = self.message_row(&mut ui, message, index, headers[index], Some(index) == last_mine, mtm);
                let old = std::mem::replace(&mut ui.rows[index], (message.id.clone(), signatures[index].clone(), view.clone()));
                widgets.messages.removeArrangedSubview(&old.2);
                old.2.removeFromSuperview();
                widgets.messages.insertArrangedSubview_atIndex(&view, index as isize);
                view.widthAnchor().constraintEqualToAnchor(&widgets.messages.widthAnchor()).setActive(true);
            } else {
                let view = self.message_row(&mut ui, message, index, headers[index], Some(index) == last_mine, mtm);
                full_width(&widgets.messages, &view);
                ui.rows.push((message.id.clone(), signatures[index].clone(), view));
            }
        }
        ui.row_messages = messages.to_vec();
        let added = ui.rows.len() > previous_count || !same_prefix;
        let newest_mine = messages.last().is_some_and(|m| m.mine);
        added && (at_bottom || newest_mine || !same_prefix)
    }

    /// Largeur maximale du texte d'une bulle, d'après la place réellement disponible.
    fn bubble_text_width(&self) -> f64 {
        let available = self.chat_widgets().messages.frame().size.width;
        if available < 100.0 {
            return 420.0;
        }
        // avatar 36 + écarts + bouton « … » + marges de la bulle
        (available - 120.0).clamp(220.0, MAX_BUBBLE)
    }

    fn scroll_messages_to_bottom(&self) {
        let widgets = self.chat_widgets();
        widgets.document.layoutSubtreeIfNeeded();
        let clip = widgets.scroll.contentView();
        let height = widgets.document.frame().size.height;
        let visible = clip.bounds().size.height;
        clip.scrollToPoint(NSPoint::new(0.0, (height - visible).max(0.0)));
        widgets.scroll.reflectScrolledClipView(&clip);
    }

    fn message_row(&self, ui: &mut ChatUi, message: &MessageView, index: usize, header: bool, last_mine: bool, mtm: MainThreadMarker) -> Retained<NSView> {
        if message.system {
            let row = stack(false, 0.0, mtm);
            row.setEdgeInsets(NSEdgeInsets { top: 8.0, left: 0.0, bottom: 8.0, right: 0.0 });
            let note = secondary(&format!("— {} · {} —", message.text, time_label(message.sent_ms)), mtm);
            row.addView_inGravity(&note, NSStackViewGravity::Center);
            return Retained::into_super(row);
        }

        let row = stack(false, 10.0, mtm);
        row.setAlignment(NSLayoutAttribute::Top);
        row.setEdgeInsets(NSEdgeInsets { top: if header { 10.0 } else { 1.0 }, left: 0.0, bottom: 1.0, right: 0.0 });
        if header {
            row.addArrangedSubview(&avatar(&message.author_name, &message.author_id, 36.0, mtm));
        } else {
            let gap = NSView::new(mtm);
            gap.setTranslatesAutoresizingMaskIntoConstraints(false);
            gap.widthAnchor().constraintEqualToConstant(36.0).setActive(true);
            gap.heightAnchor().constraintEqualToConstant(1.0).setActive(true);
            row.addArrangedSubview(&gap);
        }

        let column = stack(true, 3.0, mtm);
        if let Some(reply) = &message.reply {
            let quote = secondary(&format!("↪︎ {} : {}", reply.author_name, reply.excerpt), mtm);
            quote.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            quote.widthAnchor().constraintLessThanOrEqualToConstant(ui.bubble_width).setActive(true);
            column.addArrangedSubview(&quote);
        }
        if header {
            let line = stack(false, 8.0, mtm);
            let name = strong(&message.author_name, 13.0, mtm);
            if message.mine {
                name.setTextColor(Some(&NSColor::controlAccentColor()));
            }
            line.addArrangedSubview(&name);
            let when = secondary(&time_label(message.sent_ms), mtm);
            when.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            line.addArrangedSubview(&when);
            column.addArrangedSubview(&line);
        }

        let (body, content) = bubble(if message.mine { 1 } else { 0 }, 7.0, 11.0, mtm);
        let mut has_content = false;
        if message.deleted {
            let gone = secondary("Message supprimé", mtm);
            gone.setFont(Some(&NSFontManager::sharedFontManager(mtm).convertFont_toHaveTrait(&NSFont::systemFontOfSize(12.5), NSFontTraitMask::ItalicFontMask)));
            content.addArrangedSubview(&gone);
            has_content = true;
        } else if !message.text.is_empty() {
            let size = if jumbo(&message.text) { 34.0 } else { 13.5 };
            let text = NSTextField::wrappingLabelWithString(&ns(""), mtm);
            text.setAttributedStringValue(&attributed(&message.text, size, mtm));
            text.setSelectable(true);
            text.setAllowsEditingTextAttributes(true);
            text.setPreferredMaxLayoutWidth(ui.bubble_width);
            content.addArrangedSubview(&text);
            has_content = true;
        }
        for attachment in &message.attachments {
            content.addArrangedSubview(&self.attachment_view(ui, attachment, mtm));
            has_content = true;
        }
        if message.edited && !message.deleted {
            let edited = secondary("(modifié)", mtm);
            edited.setFont(Some(&NSFont::systemFontOfSize(10.5)));
            content.addArrangedSubview(&edited);
        }
        if has_content {
            column.addArrangedSubview(&body);
        }

        if !message.reactions.is_empty() {
            let reactions = stack(false, 4.0, mtm);
            for reaction in &message.reactions {
                let pill = button(&format!("{} {}", reaction.emoji, reaction.count), sel!(chatToggleReaction:), self, mtm);
                pill.setControlSize(NSControlSize::Small);
                pill.setToolTip(Some(&ns(&reaction.names.join(", "))));
                if reaction.mine {
                    pill.setContentTintColor(Some(&NSColor::controlAccentColor()));
                    pill.setFont(Some(&semibold(11.0)));
                }
                pill.setTag(ui.reaction_tags.len() as isize);
                ui.reaction_tags.push((message.id.clone(), reaction.emoji.clone()));
                reactions.addArrangedSubview(&pill);
            }
            column.addArrangedSubview(&reactions);
        }

        if message.mine && (last_mine || message.status == MessageStatus::Sending) {
            let status = match &message.status {
                MessageStatus::Sending => "Envoi… (partira dès que le destinataire est joignable)".to_string(),
                MessageStatus::Delivered => "Distribué".to_string(),
                MessageStatus::Read(names) if names.len() <= 1 => "Lu".to_string(),
                MessageStatus::Read(names) => format!("Lu par {}", names.join(", ")),
                MessageStatus::None => String::new(),
            };
            if !status.is_empty() {
                let label_view = secondary(&status, mtm);
                label_view.setFont(Some(&NSFont::systemFontOfSize(10.5)));
                column.addArrangedSubview(&label_view);
            }
        }
        row.addArrangedSubview(&column);

        let actions = icon_button("ellipsis", "Répondre, réagir, modifier…", sel!(chatMessageMenu:), self, mtm);
        actions.setTag(index as isize);
        row.addView_inGravity(&actions, NSStackViewGravity::Trailing);
        // Clic droit sur le message : même menu.
        unsafe { row.setMenu(Some(&self.message_menu(index, message, mtm))) };
        Retained::into_super(row)
    }

    fn attachment_view(&self, ui: &mut ChatUi, attachment: &magicplus_core::AttachmentView, mtm: MainThreadMarker) -> Retained<NSView> {
        let visual = matches!(attachment.kind, AttachmentKind::Image | AttachmentKind::Gif | AttachmentKind::Sticker);
        if let (true, Some(path)) = (visual, &attachment.path) {
            if let Some(image) = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(&path.to_string_lossy())) {
                let size = image.size();
                let (max_w, max_h) = if attachment.kind == AttachmentKind::Sticker { (150.0, 150.0) } else { (340.0, 260.0) };
                let scale = (max_w / size.width.max(1.0)).min(max_h / size.height.max(1.0)).min(1.0);
                let view = NSImageView::imageViewWithImage(&image, mtm);
                view.setAnimates(true);
                view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
                view.setTranslatesAutoresizingMaskIntoConstraints(false);
                view.widthAnchor().constraintEqualToConstant((size.width * scale).max(24.0)).setActive(true);
                view.heightAnchor().constraintEqualToConstant((size.height * scale).max(24.0)).setActive(true);
                view.setToolTip(Some(&ns(&format!("{} — cliquer pour ouvrir", attachment.file_name))));
                view.setTag(ui.attachment_paths.len() as isize);
                ui.attachment_paths.push(path.clone());
                let click = unsafe {
                    NSClickGestureRecognizer::initWithTarget_action(NSClickGestureRecognizer::alloc(mtm), Some(as_target(self)), Some(sel!(chatOpenAttachment:)))
                };
                view.addGestureRecognizer(&click);
                return Retained::into_super(Retained::into_super(view));
            }
        }
        match &attachment.path {
            Some(path) => {
                let icon = match attachment.kind {
                    AttachmentKind::Video => "🎞️",
                    AttachmentKind::Image | AttachmentKind::Gif | AttachmentKind::Sticker => "🖼️",
                    AttachmentKind::File => "📄",
                };
                let open = button(&format!("{icon} {}  ·  {}", attachment.file_name, format_size(attachment.size)), sel!(chatOpenAttachment:), self, mtm);
                open.setTag(ui.attachment_paths.len() as isize);
                ui.attachment_paths.push(path.clone());
                Retained::into_super(Retained::into_super(open))
            }
            None => {
                let state = match attachment.progress {
                    Some(progress) => format!("⏳ {}  ·  réception {} %", attachment.file_name, (progress * 100.0).round() as u32),
                    None => format!("⏳ {}  ·  {}  ·  en attente", attachment.file_name, format_size(attachment.size)),
                };
                Retained::into_super(Retained::into_super(secondary(&state, mtm)))
            }
        }
    }

    fn message_menu(&self, index: usize, message: &MessageView, mtm: MainThreadMarker) -> Retained<NSMenu> {
        let menu = NSMenu::new(mtm);
        let add = |title: &str, action: Sel, tag: isize| {
            let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), Some(action), &ns("")) };
            unsafe { item.setTarget(Some(as_target(self))) };
            item.setTag(tag);
            menu.addItem(&item);
        };
        if !message.deleted {
            add("Répondre", sel!(chatReply:), index as isize);
            for (k, emoji) in QUICK_REACTIONS.iter().enumerate() {
                add(&format!("Réagir {emoji}"), sel!(chatReact:), (index * 10 + k) as isize);
            }
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            if !message.text.is_empty() {
                add("Copier le texte", sel!(chatCopyMessage:), index as isize);
            }
            if message.mine {
                add("Modifier", sel!(chatEdit:), index as isize);
                add("Supprimer pour tout le monde", sel!(chatDelete:), index as isize);
            }
        }
        menu
    }

    fn message_at(&self, index: isize) -> Option<MessageView> {
        self.ivars().chat.borrow().row_messages.get(index.max(0) as usize).cloned()
    }

    fn current_conversation_id(&self) -> Option<String> {
        self.ivars().chat.borrow().current.clone()
    }

    fn update_banner(&self) {
        let widgets = self.chat_widgets();
        let ui = self.ivars().chat.borrow();
        let text = if ui.editing.is_some() {
            Some("✏️ Modification du message — Entrée pour enregistrer, Échap pour annuler".to_string())
        } else {
            ui.reply_to.as_ref().map(|(_, author, excerpt)| format!("↪︎ Réponse à {author} : {excerpt}"))
        };
        match text {
            Some(text) => {
                widgets.banner_label.setStringValue(&ns(&text));
                widgets.banner.setHidden(false);
            }
            None => widgets.banner.setHidden(true),
        }
    }

    fn update_pending(&self) {
        let mtm = self.mtm();
        let widgets = self.chat_widgets();
        clear(&widgets.pending);
        let files = self.ivars().chat.borrow().pending.clone();
        for (index, path) in files.iter().enumerate() {
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let chip = button(&format!("📎 {name}  ✕"), sel!(chatRemovePending:), self, mtm);
            chip.setControlSize(NSControlSize::Small);
            chip.setToolTip(Some(&ns("Retirer cette pièce jointe")));
            chip.setTag(index as isize);
            widgets.pending.addArrangedSubview(&chip);
        }
        widgets.pending.setHidden(files.is_empty());
    }

    fn focus_input(&self) {
        let widgets = self.chat_widgets();
        if let Some(window) = widgets.input.window() {
            let responder: &NSResponder = &widgets.input;
            window.makeFirstResponder(Some(responder));
        }
    }

    pub(super) fn chat_select(&self, index: isize) {
        let id = self.ivars().chat.borrow().list_ids.get(index.max(0) as usize).cloned();
        if let Some(id) = id {
            self.open_conversation(&id);
        }
    }

    pub(super) fn open_conversation(&self, id: &str) {
        {
            let mut ui = self.ivars().chat.borrow_mut();
            if ui.current.as_deref() != Some(id) {
                ui.current = Some(id.to_string());
                ui.reply_to = None;
                ui.editing = None;
                ui.pending.clear();
            }
            ui.list_signature.clear();
        }
        self.chat_widgets().input.setString(&ns(""));
        self.update_banner();
        self.update_pending();
        self.refresh_chat(true);
        self.scroll_messages_to_bottom();
        self.focus_input();
    }

    pub(super) fn chat_send(&self) {
        let Some(conversation) = self.current_conversation_id() else { return };
        let engine = self.engine();
        let widgets = self.chat_widgets();
        let text = widgets.input.string().to_string();
        let (editing, reply, files) = {
            let ui = self.ivars().chat.borrow();
            (ui.editing.clone(), ui.reply_to.as_ref().map(|r| r.0.clone()), ui.pending.clone())
        };
        let result = match editing {
            Some(message) => engine.edit_chat(&conversation, &message, &text),
            None => engine.send_chat(&conversation, &text, reply.as_deref(), &files),
        };
        match result {
            Ok(()) => {
                widgets.input.setString(&ns(""));
                let mut ui = self.ivars().chat.borrow_mut();
                ui.editing = None;
                ui.reply_to = None;
                ui.pending.clear();
                drop(ui);
                self.update_banner();
                self.update_pending();
                self.refresh_chat(false);
                self.scroll_messages_to_bottom();
            }
            Err(error) => engine.set_status(format!("Message non envoyé : {error}")),
        }
    }

    pub(super) fn chat_cancel_banner(&self) -> bool {
        let mut ui = self.ivars().chat.borrow_mut();
        let active = ui.editing.is_some() || ui.reply_to.is_some();
        let was_editing = ui.editing.take().is_some();
        ui.reply_to = None;
        drop(ui);
        if was_editing {
            self.chat_widgets().input.setString(&ns(""));
        }
        self.update_banner();
        active
    }

    /// Touches spéciales de la zone de saisie : Entrée envoie, Maj+Entrée va à la ligne, Échap annule.
    pub(super) fn chat_command(&self, selector: Sel) -> bool {
        if selector == sel!(insertNewline:) {
            let shift = NSApplication::sharedApplication(self.mtm())
                .currentEvent()
                .is_some_and(|event| event.modifierFlags().contains(NSEventModifierFlags::Shift));
            if !shift {
                self.chat_send();
                return true;
            }
        }
        if selector == sel!(cancelOperation:) {
            return self.chat_cancel_banner();
        }
        false
    }

    pub(super) fn chat_text_changed(&self) {
        if let Some(conversation) = self.current_conversation_id() {
            if !self.chat_widgets().input.string().to_string().trim().is_empty() {
                self.engine().notify_typing(&conversation);
            }
        }
    }

    pub(super) fn chat_reply(&self, index: isize) {
        if let Some(message) = self.message_at(index) {
            let excerpt: String = markdown::plain(&message.text).chars().take(80).collect();
            let excerpt = if excerpt.is_empty() { "pièce jointe".to_string() } else { excerpt };
            let mut ui = self.ivars().chat.borrow_mut();
            ui.editing = None;
            ui.reply_to = Some((message.id, message.author_name, excerpt));
            drop(ui);
            self.update_banner();
            self.focus_input();
        }
    }

    pub(super) fn chat_edit(&self, index: isize) {
        if let Some(message) = self.message_at(index) {
            self.chat_widgets().input.setString(&ns(&message.text));
            let mut ui = self.ivars().chat.borrow_mut();
            ui.reply_to = None;
            ui.editing = Some(message.id);
            drop(ui);
            self.update_banner();
            self.focus_input();
        }
    }

    pub(super) fn chat_delete(&self, index: isize) {
        let (Some(message), Some(conversation)) = (self.message_at(index), self.current_conversation_id()) else { return };
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&ns("Supprimer ce message ?"));
        alert.setInformativeText(&ns("Il disparaîtra aussi chez les autres membres de la discussion."));
        alert.addButtonWithTitle(&ns("Supprimer"));
        alert.addButtonWithTitle(&ns("Annuler"));
        if alert.runModal() == NSAlertFirstButtonReturn {
            if let Err(error) = self.engine().delete_chat(&conversation, &message.id) {
                self.engine().set_status(error.to_string());
            }
        }
    }

    pub(super) fn chat_copy_message(&self, index: isize) {
        if let Some(message) = self.message_at(index) {
            self.copy_text(&markdown::plain(&message.text));
        }
    }

    pub(super) fn chat_react(&self, tag: isize) {
        let tag = tag.max(0) as usize;
        let (Some(message), Some(conversation)) = (self.message_at((tag / 10) as isize), self.current_conversation_id()) else { return };
        if let Some(emoji) = QUICK_REACTIONS.get(tag % 10) {
            if let Err(error) = self.engine().toggle_reaction(&conversation, &message.id, emoji) {
                self.engine().set_status(error.to_string());
            }
        }
    }

    pub(super) fn chat_toggle_reaction(&self, tag: isize) {
        let target = self.ivars().chat.borrow().reaction_tags.get(tag.max(0) as usize).cloned();
        if let (Some((message, emoji)), Some(conversation)) = (target, self.current_conversation_id()) {
            if let Err(error) = self.engine().toggle_reaction(&conversation, &message, &emoji) {
                self.engine().set_status(error.to_string());
            }
        }
    }

    pub(super) fn chat_open_attachment(&self, tag: isize) {
        let path = self.ivars().chat.borrow().attachment_paths.get(tag.max(0) as usize).cloned();
        if let Some(path) = path {
            open_path(&path.to_string_lossy());
        }
    }

    pub(super) fn chat_attach(&self) {
        if let Some(files) = rfd::FileDialog::new().set_title("Joindre des fichiers").pick_files() {
            self.add_pending(files);
        }
    }

    fn add_pending(&self, files: Vec<PathBuf>) {
        let mut ui = self.ivars().chat.borrow_mut();
        for file in files {
            if ui.pending.len() >= magicplus_core::protocol::MAX_ATTACHMENTS {
                self.engine().set_status("15 pièces jointes au plus par message");
                break;
            }
            if !ui.pending.contains(&file) {
                ui.pending.push(file);
            }
        }
        drop(ui);
        self.update_pending();
        self.focus_input();
    }

    pub(super) fn chat_remove_pending(&self, index: isize) {
        let mut ui = self.ivars().chat.borrow_mut();
        let index = index.max(0) as usize;
        if index < ui.pending.len() {
            ui.pending.remove(index);
        }
        drop(ui);
        self.update_pending();
    }

    pub(super) fn chat_emoji(&self) {
        self.focus_input();
        NSApplication::sharedApplication(self.mtm()).orderFrontCharacterPalette(None);
    }

    /// Cmd+V : dans la zone de saisie, une image ou des fichiers copiés deviennent des pièces jointes.
    pub(super) fn smart_paste(&self, sender: Option<&AnyObject>) {
        let mtm = self.mtm();
        let in_composer = self.chat_visible()
            && self.ivars().chat.borrow().current.is_some()
            && self.chat_widgets().input.window().and_then(|w| w.firstResponder()).is_some_and(|responder| {
                let input: &AnyObject = &self.chat_widgets().input;
                responder.isEqual(Some(input))
            });
        if in_composer {
            let pasteboard = NSPasteboard::generalPasteboard();
            let types: Vec<String> = pasteboard
                .types()
                .map(|types| types.iter().map(|t| t.to_string()).collect())
                .unwrap_or_default();
            if types.iter().any(|t| t == "public.file-url") {
                let files: Vec<PathBuf> = pasteboard
                    .pasteboardItems()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.stringForType(&ns("public.file-url")))
                            .filter_map(|url| NSURL::URLWithString(&url))
                            .filter_map(|url| url.path())
                            .map(|path| PathBuf::from(path.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                if !files.is_empty() {
                    self.add_pending(files);
                    return;
                }
            }
            if types.iter().any(|t| t == "public.png" || t == "public.tiff" || t == "public.jpeg") {
                match self.engine().clipboard_image_to_file() {
                    Ok(path) => {
                        self.add_pending(vec![path]);
                        return;
                    }
                    Err(error) => self.engine().set_status(error.to_string()),
                }
            }
        }
        unsafe {
            NSApplication::sharedApplication(mtm).sendAction_to_from(sel!(paste:), None, sender);
        }
    }

    // ---------------------------------------------------------------- menus et boîtes de dialogue

    fn popup(&self, menu: &NSMenu, anchor: &NSView) {
        let height = anchor.frame().size.height;
        menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0.0, height + 4.0), Some(anchor));
    }

    fn menu_item(&self, title: &str, action: Option<Sel>, tag: isize) -> Retained<NSMenuItem> {
        let mtm = self.mtm();
        let item = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), action, &ns("")) };
        unsafe { item.setTarget(Some(as_target(self))) };
        item.setTag(tag);
        if action.is_none() {
            item.setEnabled(false);
        }
        item
    }

    pub(super) fn chat_message_menu(&self, sender: &NSButton) {
        if let Some(message) = self.message_at(sender.tag()) {
            let menu = self.message_menu(sender.tag() as usize, &message, self.mtm());
            self.popup(&menu, sender);
        }
    }

    pub(super) fn chat_new_menu(&self, sender: &NSButton) {
        let menu = NSMenu::new(self.mtm());
        menu.addItem(&self.menu_item("Nouveau groupe…", Some(sel!(chatNewGroup:)), 0));
        menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        let trusted: Vec<(String, String)> = self
            .engine()
            .snapshot()
            .peers
            .into_iter()
            .filter(|p| p.trusted)
            .map(|p| (p.id, p.name))
            .collect();
        if trusted.is_empty() {
            menu.addItem(&self.menu_item("Associez d'abord un appareil (page Appareils)", None, 0));
        }
        for (index, (_, name)) in trusted.iter().enumerate() {
            menu.addItem(&self.menu_item(&format!("Discuter avec {name}"), Some(sel!(chatOpenDirect:)), index as isize));
        }
        self.ivars().chat.borrow_mut().direct_choices = trusted.into_iter().map(|(id, _)| id).collect();
        self.popup(&menu, sender);
    }

    pub(super) fn chat_open_direct(&self, index: isize) {
        let id = self.ivars().chat.borrow().direct_choices.get(index.max(0) as usize).cloned();
        if let Some(peer) = id {
            self.chat_with(&peer);
        }
    }

    /// Ouvre la discussion avec un appareil associé (depuis le menu ou la page Appareils).
    pub(super) fn chat_with(&self, peer: &str) {
        match self.engine().open_direct(peer) {
            Ok(conversation) => {
                self.show_page(CHAT_PAGE);
                self.open_conversation(&conversation);
            }
            Err(error) => self.engine().set_status(error.to_string()),
        }
    }

    /// Boîte de dialogue : nom (facultatif) + cases à cocher pour des appareils associés.
    fn choose_devices(&self, title: &str, info: &str, name: Option<&str>, exclude: &[String]) -> Option<(String, Vec<String>)> {
        let mtm = self.mtm();
        let candidates: Vec<(String, String)> = self
            .engine()
            .snapshot()
            .peers
            .into_iter()
            .filter(|p| p.trusted && !exclude.contains(&p.id))
            .map(|p| (p.id, p.name))
            .collect();
        if candidates.is_empty() {
            self.engine().set_status("Aucun autre appareil associé à ajouter : associez-en un dans la page Appareils");
            return None;
        }
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&ns(title));
        alert.setInformativeText(&ns(info));
        let accessory = stack(true, 6.0, mtm);
        let name_field = NSTextField::textFieldWithString(&ns(name.unwrap_or("")), mtm);
        name_field.setPlaceholderString(Some(&ns("Nom du groupe")));
        name_field.setTranslatesAutoresizingMaskIntoConstraints(false);
        name_field.widthAnchor().constraintEqualToConstant(300.0).setActive(true);
        if name.is_some() {
            accessory.addArrangedSubview(&name_field);
        }
        let boxes: Vec<Retained<NSButton>> = candidates
            .iter()
            .map(|(_, label_text)| unsafe { NSButton::checkboxWithTitle_target_action(&ns(label_text), None, None, mtm) })
            .collect();
        for checkbox in &boxes {
            accessory.addArrangedSubview(checkbox);
        }
        accessory.setTranslatesAutoresizingMaskIntoConstraints(true);
        let size = accessory.fittingSize();
        accessory.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(size.width.max(300.0), size.height)));
        alert.setAccessoryView(Some(&accessory));
        alert.addButtonWithTitle(&ns("Valider"));
        alert.addButtonWithTitle(&ns("Annuler"));
        if alert.runModal() != NSAlertFirstButtonReturn {
            return None;
        }
        let chosen: Vec<String> = candidates
            .iter()
            .zip(&boxes)
            .filter(|(_, checkbox)| checkbox.state() == NSControlStateValueOn)
            .map(|((id, _), _)| id.clone())
            .collect();
        Some((name_field.stringValue().to_string(), chosen))
    }

    pub(super) fn chat_new_group(&self) {
        let info = "Choisissez des appareils que vous avez associés. MagicPlus présente leurs clés aux autres membres : \
                    ils pourront se parler dans ce groupe sans s'associer entre eux.";
        if let Some((name, members)) = self.choose_devices("Nouveau groupe", info, Some(""), &[]) {
            match self.engine().create_group(&name, &members) {
                Ok(conversation) => self.open_conversation(&conversation),
                Err(error) => self.engine().set_status(error.to_string()),
            }
        }
    }

    pub(super) fn chat_group_menu(&self, sender: &NSButton) {
        let Some(conversation) = self.ivars().chat.borrow().current_conversation.clone() else { return };
        let menu = NSMenu::new(self.mtm());
        if conversation.is_creator && !conversation.closed {
            menu.addItem(&self.menu_item("Ajouter des membres…", Some(sel!(chatAddMembers:)), 0));
            menu.addItem(&self.menu_item("Renommer le groupe…", Some(sel!(chatRename:)), 0));
            menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
        }
        for member in &conversation.members {
            let state = if member.me { "vous" } else if member.online { "en ligne" } else { "hors ligne" };
            menu.addItem(&self.menu_item(&format!("{} — {state}", member.name), None, 0));
        }
        if !conversation.closed {
            menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
            menu.addItem(&self.menu_item("Quitter le groupe", Some(sel!(chatLeave:)), 0));
        }
        self.popup(&menu, sender);
    }

    pub(super) fn chat_add_members(&self) {
        let Some(conversation) = self.ivars().chat.borrow().current_conversation.clone() else { return };
        let existing: Vec<String> = conversation.members.iter().map(|m| m.id.clone()).collect();
        let info = "Les nouveaux membres reçoivent l'historique du groupe et les clés des autres membres.";
        if let Some((_, members)) = self.choose_devices("Ajouter des membres", info, None, &existing) {
            if let Err(error) = self.engine().add_group_members(&conversation.id, &members) {
                self.engine().set_status(error.to_string());
            }
        }
    }

    pub(super) fn chat_rename(&self) {
        let mtm = self.mtm();
        let Some(conversation) = self.ivars().chat.borrow().current_conversation.clone() else { return };
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&ns("Renommer le groupe"));
        let field = NSTextField::textFieldWithString(&ns(&conversation.title), mtm);
        field.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 24.0)));
        alert.setAccessoryView(Some(&field));
        alert.addButtonWithTitle(&ns("Renommer"));
        alert.addButtonWithTitle(&ns("Annuler"));
        if alert.runModal() == NSAlertFirstButtonReturn {
            if let Err(error) = self.engine().rename_group(&conversation.id, &field.stringValue().to_string()) {
                self.engine().set_status(error.to_string());
            }
        }
    }

    pub(super) fn chat_leave(&self) {
        let Some(conversation) = self.current_conversation_id() else { return };
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&ns("Quitter ce groupe ?"));
        alert.setInformativeText(&ns("Vous ne recevrez plus ses messages. L'historique reste consultable sur cet appareil."));
        alert.addButtonWithTitle(&ns("Quitter"));
        alert.addButtonWithTitle(&ns("Annuler"));
        if alert.runModal() == NSAlertFirstButtonReturn {
            if let Err(error) = self.engine().leave_group(&conversation) {
                self.engine().set_status(error.to_string());
            }
        }
    }

    pub(super) fn chat_stickers_menu(&self) {
        let mtm = self.mtm();
        let stickers = self.engine().stickers();
        let menu = NSMenu::new(mtm);
        if stickers.is_empty() {
            menu.addItem(&self.menu_item("Aucun autocollant pour l'instant", None, 0));
        }
        let thumbnail = |path: &std::path::Path| {
            NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(&path.to_string_lossy())).inspect(|image| image.setSize(NSSize::new(56.0, 56.0)))
        };
        for (index, sticker) in stickers.iter().take(60).enumerate() {
            let item = self.menu_item(if sticker.animated { "GIF" } else { "Autocollant" }, Some(sel!(chatSendSticker:)), index as isize);
            if let Some(image) = thumbnail(&sticker.path) {
                item.setImage(Some(&image));
            }
            menu.addItem(&item);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&self.menu_item("Créer un autocollant ou ajouter un GIF…", Some(sel!(chatAddSticker:)), 0));
        if !stickers.is_empty() {
            let remove = NSMenu::new(mtm);
            for (index, sticker) in stickers.iter().take(60).enumerate() {
                let item = self.menu_item(if sticker.animated { "GIF" } else { "Autocollant" }, Some(sel!(chatRemoveSticker:)), index as isize);
                if let Some(image) = thumbnail(&sticker.path) {
                    item.setImage(Some(&image));
                }
                remove.addItem(&item);
            }
            let entry = self.menu_item("Retirer de la bibliothèque", Some(sel!(chatNoop:)), 0);
            entry.setSubmenu(Some(&remove));
            menu.addItem(&entry);
        }
        self.ivars().chat.borrow_mut().sticker_ids = stickers.into_iter().map(|s| s.id).collect();
        let anchor: Retained<NSButton> = self.chat_widgets().stickers_button.clone();
        self.popup(&menu, &anchor);
    }

    pub(super) fn chat_send_sticker(&self, index: isize) {
        let id = self.ivars().chat.borrow().sticker_ids.get(index.max(0) as usize).cloned();
        if let (Some(id), Some(conversation)) = (id, self.current_conversation_id()) {
            if let Err(error) = self.engine().send_sticker(&conversation, &id) {
                self.engine().set_status(error.to_string());
            }
        }
    }

    pub(super) fn chat_add_sticker(&self) {
        let picked = rfd::FileDialog::new()
            .set_title("Image ou GIF pour un autocollant")
            .add_filter("Images et GIF", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
            .pick_file();
        if let Some(path) = picked {
            match self.engine().add_sticker(&path) {
                Ok(_) => self.engine().set_status("Autocollant ajouté à votre bibliothèque"),
                Err(error) => self.engine().set_status(error.to_string()),
            }
        }
    }

    pub(super) fn chat_remove_sticker(&self, index: isize) {
        let id = self.ivars().chat.borrow().sticker_ids.get(index.max(0) as usize).cloned();
        if let Some(id) = id {
            self.engine().remove_sticker(&id);
        }
    }
}

/// La vue cliquée par un geste (image jointe), pour retrouver son numéro.
pub(super) fn gesture_tag(recognizer: &NSClickGestureRecognizer) -> Option<isize> {
    let view = recognizer.view()?;
    let image = view.downcast::<NSImageView>().ok()?;
    Some(image.tag())
}
