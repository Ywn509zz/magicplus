//! Menus de l'interface Mac, dessinés par MagicPlus (bulles surgissantes) au lieu des menus du
//! système : icônes, survol, actions dangereuses en rouge, rangée de réactions, grilles d'émojis
//! et d'autocollants, choix de la conversation où transférer, messages épinglés.

use super::chat_page::QUICK_REACTIONS;
use super::*;
use crate::shared::time_label;
use magicplus_core::{markdown, ConversationKind, FriendState};
use objc2::{AnyThread, Message};
use objc2_app_kit::{NSBezelStyle, NSPopover, NSPopoverBehavior, NSViewController};
use objc2_foundation::NSRectEdge;

/// Ce que fait un élément de menu.
pub(super) type Action = Box<dyn Fn(&Controller)>;

pub(super) enum Entry {
    /// Icône (symbole SF), titre, rouge ?, action.
    Item(&'static str, String, bool, Action),
    Separator,
    /// Texte discret (titre de section, explication).
    Note(String),
    /// Rangée d'émojis (réactions rapides) et leur action.
    Emojis(Vec<(String, Action)>),
}

const MENU_WIDTH: f64 = 250.0;

/// Émojis proposés pour réagir (en plus de « Autre émoji… »).
const REACTION_EMOJIS: [&str; 48] = [
    "👍", "👎", "❤️", "🔥", "😂", "🤣", "😮", "😢", "😭", "😡", "🎉", "🙏", "👏", "🙌", "💪", "👀", "✅", "❌", "⭐", "💯", "🤔", "😍", "🥰",
    "😎", "🤩", "🥳", "😴", "🤯", "😱", "🤗", "😇", "😅", "🙃", "😬", "🫡", "🤝", "👋", "✌️", "🤞", "💡", "📌", "🚀", "🍕", "☕", "🎵", "🎮",
    "🌞", "🌧️",
];

impl Controller {
    /// Montre un menu stylisé à côté de `anchor`.
    pub(super) fn show_actions(&self, anchor: &NSView, entries: Vec<Entry>) {
        let mtm = self.mtm();
        let column = stack(true, 1.0, mtm);
        column.setEdgeInsets(NSEdgeInsets { top: 8.0, left: 6.0, bottom: 8.0, right: 6.0 });
        let mut actions: Vec<Action> = Vec::new();
        let mut add_button = |title: &str, symbol: Option<&str>, action: Action| -> Retained<NSButton> {
            let tag = actions.len() as isize;
            actions.push(action);
            let image = symbol.and_then(|s| NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(s), None));
            let item = match &image {
                Some(image) => unsafe { NSButton::buttonWithTitle_image_target_action(&ns(title), image, Some(as_target(self)), Some(sel!(popoverAction:)), mtm) },
                None => button(title, sel!(popoverAction:), self, mtm),
            };
            item.setTag(tag);
            item
        };
        for entry in entries {
            match entry {
                Entry::Item(symbol, title, danger, action) => {
                    let item = add_button(&format!("  {title}"), Some(symbol), action);
                    item.setBezelStyle(NSBezelStyle::AccessoryBar);
                    item.setShowsBorderOnlyWhileMouseInside(true);
                    item.setImagePosition(NSCellImagePosition::ImageLeft);
                    item.setAlignment(NSTextAlignment::Left);
                    item.setFont(Some(&NSFont::systemFontOfSize(13.0)));
                    if danger {
                        item.setContentTintColor(Some(&NSColor::systemRedColor()));
                    }
                    item.setTranslatesAutoresizingMaskIntoConstraints(false);
                    item.heightAnchor().constraintEqualToConstant(28.0).setActive(true);
                    full_width(&column, &item);
                }
                Entry::Separator => {
                    let line = NSBox::new(mtm);
                    line.setBoxType(NSBoxType::Separator);
                    line.setTranslatesAutoresizingMaskIntoConstraints(false);
                    full_width(&column, &line);
                }
                Entry::Note(text) => {
                    let note = secondary(&text, mtm);
                    note.setFont(Some(&semibold(11.0)));
                    column.addArrangedSubview(&note);
                }
                Entry::Emojis(emojis) => {
                    let grid = stack(true, 2.0, mtm);
                    let mut row = stack(false, 2.0, mtm);
                    for (k, (emoji, action)) in emojis.into_iter().enumerate() {
                        if k > 0 && k % 8 == 0 {
                            grid.addArrangedSubview(&row);
                            row = stack(false, 2.0, mtm);
                        }
                        let item = add_button(&emoji, None, action);
                        item.setBezelStyle(NSBezelStyle::AccessoryBar);
                        item.setShowsBorderOnlyWhileMouseInside(true);
                        item.setFont(Some(&NSFont::systemFontOfSize(19.0)));
                        item.setTranslatesAutoresizingMaskIntoConstraints(false);
                        item.widthAnchor().constraintEqualToConstant(26.0).setActive(true);
                        item.heightAnchor().constraintEqualToConstant(30.0).setActive(true);
                        row.addArrangedSubview(&item);
                    }
                    grid.addArrangedSubview(&row);
                    full_width(&column, &grid);
                }
            }
        }
        self.present_popover(&column, anchor, actions);
    }

    /// Affiche `content` dans une bulle surgissante ; `actions` : boutons `popoverAction:`.
    pub(super) fn present_popover(&self, content: &NSStackView, anchor: &NSView, actions: Vec<Action>) {
        let mtm = self.mtm();
        self.close_popover();
        content.widthAnchor().constraintEqualToConstant(MENU_WIDTH).setActive(true);
        content.layoutSubtreeIfNeeded();
        let size = content.fittingSize();
        content.setFrameSize(NSSize::new(MENU_WIDTH, size.height));
        let controller = NSViewController::new(mtm);
        controller.setView(content);
        let popover = NSPopover::new(mtm);
        popover.setBehavior(NSPopoverBehavior::Transient);
        popover.setContentViewController(Some(&controller));
        popover.setContentSize(NSSize::new(MENU_WIDTH, size.height));
        popover.showRelativeToRect_ofView_preferredEdge(anchor.bounds(), anchor, NSRectEdge::MaxY);
        *self.ivars().popover_actions.borrow_mut() = actions;
        *self.ivars().popover.borrow_mut() = Some(popover);
    }

    pub(super) fn close_popover(&self) {
        if let Some(popover) = self.ivars().popover.borrow_mut().take() {
            popover.close();
        }
    }

    /// Un bouton d'une bulle a été cliqué : on la ferme, puis on exécute son action.
    pub(super) fn popover_action(&self, tag: isize) {
        let actions = std::mem::take(&mut *self.ivars().popover_actions.borrow_mut());
        self.close_popover();
        if let Some(action) = actions.get(tag.max(0) as usize) {
            action(self);
        }
    }

    // ---------------------------------------------------------------- messages

    /// Actions d'un message (bouton « … » ou clic droit).
    pub(super) fn message_actions(&self, index: isize, anchor: &NSView) {
        let Some(message) = self.message_at(index) else { return };
        let closed = self.ivars().chat.borrow().current_closed();
        let mut entries = Vec::new();
        if !message.deleted && !closed {
            let mut emojis: Vec<(String, Action)> = QUICK_REACTIONS
                .iter()
                .enumerate()
                .map(|(k, emoji)| ((*emoji).to_string(), Box::new(move |c: &Controller| c.chat_react(index * 10 + k as isize)) as Action))
                .collect();
            emojis.push(("➕".to_string(), Box::new(move |c: &Controller| c.reaction_picker(index))));
            entries.push(Entry::Emojis(emojis));
            entries.push(Entry::Separator);
            entries.push(Entry::Item("arrowshape.turn.up.left", "Répondre".into(), false, Box::new(move |c| c.chat_reply(index))));
            entries.push(Entry::Item("arrowshape.turn.up.right", "Transférer…".into(), false, Box::new(move |c| c.forward_picker(index))));
            let (symbol, title) = if message.pinned_by.is_some() { ("pin.slash", "Désépingler") } else { ("pin", "Épingler") };
            let pin = message.pinned_by.is_none();
            entries.push(Entry::Item(symbol, title.into(), false, Box::new(move |c| c.chat_pin(index, pin))));
        }
        if !message.deleted {
            if !message.text.is_empty() {
                entries.push(Entry::Item("doc.on.doc", "Copier le texte".into(), false, Box::new(move |c| c.chat_copy_message(index))));
            }
            if super::markdown_view::code_of(&message.text).is_some() {
                entries.push(Entry::Item("chevron.left.forwardslash.chevron.right", "Copier le code".into(), false, Box::new(move |c| c.chat_copy_code(index))));
            }
            if message.mine && !closed {
                entries.push(Entry::Separator);
                if !message.text.is_empty() {
                    entries.push(Entry::Item("pencil", "Modifier".into(), false, Box::new(move |c| c.chat_edit(index))));
                }
                entries.push(Entry::Item("trash", "Supprimer pour tout le monde".into(), true, Box::new(move |c| c.chat_delete(index))));
            }
        }
        if !entries.is_empty() {
            self.show_actions(anchor, entries);
        }
    }

    /// Grille d'émojis pour réagir, et champ pour en taper (ou coller) n'importe quel autre.
    pub(super) fn reaction_picker(&self, index: isize) {
        let mtm = self.mtm();
        let Some(anchor) = self.ivars().chat.borrow().row_view(index) else { return };
        let mut actions: Vec<Action> = Vec::new();
        let column = stack(true, 6.0, mtm);
        column.setEdgeInsets(NSEdgeInsets { top: 10.0, left: 8.0, bottom: 10.0, right: 8.0 });
        let title = secondary("Réagir avec…", mtm);
        title.setFont(Some(&semibold(11.0)));
        column.addArrangedSubview(&title);
        for chunk in REACTION_EMOJIS.chunks(8) {
            let row = stack(false, 2.0, mtm);
            for emoji in chunk {
                let tag = actions.len() as isize;
                let emoji_text = (*emoji).to_string();
                actions.push(Box::new(move |c: &Controller| c.chat_react_with(index, &emoji_text)));
                let item = button(emoji, sel!(popoverAction:), self, mtm);
                item.setTag(tag);
                item.setBezelStyle(NSBezelStyle::AccessoryBar);
                item.setShowsBorderOnlyWhileMouseInside(true);
                item.setFont(Some(&NSFont::systemFontOfSize(19.0)));
                item.setTranslatesAutoresizingMaskIntoConstraints(false);
                item.widthAnchor().constraintEqualToConstant(26.0).setActive(true);
                item.heightAnchor().constraintEqualToConstant(30.0).setActive(true);
                row.addArrangedSubview(&item);
            }
            column.addArrangedSubview(&row);
        }
        let other = stack(false, 6.0, mtm);
        let field = NSTextField::textFieldWithString(&ns(""), mtm);
        field.setPlaceholderString(Some(&ns("Autre émoji (Ctrl+Cmd+Espace)")));
        field.setTranslatesAutoresizingMaskIntoConstraints(false);
        field.widthAnchor().constraintEqualToConstant(160.0).setActive(true);
        other.addArrangedSubview(&field);
        let tag = actions.len() as isize;
        let typed = field.clone();
        actions.push(Box::new(move |c: &Controller| {
            let emoji = typed.stringValue().to_string();
            let emoji = emoji.trim();
            if !emoji.is_empty() {
                c.chat_react_with(index, emoji);
            }
        }));
        let react = button("Réagir", sel!(popoverAction:), self, mtm);
        react.setTag(tag);
        other.addArrangedSubview(&react);
        column.addArrangedSubview(&other);
        self.present_popover(&column, &anchor, actions);
    }

    /// Choix de la conversation où transférer un message.
    pub(super) fn forward_picker(&self, index: isize) {
        let Some(anchor) = self.ivars().chat.borrow().row_view(index) else { return };
        let Some(message) = self.message_at(index) else { return };
        let current = self.current_conversation_id();
        let mut entries = vec![Entry::Note("TRANSFÉRER VERS".into())];
        let conversations: Vec<_> = self.engine().conversations().into_iter().filter(|c| Some(&c.id) != current.as_ref() && !c.closed).collect();
        if conversations.is_empty() {
            entries.push(Entry::Note("Aucune autre discussion : créez-en une avec le crayon.".into()));
        }
        for conversation in conversations {
            let symbol = if conversation.kind == ConversationKind::Group { "person.3" } else { "person.crop.circle" };
            let (from, id, to) = (current.clone().unwrap_or_default(), message.id.clone(), conversation.id.clone());
            entries.push(Entry::Item(
                symbol,
                conversation.title.clone(),
                false,
                Box::new(move |c: &Controller| match c.engine().forward_chat(&from, &id, &to) {
                    Ok(()) => c.engine().set_status("Message transféré"),
                    Err(error) => c.engine().set_status(format!("Transfert impossible : {error}")),
                }),
            ));
        }
        self.show_actions(&anchor, entries);
    }

    /// Messages épinglés de la conversation (bouton de l'en-tête).
    pub(super) fn pins_popover(&self, anchor: &NSView) {
        let Some(conversation) = self.current_conversation_id() else { return };
        let pinned = self.engine().pinned_messages(&conversation);
        let mut entries = vec![Entry::Note(format!("MESSAGES ÉPINGLÉS ({})", pinned.len()))];
        if pinned.is_empty() {
            entries.push(Entry::Note("Aucun pour l'instant : « Épingler » dans le menu d'un message.".into()));
        }
        for message in pinned {
            let text: String = markdown::plain(&message.text).chars().take(70).collect();
            let text = if text.is_empty() { "pièce jointe".to_string() } else { text };
            entries.push(Entry::Note(format!("{} · {}", message.author_name, time_label(message.sent_ms))));
            let (conversation, id) = (conversation.clone(), message.id.clone());
            entries.push(Entry::Item(
                "pin.slash",
                text,
                false,
                Box::new(move |c: &Controller| {
                    if let Err(error) = c.engine().pin_chat(&conversation, &id, false) {
                        c.engine().set_status(error.to_string());
                    }
                }),
            ));
        }
        self.show_actions(anchor, entries);
    }

    // ---------------------------------------------------------------- autres menus

    pub(super) fn new_conversation_popover(&self, anchor: &NSView) {
        let mut entries = vec![Entry::Item("person.3", "Nouveau groupe…".into(), false, Box::new(|c: &Controller| c.chat_new_group())), Entry::Separator];
        let trusted: Vec<(String, String)> = self.engine().snapshot().peers.into_iter().filter(|p| p.trusted).map(|p| (p.id, p.name)).collect();
        if trusted.is_empty() {
            entries.push(Entry::Note("Associez d'abord un appareil (page Appareils).".into()));
        }
        for (id, name) in trusted {
            entries.push(Entry::Item("bubble.left", format!("Discuter avec {name}"), false, Box::new(move |c: &Controller| c.chat_with(&id))));
        }
        self.show_actions(anchor, entries);
    }

    pub(super) fn group_popover(&self, anchor: &NSView) {
        let Some(conversation) = self.ivars().chat.borrow().current_view() else { return };
        let mut entries = Vec::new();
        if !conversation.closed {
            // Tout membre peut ajouter ses appareils associés ; seul le créateur renomme.
            entries.push(Entry::Item("person.badge.plus", "Ajouter des membres…".into(), false, Box::new(|c: &Controller| c.chat_add_members())));
            if conversation.is_creator {
                entries.push(Entry::Item("pencil", "Renommer le groupe…".into(), false, Box::new(|c: &Controller| c.chat_rename())));
            }
            entries.push(Entry::Separator);
        }
        entries.push(Entry::Note(format!("MEMBRES ({})", conversation.members.len())));
        for member in &conversation.members {
            let presence = if member.me { "vous" } else if member.online { "en ligne" } else { "hors ligne" };
            let relation = match member.friend {
                FriendState::Friend => " · ami",
                FriendState::Sent => " · demande envoyée",
                FriendState::Received => " · demande reçue",
                FriendState::None | FriendState::Me => "",
            };
            let id = member.id.clone();
            let symbol = if member.online || member.me { "circle.fill" } else { "circle" };
            entries.push(Entry::Item(symbol, format!("{} — {presence}{relation}", member.name), false, Box::new(move |c: &Controller| c.show_profile_near_header(&id))));
            let conversation_id = conversation.id.clone();
            let id = member.id.clone();
            match member.friend {
                FriendState::None if !conversation.closed => entries.push(Entry::Item(
                    "person.crop.circle.badge.plus",
                    "      Ajouter en ami".to_string(),
                    false,
                    Box::new(move |c: &Controller| {
                        if let Err(error) = c.engine().send_friend_request(&conversation_id, &id) {
                            c.engine().set_status(error.to_string());
                        }
                    }),
                )),
                FriendState::Received => entries.push(Entry::Item(
                    "person.crop.circle.badge.checkmark",
                    "      Accepter sa demande d'ami".to_string(),
                    false,
                    Box::new(move |c: &Controller| {
                        if let Err(error) = c.engine().answer_friend_request(&id, true) {
                            c.engine().set_status(error.to_string());
                        }
                    }),
                )),
                _ => {}
            }
        }
        if !conversation.closed {
            entries.push(Entry::Separator);
            entries.push(Entry::Item("rectangle.portrait.and.arrow.right", "Quitter le groupe".into(), true, Box::new(|c: &Controller| c.chat_leave())));
        }
        self.show_actions(anchor, entries);
    }

    /// Bibliothèque d'autocollants : grille d'images ; `removing` : un clic retire au lieu d'envoyer.
    pub(super) fn stickers_popover(&self, anchor: &NSView, removing: bool) {
        let mtm = self.mtm();
        let stickers = self.engine().stickers();
        let mut actions: Vec<Action> = Vec::new();
        let column = stack(true, 6.0, mtm);
        column.setEdgeInsets(NSEdgeInsets { top: 10.0, left: 8.0, bottom: 10.0, right: 8.0 });
        let title = secondary(if removing { "Cliquez sur un autocollant pour le retirer" } else { "AUTOCOLLANTS ET GIF" }, mtm);
        title.setFont(Some(&semibold(11.0)));
        column.addArrangedSubview(&title);
        if stickers.is_empty() {
            column.addArrangedSubview(&secondary("Aucun pour l'instant.", mtm));
        }
        for chunk in stickers.chunks(4) {
            let row = stack(false, 4.0, mtm);
            for sticker in chunk {
                let tag = actions.len() as isize;
                let id = sticker.id.clone();
                actions.push(Box::new(move |c: &Controller| {
                    if removing {
                        c.engine().remove_sticker(&id);
                    } else if let Some(conversation) = c.current_conversation_id() {
                        if let Err(error) = c.engine().send_sticker(&conversation, &id) {
                            c.engine().set_status(error.to_string());
                        }
                    }
                }));
                let item = button("", sel!(popoverAction:), self, mtm);
                item.setTag(tag);
                item.setBezelStyle(NSBezelStyle::AccessoryBar);
                item.setShowsBorderOnlyWhileMouseInside(true);
                if let Some(image) = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(&sticker.path.to_string_lossy())) {
                    image.setSize(NSSize::new(52.0, 52.0));
                    item.setImage(Some(&image));
                    item.setImagePosition(NSCellImagePosition::ImageOnly);
                }
                item.setTranslatesAutoresizingMaskIntoConstraints(false);
                item.widthAnchor().constraintEqualToConstant(56.0).setActive(true);
                item.heightAnchor().constraintEqualToConstant(56.0).setActive(true);
                row.addArrangedSubview(&item);
            }
            column.addArrangedSubview(&row);
        }
        let line = NSBox::new(mtm);
        line.setBoxType(NSBoxType::Separator);
        line.setTranslatesAutoresizingMaskIntoConstraints(false);
        full_width(&column, &line);
        let footer = stack(false, 6.0, mtm);
        let tag = actions.len() as isize;
        actions.push(Box::new(|c: &Controller| c.chat_add_sticker()));
        let create = button("Créer…", sel!(popoverAction:), self, mtm);
        create.setTag(tag);
        footer.addArrangedSubview(&create);
        if !stickers.is_empty() {
            let tag = actions.len() as isize;
            let anchor_view = anchor.retain();
            actions.push(Box::new(move |c: &Controller| c.stickers_popover(&anchor_view, !removing)));
            let toggle = button(if removing { "Terminé" } else { "Retirer…" }, sel!(popoverAction:), self, mtm);
            toggle.setTag(tag);
            footer.addArrangedSubview(&toggle);
        }
        column.addArrangedSubview(&footer);
        self.present_popover(&column, anchor, actions);
    }
}
