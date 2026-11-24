//! Interface macOS native (AppKit).
//!
//! Slint s'affiche lentement sur les Mac sans accélération Metal (par exemple un MacBook Pro
//! de 2011 sous OpenCore Legacy Patcher) ; les contrôles AppKit restent fluides partout.
//! Le moteur (`magicplus_core`) est le même que sous Windows : les deux versions se parlent.

#[path = "ui_mac_chat.rs"]
mod chat_page;
#[path = "ui_mac_markdown.rs"]
mod markdown_view;
#[path = "ui_mac_profile.rs"]
mod profile_page;
#[path = "ui_mac_canvas.rs"]
mod canvas_page;
#[path = "ui_mac_menus.rs"]
mod menus;
#[path = "ui_mac_gif.rs"]
mod gif_page;
#[path = "ui_mac_system.rs"]
mod system;

use crate::shared::{hotkey_labels, platform_label, reveal, setup_hotkeys};
use chat_page::{ChatUi, ChatWidgets, CHAT_PAGE};
use magicplus_core::{util::format_size, Engine, PeerView, Snapshot, TransferState};
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject, Sel},
    sel, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSApplicationActivationPolicy,
    NSApplicationDelegate, NSBackingStoreType, NSBezierPath, NSBorderType, NSBox, NSBoxType, NSButton,
    NSCellImagePosition, NSColor, NSControlStateValueOn, NSFont, NSFontWeightSemibold, NSImage, NSImageScaling, NSImageView,
    NSLayoutAttribute, NSMenu, NSMenuItem, NSPopUpButton, NSProgressIndicator, NSScrollView, NSStackView,
    NSClickGestureRecognizer, NSResponder, NSStackViewGravity, NSTextAlignment, NSTextDelegate, NSTextField, NSTextView,
    NSTextViewDelegate, NSUserInterfaceLayoutOrientation, NSView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSEdgeInsets, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer,
};
use std::{
    cell::{Cell, OnceCell, RefCell},
    thread,
};

const PAGES: [(&str, &str); 5] = [
    ("Appareils", "laptopcomputer"),
    ("Discussions", "bubble.left.and.bubble.right"),
    ("Presse-papiers", "doc.on.clipboard"),
    ("Transferts", "arrow.up.arrow.down"),
    ("Paramètres", "gearshape"),
];
/// Valeurs proposées pour le cache mémoire (Mo).
/// Durées proposées pour reprendre un transfert interrompu (heures).
/// Position de « Paramètres » dans `PAGES`.
const SETTINGS_PAGE: usize = 4;
const RESUME_CHOICES: [u64; 7] = [1, 6, 12, 24, 48, 72, 168];

fn hours_label(hours: u64) -> String {
    match hours {
        1 => "1 heure".to_string(),
        h if h % 24 == 0 && h >= 72 => format!("{} jours", h / 24),
        h => format!("{h} heures"),
    }
}

// ------------------------------------------------------------------ petites aides

/// Numéro (`tag`) de l'élément qui a déclenché une action : bouton ou élément de menu.
fn tag_of(sender: &AnyObject) -> isize {
    if let Some(item) = sender.downcast_ref::<NSMenuItem>() {
        return item.tag();
    }
    if let Some(control) = sender.downcast_ref::<objc2_app_kit::NSControl>() {
        return control.tag();
    }
    -1
}

fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

fn semibold(size: f64) -> Retained<NSFont> {
    NSFont::systemFontOfSize_weight(size, unsafe { NSFontWeightSemibold })
}

fn label(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    NSTextField::labelWithString(&ns(text), mtm)
}

fn strong(text: &str, size: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = label(text, mtm);
    field.setFont(Some(&semibold(size)));
    field
}

fn secondary(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = label(text, mtm);
    field.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field
}

/// Texte sur plusieurs lignes : il faut lui donner une largeur (voir `full_width`).
fn paragraph(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = NSTextField::wrappingLabelWithString(&ns(text), mtm);
    field.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field.setSelectable(false);
    field
}

fn as_target(controller: &Controller) -> &AnyObject {
    controller
}

fn button(title: &str, action: Sel, target: &Controller, mtm: MainThreadMarker) -> Retained<NSButton> {
    unsafe { NSButton::buttonWithTitle_target_action(&ns(title), Some(as_target(target)), Some(action), mtm) }
}

fn symbol(name: &str, tint: &NSColor, mtm: MainThreadMarker) -> Retained<NSImageView> {
    let view = match NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(name), None) {
        Some(image) => NSImageView::imageViewWithImage(&image, mtm),
        None => NSImageView::new(mtm),
    };
    view.setContentTintColor(Some(tint));
    view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.widthAnchor().constraintEqualToConstant(26.0).setActive(true);
    view.heightAnchor().constraintEqualToConstant(26.0).setActive(true);
    view
}

fn stack(vertical: bool, spacing: f64, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let view = NSStackView::new(mtm);
    if vertical {
        view.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        view.setAlignment(NSLayoutAttribute::Leading);
    } else {
        view.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        view.setAlignment(NSLayoutAttribute::CenterY);
    }
    view.setSpacing(spacing);
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view
}

/// Ajoute une vue à une pile verticale en lui donnant toute la largeur.
fn full_width(parent: &NSStackView, view: &NSView) {
    parent.addArrangedSubview(view);
    view.widthAnchor().constraintEqualToAnchor(&parent.widthAnchor()).setActive(true);
}

/// Colle `view` aux bords de `to`, avec des marges.
fn pin(view: &NSView, to: &NSView, top: f64, side: f64, bottom: f64) {
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.topAnchor().constraintEqualToAnchor_constant(&to.topAnchor(), top).setActive(true);
    view.leadingAnchor().constraintEqualToAnchor_constant(&to.leadingAnchor(), side).setActive(true);
    to.trailingAnchor().constraintEqualToAnchor_constant(&view.trailingAnchor(), side).setActive(true);
    to.bottomAnchor().constraintEqualToAnchor_constant(&view.bottomAnchor(), bottom).setActive(true);
}

fn clear(list: &NSStackView) {
    for view in list.arrangedSubviews().iter() {
        list.removeArrangedSubview(&view);
        view.removeFromSuperview();
    }
}

/// Zone de texte sur plusieurs lignes, avec barre de défilement.
fn text_area(editable: bool, mtm: MainThreadMarker) -> (Retained<NSScrollView>, Retained<NSTextView>) {
    let scroll = NSTextView::scrollableTextView(mtm);
    scroll.setBorderType(NSBorderType::BezelBorder);
    scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
    scroll.heightAnchor().constraintEqualToConstant(96.0).setActive(true);
    let text = scroll
        .documentView()
        .and_then(|view| view.downcast::<NSTextView>().ok())
        .expect("scrollableTextView contient une NSTextView");
    text.setRichText(false);
    text.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    text.setEditable(editable);
    (scroll, text)
}

// ------------------------------------------------------------------ vues personnalisées

define_class!(
    /// Vue dont l'origine est en haut : le contenu d'une page défile à partir du haut.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "MagicPlusFlippedView"]
    struct FlippedView;

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl FlippedView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![Self::alloc(mtm), init] }
    }
}

define_class!(
    /// Carte : rectangle arrondi avec une bordure fine (dessin simple, sans effet coûteux).
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "MagicPlusCardView"]
    struct CardView;

    impl CardView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let bounds = self.bounds();
            let rect = NSRect::new(
                NSPoint::new(bounds.origin.x + 0.5, bounds.origin.y + 0.5),
                NSSize::new(bounds.size.width - 1.0, bounds.size.height - 1.0),
            );
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 8.0, 8.0);
            NSColor::controlBackgroundColor().setFill();
            path.fill();
            NSColor::separatorColor().setStroke();
            path.setLineWidth(1.0);
            path.stroke();
        }
    }
);

impl CardView {
    /// Retourne la carte et la pile verticale où placer son contenu.
    fn with_stack(mtm: MainThreadMarker) -> (Retained<Self>, Retained<NSStackView>) {
        let card: Retained<Self> = unsafe { msg_send![Self::alloc(mtm), init] };
        card.setTranslatesAutoresizingMaskIntoConstraints(false);
        let inner = stack(true, 8.0, mtm);
        card.addSubview(&inner);
        pin(&inner, &card, 14.0, 16.0, 14.0);
        (card, inner)
    }
}

// ------------------------------------------------------------------ contrôleur

struct Widgets {
    nav: Vec<Retained<NSButton>>,
    pages: Vec<Retained<NSView>>,
    status: Retained<NSTextField>,
    paired: Retained<NSTextField>,
    me_title: Retained<NSTextField>,
    me_details: Retained<NSTextField>,
    peers_header: Retained<NSTextField>,
    peers_list: Retained<NSStackView>,
    own_addresses: Retained<NSTextField>,
    address_field: Retained<NSTextField>,
    manual_list: Retained<NSStackView>,
    clip_joinable: Retained<NSTextField>,
    send_clip: Retained<NSButton>,
    paste_clip: Retained<NSButton>,
    local_text: Retained<NSTextView>,
    received_title: Retained<NSTextField>,
    received_text: Retained<NSTextView>,
    copy_received: Retained<NSButton>,
    files_joinable: Retained<NSTextField>,
    choose_files: Retained<NSButton>,
    transfers_empty: Retained<NSTextField>,
    transfers_list: Retained<NSStackView>,
    received_header: Retained<NSTextField>,
    received_list: Retained<NSStackView>,
    name_field: Retained<NSTextField>,
    pseudo_field: Retained<NSTextField>,
    folder_label: Retained<NSTextField>,
    clear_folder: Retained<NSButton>,
    resume_popup: Retained<NSPopUpButton>,
    auto_copy: Retained<NSButton>,
    profile_preview: Retained<NSStackView>,
    profile_bio: Retained<NSTextField>,
    profile_pronouns: Retained<NSTextField>,
    profile_color: Retained<NSPopUpButton>,
    profile_remove_photo: Retained<NSButton>,
    profile_remove_banner: Retained<NSButton>,
    update_text: Retained<NSTextField>,
    update_auto: Retained<NSButton>,
    launch_at_login: Retained<NSButton>,
    update_check: Retained<NSButton>,
    update_action: Retained<NSButton>,
    update_page: Retained<NSButton>,
}

/// Ligne de transfert, mise à jour sur place pour ne pas reconstruire la liste à chaque pourcent.
struct TransferRow {
    cancel: Retained<NSButton>,
    id: String,
    icon: Retained<NSImageView>,
    details: Retained<NSTextField>,
    progress: Retained<NSProgressIndicator>,
    state: Retained<NSTextField>,
}

#[derive(Default)]
struct Ivars {
    engine: OnceCell<Engine>,
    widgets: OnceCell<Widgets>,
    peer_ids: RefCell<Vec<String>>,
    /// Appareils des demandes d'ami, dans l'ordre des boutons.
    friend_ids: RefCell<Vec<String>>,
    /// (id, chemin, nom) des fichiers reçus, dans l'ordre des lignes.
    received_files: RefCell<Vec<(String, String, String)>>,
    peers_signature: RefCell<String>,
    manual_signature: RefCell<String>,
    /// Adresses saisies, dans l'ordre des lignes (pour les boutons « Retirer »).
    manual_addresses: RefCell<Vec<String>>,
    received_signature: RefCell<String>,
    clipboard_signature: RefCell<String>,
    transfer_rows: RefCell<Vec<TransferRow>>,
    /// Page affichée (index dans `PAGES`).
    current_page: Cell<usize>,
    chat: RefCell<ChatUi>,
    chat_widgets: OnceCell<ChatWidgets>,
    /// Carte de profil ouverte et l'appareil qu'elle montre.
    profile_popover: RefCell<Option<(Retained<objc2_app_kit::NSPopover>, String)>>,
    /// Version des profils déjà affichée dans les réglages.
    profile_revision: Cell<u64>,
    canvas: RefCell<canvas_page::CanvasUi>,
    canvas_widgets: OnceCell<canvas_page::CanvasWidgets>,
    /// Menu stylisé ouvert et les actions de ses boutons.
    popover: RefCell<Option<Retained<objc2_app_kit::NSPopover>>>,
    popover_actions: RefCell<Vec<menus::Action>>,
    /// Recadrage d'une photo ou d'une bannière en cours.
    crop_editor: RefCell<Option<profile_page::CropEditor>>,
    /// Profil en grand ouvert.
    full_profile: RefCell<Option<profile_page::FullProfile>>,
    gif: RefCell<gif_page::GifUi>,
    /// Fenêtre principale (cachée au lieu d'être fermée : MagicPlus reste joignable).
    main_window: OnceCell<Retained<NSWindow>>,
    /// Barre des menus, notifications, anti-« App Nap ».
    system: OnceCell<system::SystemState>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MagicPlusController"]
    #[ivars = Ivars]
    struct Controller;

    impl Controller {
        #[unsafe(method(tick:))]
        fn tick(&self, _timer: &NSTimer) {
            self.refresh_if_needed();
            self.chat_tick();
            self.gif_tick();
            self.system_tick();
        }

        #[unsafe(method(showMainWindow:))]
        fn show_main_window_action(&self, _sender: &AnyObject) {
            self.show_main_window();
        }

        #[unsafe(method(showDiscussions:))]
        fn show_discussions_action(&self, _sender: &AnyObject) {
            self.show_main_window();
            self.show_page(CHAT_PAGE);
        }

        #[unsafe(method(launchAtLoginChanged:))]
        fn launch_at_login_changed(&self, sender: &NSButton) {
            let enabled = sender.state() == NSControlStateValueOn;
            let engine = self.engine();
            if let Err(error) = engine.set_launch_at_login(enabled) {
                engine.set_status(error.to_string());
            }
            sender.setState(if engine.launch_at_login() { NSControlStateValueOn } else { 0 });
        }

        #[unsafe(method(chatSelect:))]
        fn chat_select_action(&self, sender: &AnyObject) {
            self.chat_select(tag_of(sender));
        }

        #[unsafe(method(chatNew:))]
        fn chat_new_action(&self, sender: &NSButton) {
            self.chat_new_menu(sender);
        }

        #[unsafe(method(chatOpenDirect:))]
        fn chat_open_direct_action(&self, sender: &AnyObject) {
            self.chat_open_direct(tag_of(sender));
        }

        // ---- menus stylisés (ui_mac_menus.rs)

        #[unsafe(method(gifTileClicked:))]
        fn gif_tile_clicked_action(&self, recognizer: &NSClickGestureRecognizer) {
            self.gif_tile_clicked(recognizer);
        }

        #[unsafe(method(chatGif:))]
        fn chat_gif_action(&self, sender: &AnyObject) {
            if let Some(view) = sender.downcast_ref::<NSView>() {
                self.gif_button(view);
            }
        }

        #[unsafe(method(popoverAction:))]
        fn popover_action_clicked(&self, sender: &AnyObject) {
            self.popover_action(tag_of(sender));
        }

        #[unsafe(method(chatRowRightClick:))]
        fn chat_row_right_click_action(&self, recognizer: &NSClickGestureRecognizer) {
            self.chat_row_right_click(recognizer);
        }

        #[unsafe(method(chatPins:))]
        fn chat_pins_action(&self, sender: &NSButton) {
            self.chat_pins(sender);
        }

        // ---- canevas (logique dans ui_mac_canvas.rs)

        #[unsafe(method(chatToggleCanvas:))]
        fn chat_toggle_canvas(&self, _sender: &AnyObject) {
            self.canvas_toggle();
        }

        #[unsafe(method(canvasNew:))]
        fn canvas_new_action(&self, _sender: &AnyObject) {
            self.canvas_new();
        }

        #[unsafe(method(canvasSelect:))]
        fn canvas_select_action(&self, sender: &AnyObject) {
            self.canvas_select(tag_of(sender));
        }

        #[unsafe(method(canvasRename:))]
        fn canvas_rename_action(&self, _sender: &AnyObject) {
            self.canvas_rename();
        }

        #[unsafe(method(canvasDelete:))]
        fn canvas_delete_action(&self, _sender: &AnyObject) {
            self.canvas_delete();
        }

        #[unsafe(method(canvasFormat:))]
        fn canvas_format_action(&self, sender: &AnyObject) {
            self.canvas_format_index(tag_of(sender));
        }

        #[unsafe(method(canvasColor:))]
        fn canvas_color_action(&self, sender: &NSPopUpButton) {
            self.canvas_color(sender);
        }

        #[unsafe(method(canvasHistory:))]
        fn canvas_history_action(&self, _sender: &AnyObject) {
            self.canvas_history_toggle();
        }

        #[unsafe(method(canvasVersionSelect:))]
        fn canvas_version_select_action(&self, sender: &AnyObject) {
            self.canvas_version_select(tag_of(sender));
        }

        #[unsafe(method(canvasRestore:))]
        fn canvas_restore_action(&self, _sender: &AnyObject) {
            self.canvas_restore();
        }

        #[unsafe(method(canvasNameVersion:))]
        fn canvas_name_version_action(&self, _sender: &AnyObject) {
            self.canvas_name_version();
        }

        // ---- profils (logique dans ui_mac_profile.rs)

        #[unsafe(method(chatShowProfile:))]
        fn chat_show_profile_action(&self, sender: &NSButton) {
            let id = self.chat_profile_id(sender.tag());
            if let Some(id) = id {
                self.show_profile(&id, sender);
            }
        }

        #[unsafe(method(profileDirect:))]
        fn profile_direct_action(&self, _sender: &AnyObject) {
            if let Some(id) = self.close_profile_popover() {
                self.chat_with(&id);
            }
        }

        #[unsafe(method(peerProfile:))]
        fn peer_profile_action(&self, sender: &NSButton) {
            let id = self.ivars().peer_ids.borrow().get(sender.tag().max(0) as usize).cloned();
            if let Some(id) = id {
                self.show_full_profile(&id);
            }
        }

        #[unsafe(method(showWhatsNew:))]
        fn show_whats_new_action(&self, _sender: &AnyObject) {
            let notes = self.engine().current_release_notes();
            self.show_release_notes("Nouveautés de MagicPlus", &notes);
        }

        #[unsafe(method(whatsNewAtLaunch:))]
        fn whats_new_at_launch(&self, _timer: &NSTimer) {
            self.maybe_show_whats_new();
        }

        #[unsafe(method(profileFull:))]
        fn profile_full_action(&self, _sender: &AnyObject) {
            if let Some(id) = self.close_profile_popover() {
                self.show_full_profile(&id);
            }
        }

        #[unsafe(method(fullProfileAction:))]
        fn full_profile_action_selector(&self, sender: &NSButton) {
            self.full_profile_action(sender.tag());
        }

        #[unsafe(method(cropChanged:))]
        fn crop_changed_action(&self, _sender: &AnyObject) {
            self.crop_changed();
        }

        #[unsafe(method(profileEdit:))]
        fn profile_edit_action(&self, _sender: &AnyObject) {
            self.close_profile_popover();
            self.show_page(SETTINGS_PAGE);
        }

        #[unsafe(method(profileChoosePhoto:))]
        fn profile_choose_photo(&self, _sender: &AnyObject) {
            self.profile_choose(magicplus_core::ProfileImage::Avatar);
            self.refresh_if_needed();
        }

        #[unsafe(method(profileRemovePhoto:))]
        fn profile_remove_photo(&self, _sender: &AnyObject) {
            self.profile_remove(magicplus_core::ProfileImage::Avatar);
            self.refresh_if_needed();
        }

        #[unsafe(method(profileChooseBanner:))]
        fn profile_choose_banner(&self, _sender: &AnyObject) {
            self.profile_choose(magicplus_core::ProfileImage::Banner);
            self.refresh_if_needed();
        }

        #[unsafe(method(profileRemoveBanner:))]
        fn profile_remove_banner(&self, _sender: &AnyObject) {
            self.profile_remove(magicplus_core::ProfileImage::Banner);
            self.refresh_if_needed();
        }

        #[unsafe(method(profileSave:))]
        fn profile_save_action(&self, _sender: &AnyObject) {
            self.profile_save();
            self.refresh_if_needed();
        }

        #[unsafe(method(chatWith:))]
        fn chat_with_action(&self, sender: &AnyObject) {
            let peer = self.ivars().peer_ids.borrow().get(tag_of(sender).max(0) as usize).cloned();
            if let Some(peer) = peer {
                self.chat_with(&peer);
            }
        }

        #[unsafe(method(chatNewGroup:))]
        fn chat_new_group_action(&self, _sender: &AnyObject) {
            self.chat_new_group();
        }

        #[unsafe(method(chatGroupMenu:))]
        fn chat_group_menu_action(&self, sender: &NSButton) {
            self.chat_group_menu(sender);
        }

        #[unsafe(method(chatAddMembers:))]
        fn chat_add_members_action(&self, _sender: &AnyObject) {
            self.chat_add_members();
        }

        #[unsafe(method(chatRename:))]
        fn chat_rename_action(&self, _sender: &AnyObject) {
            self.chat_rename();
        }

        #[unsafe(method(chatLeave:))]
        fn chat_leave_action(&self, _sender: &AnyObject) {
            self.chat_leave();
        }

        #[unsafe(method(chatSend:))]
        fn chat_send_action(&self, _sender: &AnyObject) {
            self.chat_send();
        }

        #[unsafe(method(chatAttach:))]
        fn chat_attach_action(&self, _sender: &AnyObject) {
            self.chat_attach();
        }

        #[unsafe(method(chatRemovePending:))]
        fn chat_remove_pending_action(&self, sender: &AnyObject) {
            self.chat_remove_pending(tag_of(sender));
        }

        #[unsafe(method(chatEmoji:))]
        fn chat_emoji_action(&self, sender: &AnyObject) {
            if let Some(view) = sender.downcast_ref::<NSView>() {
                self.chat_emoji(view);
            }
        }

        #[unsafe(method(chatStickers:))]
        fn chat_stickers_action(&self, _sender: &AnyObject) {
            self.chat_stickers_menu();
        }

        #[unsafe(method(chatSendSticker:))]
        fn chat_send_sticker_action(&self, sender: &AnyObject) {
            self.chat_send_sticker(tag_of(sender));
        }

        #[unsafe(method(chatAddSticker:))]
        fn chat_add_sticker_action(&self, _sender: &AnyObject) {
            self.chat_add_sticker();
        }

        #[unsafe(method(chatRemoveSticker:))]
        fn chat_remove_sticker_action(&self, sender: &AnyObject) {
            self.chat_remove_sticker(tag_of(sender));
        }

        #[unsafe(method(chatNoop:))]
        fn chat_noop(&self, _sender: &AnyObject) {}

        #[unsafe(method(chatMessageMenu:))]
        fn chat_message_menu_action(&self, sender: &NSButton) {
            self.chat_message_menu(sender);
        }

        #[unsafe(method(chatReply:))]
        fn chat_reply_action(&self, sender: &AnyObject) {
            self.chat_reply(tag_of(sender));
        }

        #[unsafe(method(chatEdit:))]
        fn chat_edit_action(&self, sender: &AnyObject) {
            self.chat_edit(tag_of(sender));
        }

        #[unsafe(method(chatDelete:))]
        fn chat_delete_action(&self, sender: &AnyObject) {
            self.chat_delete(tag_of(sender));
        }

        #[unsafe(method(chatCopyMessage:))]
        fn chat_copy_message_action(&self, sender: &AnyObject) {
            self.chat_copy_message(tag_of(sender));
        }

        #[unsafe(method(chatCopyCode:))]
        fn chat_copy_code_action(&self, sender: &AnyObject) {
            self.chat_copy_code(tag_of(sender));
        }

        #[unsafe(method(chatReact:))]
        fn chat_react_action(&self, sender: &AnyObject) {
            self.chat_react(tag_of(sender));
        }

        #[unsafe(method(chatToggleReaction:))]
        fn chat_toggle_reaction_action(&self, sender: &AnyObject) {
            self.chat_toggle_reaction(tag_of(sender));
        }

        #[unsafe(method(chatOpenAttachment:))]
        fn chat_open_attachment_action(&self, sender: &AnyObject) {
            let tag = match sender.downcast_ref::<NSClickGestureRecognizer>() {
                Some(recognizer) => chat_page::gesture_tag(recognizer).unwrap_or(-1),
                None => tag_of(sender),
            };
            self.chat_open_attachment(tag);
        }

        #[unsafe(method(chatCancelBanner:))]
        fn chat_cancel_banner_action(&self, _sender: &AnyObject) {
            self.chat_cancel_banner();
        }

        /// Cmd+V : image ou fichiers copiés → pièces jointes dans une discussion, sinon collage normal.
        #[unsafe(method(smartPaste:))]
        fn smart_paste_action(&self, sender: Option<&AnyObject>) {
            self.smart_paste(sender);
        }

        #[unsafe(method(navClicked:))]
        fn nav_clicked(&self, sender: &NSButton) {
            self.show_page(sender.tag() as usize);
        }

        #[unsafe(method(friendAccept:))]
        fn friend_accept(&self, sender: &NSButton) {
            self.answer_friend(sender, Some(true));
        }

        #[unsafe(method(friendRefuse:))]
        fn friend_refuse(&self, sender: &NSButton) {
            self.answer_friend(sender, Some(false));
        }

        #[unsafe(method(friendCancel:))]
        fn friend_cancel(&self, sender: &NSButton) {
            self.answer_friend(sender, None);
        }

        #[unsafe(method(pairClicked:))]
        fn pair_clicked(&self, sender: &NSButton) {
            let Some(id) = self.ivars().peer_ids.borrow().get(sender.tag() as usize).cloned() else { return };
            let engine = self.engine();
            let Some(peer) = engine.snapshot().peers.into_iter().find(|peer| peer.id == id) else { return };
            if self.confirm_pairing(&peer) {
                if let Err(error) = engine.trust(&id) {
                    engine.set_status(error.to_string());
                }
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(addAddress:))]
        fn add_address(&self, _sender: &AnyObject) {
            let field = &self.widgets().address_field;
            let text = field.stringValue().to_string();
            let engine = self.engine();
            match engine.add_manual_address(&text) {
                Ok(_) => field.setStringValue(&ns("")),
                Err(error) => engine.set_status(format!("Connexion par adresse : {error}")),
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(removeAddress:))]
        fn remove_address(&self, sender: &NSButton) {
            let Some(address) = self.ivars().manual_addresses.borrow().get(sender.tag() as usize).cloned() else {
                return;
            };
            if let Err(error) = self.engine().remove_manual_address(&address) {
                self.engine().set_status(error.to_string());
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(forgetClicked:))]
        fn forget_clicked(&self, sender: &NSButton) {
            let Some(id) = self.ivars().peer_ids.borrow().get(sender.tag() as usize).cloned() else { return };
            if let Err(error) = self.engine().forget(&id) {
                self.engine().set_status(error.to_string());
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(sendClipboard:))]
        fn send_clipboard(&self, _sender: &NSButton) {
            let engine = self.engine().clone();
            thread::spawn(move || engine.send_clipboard(false));
        }

        #[unsafe(method(pasteReceived:))]
        fn paste_received(&self, _sender: &NSButton) {
            self.engine().paste_last_received();
            self.refresh_if_needed();
        }

        #[unsafe(method(readClipboard:))]
        fn read_clipboard(&self, _sender: &NSButton) {
            let text = self.engine().read_local_clipboard();
            self.widgets().local_text.setString(&ns(&text));
        }

        #[unsafe(method(copyLocal:))]
        fn copy_local(&self, _sender: &NSButton) {
            let text = self.widgets().local_text.string().to_string();
            self.copy_text(&text);
        }

        #[unsafe(method(copyReceived:))]
        fn copy_received(&self, _sender: &NSButton) {
            let text = self.widgets().received_text.string().to_string();
            self.copy_text(&text);
        }

        #[unsafe(method(chooseFiles:))]
        fn choose_files(&self, _sender: &NSButton) {
            if let Some(paths) = rfd::FileDialog::new().set_title("Choisir les fichiers à envoyer").pick_files() {
                self.engine().send_files(paths);
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(chooseFolder:))]
        fn choose_folder(&self, _sender: &NSButton) {
            if let Some(folder) = rfd::FileDialog::new().set_title("Dossier de réception").pick_folder() {
                let path = folder.to_string_lossy().to_string();
                let engine = self.engine();
                match engine.update_settings(|settings| settings.save_folder = Some(path)) {
                    Ok(()) => engine.set_status("Dossier de réception enregistré"),
                    Err(error) => engine.set_status(error.to_string()),
                }
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(clearFolder:))]
        fn clear_folder(&self, _sender: &NSButton) {
            let engine = self.engine();
            match engine.update_settings(|settings| settings.save_folder = None) {
                Ok(()) => engine.set_status("Les fichiers reçus iront dans Téléchargements"),
                Err(error) => engine.set_status(error.to_string()),
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(savePseudo:))]
        fn save_pseudo(&self, _sender: &AnyObject) {
            let pseudo = self.widgets().pseudo_field.stringValue().to_string();
            if let Err(error) = self.engine().set_pseudo(&pseudo) {
                self.engine().set_status(error.to_string());
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(peerAlias:))]
        fn peer_alias(&self, sender: &AnyObject) {
            let peer = self.ivars().peer_ids.borrow().get(tag_of(sender).max(0) as usize).cloned();
            let Some(peer) = peer else { return };
            let current = self.engine().snapshot().peers.into_iter().find(|p| p.id == peer);
            let Some(current) = current else { return };
            let alert = NSAlert::new(self.mtm());
            alert.setMessageText(&ns(&format!("Surnom pour « {} »", current.device_name)));
            alert.setInformativeText(&ns(&format!(
                "Visible seulement sur cet appareil. {}Laissez vide pour revenir à son nom.",
                if current.pseudo.is_empty() { String::new() } else { format!("Son pseudo : « {} ». ", current.pseudo) }
            )));
            let field = NSTextField::textFieldWithString(&ns(&current.alias), self.mtm());
            field.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(280.0, 24.0)));
            alert.setAccessoryView(Some(&field));
            alert.addButtonWithTitle(&ns("Enregistrer"));
            alert.addButtonWithTitle(&ns("Annuler"));
            if alert.runModal() == NSAlertFirstButtonReturn {
                if let Err(error) = self.engine().set_alias(&peer, &field.stringValue().to_string()) {
                    self.engine().set_status(error.to_string());
                }
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(saveName:))]
        fn save_name(&self, _sender: &AnyObject) {
            let name = self.widgets().name_field.stringValue().to_string();
            if let Err(error) = self.engine().set_device_name(&name) {
                self.engine().set_status(error.to_string());
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(presenceChanged:))]
        fn presence_changed(&self, sender: &NSPopUpButton) {
            let Some(item) = sender.selectedItem() else { return };
            if let Some(presence) = magicplus_core::Presence::CHOICES.get(item.tag().max(0) as usize) {
                if let Err(error) = self.engine().set_presence(*presence) {
                    self.engine().set_status(error.to_string());
                }
            }
        }

        #[unsafe(method(autoIdleChanged:))]
        fn auto_idle_changed(&self, sender: &NSPopUpButton) {
            let Some(item) = sender.selectedItem() else { return };
            let minutes = item.tag().max(0) as u32;
            let _ = self.engine().update_settings(|settings| settings.auto_idle_minutes = minutes);
        }

        #[unsafe(method(notifGroupsChanged:))]
        fn notif_groups_changed(&self, sender: &NSPopUpButton) {
            use magicplus_core::GroupNotifications;
            let Some(item) = sender.selectedItem() else { return };
            let groups = match item.tag() {
                1 => GroupNotifications::Mentions,
                2 => GroupNotifications::Never,
                _ => GroupNotifications::All,
            };
            let _ = self.engine().update_settings(|settings| settings.notifications.groups = groups);
        }

        /// Cases à cocher des notifications : le `tag` dit laquelle.
        #[unsafe(method(notifToggled:))]
        fn notif_toggled(&self, sender: &NSButton) {
            let on = sender.state() == NSControlStateValueOn;
            let tag = sender.tag();
            let _ = self.engine().update_settings(|settings| {
                let prefs = &mut settings.notifications;
                match tag {
                    1 => prefs.enabled = on,
                    2 => prefs.direct = on,
                    3 => prefs.preview = on,
                    4 => prefs.sound = on,
                    _ => prefs.when_focused = on,
                }
            });
        }

        #[unsafe(method(notifTest:))]
        fn notif_test(&self, _sender: &AnyObject) {
            self.engine().test_notice();
        }

        #[unsafe(method(themeChanged:))]
        fn theme_changed(&self, sender: &NSPopUpButton) {
            let Some(item) = sender.selectedItem() else { return };
            let Some(theme) = crate::themes::THEMES.get(item.tag().max(0) as usize) else { return };
            let _ = self.engine().update_settings(|settings| {
                settings.theme = Some(theme.id.to_string());
                settings.dark_mode = theme.palette.map(|p| p.dark).or(theme.platform_dark);
            });
            apply_theme(theme, self.mtm());
        }

        #[unsafe(method(resumeChanged:))]
        fn resume_changed(&self, sender: &NSPopUpButton) {
            if let Some(item) = sender.selectedItem() {
                let hours = item.tag().max(1) as u64;
                let engine = self.engine();
                match engine.update_settings(|settings| settings.resume_hours = hours) {
                    Ok(()) => engine.set_status(format!("Un transfert interrompu peut reprendre pendant {}", hours_label(hours))),
                    Err(error) => engine.set_status(error.to_string()),
                }
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(autoCopyChanged:))]
        fn auto_copy_changed(&self, sender: &NSButton) {
            let enabled = sender.state() == NSControlStateValueOn;
            let engine = self.engine();
            if let Err(error) = engine.update_settings(|settings| settings.auto_copy_received = enabled) {
                engine.set_status(error.to_string());
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(updateAutoChanged:))]
        fn update_auto_changed(&self, sender: &NSButton) {
            let enabled = sender.state() == NSControlStateValueOn;
            let engine = self.engine();
            if let Err(error) = engine.update_settings(|settings| settings.auto_update = enabled) {
                engine.set_status(error.to_string());
            } else if enabled {
                engine.check_update_now();
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(updateCheck:))]
        fn update_check(&self, _sender: &NSButton) {
            self.engine().check_update_now();
            self.refresh_if_needed();
        }

        #[unsafe(method(updateAction:))]
        fn update_action_clicked(&self, _sender: &NSButton) {
            if crate::shared::update_action(self.engine()) {
                NSApplication::sharedApplication(self.mtm()).terminate(None);
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(updatePage:))]
        fn update_page(&self, _sender: &NSButton) {
            let url = self.engine().update().page_url;
            if !url.is_empty() {
                crate::shared::open_path(&url);
            }
        }

        #[unsafe(method(cancelTransfer:))]
        fn cancel_transfer(&self, sender: &NSButton) {
            let id = self.ivars().transfer_rows.borrow().get(sender.tag() as usize).map(|row| row.id.clone());
            if let Some(id) = id {
                self.engine().cancel_transfer(&id);
            }
            self.refresh_if_needed();
        }

        #[unsafe(method(revealReceived:))]
        fn reveal_received(&self, sender: &NSButton) {
            if let Some((_, path, _)) = self.ivars().received_files.borrow().get(sender.tag() as usize) {
                reveal(path);
            }
        }

        #[unsafe(method(removeReceived:))]
        fn remove_received(&self, sender: &NSButton) {
            let Some((id, _, _)) = self.ivars().received_files.borrow().get(sender.tag() as usize).cloned() else {
                return;
            };
            self.engine().remove_received(&id);
            self.refresh_if_needed();
        }
    }

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSTextDelegate for Controller {
        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, notification: &NSNotification) {
            let object = notification.object();
            if !self.canvas_text_changed(object.as_deref()) {
                self.chat_text_changed();
            }
        }
    }

    unsafe impl NSTextViewDelegate for Controller {
        #[unsafe(method(textView:doCommandBySelector:))]
        fn text_view_do_command(&self, _text_view: &NSTextView, selector: Sel) -> bool {
            self.chat_command(selector)
        }
    }

    unsafe impl NSApplicationDelegate for Controller {
        /// Fenêtre fermée : MagicPlus reste actif (barre des menus) et joignable par les autres.
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool {
            false
        }

        /// Clic sur l'icône du Dock alors que la fenêtre est cachée.
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _sender: &NSApplication, has_visible_windows: bool) -> bool {
            if !has_visible_windows {
                self.show_main_window();
            }
            true
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            if let Some(engine) = self.ivars().engine.get() {
                engine.shutdown();
            }
        }
    }
);

impl Controller {
    fn new(engine: Engine, mtm: MainThreadMarker) -> Retained<Self> {
        let ivars = Ivars::default();
        let _ = ivars.engine.set(engine);
        let this = Self::alloc(mtm).set_ivars(ivars);
        unsafe { msg_send![super(this), init] }
    }

    fn engine(&self) -> &Engine {
        self.ivars().engine.get().expect("moteur initialisé")
    }

    fn widgets(&self) -> &Widgets {
        self.ivars().widgets.get().expect("interface construite")
    }

    fn copy_text(&self, text: &str) {
        let engine = self.engine();
        if text.is_empty() {
            engine.set_status("Rien à copier");
        } else {
            match engine.write_local_clipboard(text) {
                Ok(()) => engine.set_status("Texte copié dans le presse-papiers"),
                Err(error) => engine.set_status(error.to_string()),
            }
        }
        self.refresh_if_needed();
    }

    fn show_page(&self, index: usize) {
        self.ivars().current_page.set(index);
        if index == CHAT_PAGE && self.ivars().chat_widgets.get().is_some() {
            self.refresh_chat(false);
        }
        let widgets = self.widgets();
        for (position, page) in widgets.pages.iter().enumerate() {
            page.setHidden(position != index);
        }
        for (position, button) in widgets.nav.iter().enumerate() {
            let selected = position == index;
            let font = if selected { semibold(14.0) } else { NSFont::systemFontOfSize(14.0) };
            let tint = if selected { NSColor::controlAccentColor() } else { NSColor::labelColor() };
            button.setFont(Some(&font));
            button.setContentTintColor(Some(&tint));
        }
    }

    /// Demande d'ami : accepter (`Some(true)`), refuser (`Some(false)`) ou retirer la nôtre (`None`).
    fn answer_friend(&self, sender: &NSButton, accept: Option<bool>) {
        let Some(id) = self.ivars().friend_ids.borrow().get(sender.tag().max(0) as usize).cloned() else { return };
        let engine = self.engine();
        let result = match accept {
            Some(accept) => engine.answer_friend_request(&id, accept),
            None => engine.cancel_friend_request(&id),
        };
        if let Err(error) = result {
            engine.set_status(error.to_string());
        }
        self.refresh_if_needed();
    }

    fn confirm_pairing(&self, peer: &PeerView) -> bool {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&ns(&if peer.key_changed {
            format!("Associer à nouveau « {} » ?", peer.name)
        } else {
            format!("Associer « {} » ?", peer.name)
        }));
        let mut text = String::new();
        if peer.key_changed {
            alert.setAlertStyle(NSAlertStyle::Critical);
            text.push_str(
                "Attention : cet appareil avait déjà été associé mais annonce maintenant une autre clé. \
                 Ne continuez que si MagicPlus a été réinstallé sur cette machine.\n\n",
            );
        }
        text.push_str(&format!(
            "Vérifiez que cette empreinte est strictement identique à celle affichée sur l'autre appareil \
             (page Appareils, ligne « cet appareil ») :\n\n{}\n\nSi elle est différente, quelqu'un d'autre se fait passer pour lui.",
            peer.fingerprint
        ));
        alert.setInformativeText(&ns(&text));
        alert.addButtonWithTitle(&ns("L'empreinte est identique : associer"));
        alert.addButtonWithTitle(&ns("Annuler"));
        alert.runModal() == NSAlertFirstButtonReturn
    }

    fn refresh_if_needed(&self) {
        if self.engine().take_dirty() {
            self.refresh(false);
        }
    }

    fn refresh(&self, first: bool) {
        let mtm = self.mtm();
        let widgets = self.widgets();
        let snapshot = self.engine().snapshot();
        let joinable = snapshot.peers.iter().filter(|peer| peer.trusted && peer.online).count();
        let paired = snapshot.peers.iter().filter(|peer| peer.trusted).count();

        widgets.status.setStringValue(&ns(&snapshot.status));
        widgets.status.setToolTip(Some(&ns(&snapshot.status)));
        widgets.paired.setStringValue(&ns(&format!("{paired} appareil(s) associé(s)")));
        widgets
            .me_title
            .setStringValue(&ns(&if snapshot.identity.pseudo.is_empty() {
                format!("{}  (cet appareil)", snapshot.identity.name)
            } else {
                format!("{}  ·  {}  (cet appareil)", snapshot.identity.pseudo, snapshot.identity.name)
            }));
        widgets.me_details.setStringValue(&ns(&format!(
            "{}  ·  Empreinte  {}",
            platform_label(&snapshot.identity.platform),
            snapshot.identity.fingerprint
        )));
        widgets.peers_header.setStringValue(&ns(&format!("À proximité ({})", snapshot.peers.len())));

        self.refresh_peers(&snapshot, mtm);
        self.refresh_manual(&snapshot, mtm);

        let joinable_text = if joinable == 0 {
            "Aucun appareil associé n'est joignable pour le moment.".to_string()
        } else {
            format!("{joinable} appareil(s) associé(s) joignable(s)")
        };
        widgets.clip_joinable.setStringValue(&ns(&joinable_text));
        widgets.files_joinable.setStringValue(&ns(&joinable_text));
        widgets.send_clip.setEnabled(joinable > 0);
        widgets.choose_files.setEnabled(joinable > 0);

        match &snapshot.clipboard {
            Some(item) => {
                let signature = format!("{}\u{0}{}", item.from, item.text);
                if *self.ivars().clipboard_signature.borrow() != signature {
                    widgets.received_text.setString(&ns(&item.text));
                    widgets.received_title.setStringValue(&ns(&format!("Dernier texte reçu de {}", item.from)));
                    *self.ivars().clipboard_signature.borrow_mut() = signature;
                }
                widgets.paste_clip.setEnabled(true);
                widgets.copy_received.setEnabled(true);
            }
            None => {
                widgets.paste_clip.setEnabled(false);
                widgets.copy_received.setEnabled(false);
            }
        }

        self.refresh_transfers(&snapshot, mtm);
        self.refresh_received(&snapshot, mtm);

        let custom_folder = snapshot.settings.save_folder.as_ref().is_some_and(|folder| !folder.is_empty());
        widgets.folder_label.setStringValue(&ns(&if custom_folder {
            format!("Dossier de réception :  {}", snapshot.receive_folder.display())
        } else {
            format!("Dossier de réception (par défaut) :  {}", snapshot.receive_folder.display())
        }));
        widgets.clear_folder.setEnabled(custom_folder);

        if first {
            widgets.name_field.setStringValue(&ns(&snapshot.identity.name));
            widgets.pseudo_field.setStringValue(&ns(&snapshot.identity.pseudo));
            let current = snapshot.settings.resume_hours;
            let index = RESUME_CHOICES.iter().position(|value| *value == current).unwrap_or_else(|| {
                widgets.resume_popup.addItemWithTitle(&ns(&hours_label(current)));
                if let Some(item) = widgets.resume_popup.lastItem() {
                    item.setTag(current as isize);
                }
                RESUME_CHOICES.len()
            });
            widgets.resume_popup.selectItemAtIndex(index as isize);
            widgets.auto_copy.setState(if snapshot.settings.auto_copy_received { NSControlStateValueOn } else { 0 });
            widgets.update_auto.setState(if snapshot.settings.auto_update { NSControlStateValueOn } else { 0 });
            widgets.launch_at_login.setState(if self.engine().launch_at_login() { NSControlStateValueOn } else { 0 });
        }

        self.refresh_profile_settings(first);

        // Mises à jour.
        let (text, action) = crate::shared::update_texts(&snapshot.update, snapshot.settings.auto_update);
        widgets.update_text.setStringValue(&ns(&text));
        match action {
            Some(title) => {
                widgets.update_action.setTitle(&ns(title));
                widgets.update_action.setHidden(false);
            }
            None => widgets.update_action.setHidden(true),
        }
        let busy = matches!(snapshot.update.state, magicplus_core::UpdateState::Checking | magicplus_core::UpdateState::Downloading(_));
        widgets.update_check.setEnabled(!busy);
        widgets.update_page.setHidden(snapshot.update.latest.is_empty());
        if let Some(nav) = widgets.nav.get(SETTINGS_PAGE) {
            let title = if crate::shared::update_pending(&snapshot.update) { "  Paramètres  •" } else { "  Paramètres" };
            nav.setTitle(&ns(title));
        }
    }

    fn refresh_peers(&self, snapshot: &Snapshot, mtm: MainThreadMarker) {
        // Le nombre en tête évite qu'une liste vide ressemble à « jamais affichée ».
        let signature: String = std::iter::once(format!("{}\n", snapshot.peers.len()))
            .chain(snapshot.other_versions.iter().map(|o| format!("{}\n", o.message)))
            .chain(snapshot.friend_requests.iter().map(|r| format!("{r:?}\n")))
            .chain(snapshot.peers.iter().map(|p| {
                format!(
                    "{}|{}|{}|{}|{}|{}|{}|{}\n",
                    p.id, p.name, p.address, p.fingerprint, p.trusted, p.online, p.key_changed, p.wants_pairing
                )
            }))
            .collect();
        if *self.ivars().peers_signature.borrow() == signature {
            return;
        }
        *self.ivars().peers_signature.borrow_mut() = signature;

        let list = &self.widgets().peers_list;
        clear(list);
        let mut ids = Vec::new();

        // Appareils d'une autre version : on explique pourquoi ils n'apparaissent pas.
        for other in &snapshot.other_versions {
            let (card, inner) = CardView::with_stack(mtm);
            let row = stack(false, 12.0, mtm);
            row.addArrangedSubview(&symbol("exclamationmark.triangle", &NSColor::systemOrangeColor(), mtm));
            let texts = stack(true, 4.0, mtm);
            let title = if other.newer { "Version plus récente sur" } else { "Ancienne version sur" };
            texts.addArrangedSubview(&strong(&format!("{title} « {} »", other.name), 14.0, mtm));
            full_width(&texts, &paragraph(&other.message, mtm));
            row.addArrangedSubview(&texts);
            full_width(&inner, &row);
            full_width(list, &card);
        }

        // Demandes d'ami (membres d'un groupe commun).
        let mut friend_ids = Vec::new();
        for request in &snapshot.friend_requests {
            let tag = friend_ids.len() as isize;
            friend_ids.push(request.id.clone());
            let (card, inner) = CardView::with_stack(mtm);
            let row = stack(false, 12.0, mtm);
            let (icon, color) = if request.incoming {
                ("person.crop.circle.badge.plus", NSColor::systemOrangeColor())
            } else {
                ("paperplane", NSColor::secondaryLabelColor())
            };
            row.addArrangedSubview(&symbol(icon, &color, mtm));
            let texts = stack(true, 2.0, mtm);
            let group = if request.group.is_empty() { String::new() } else { format!(" (groupe « {} »)", request.group) };
            let title = if request.incoming {
                format!("{} vous demande en ami{group}", request.name)
            } else {
                format!("Demande d'ami à {}{group}", request.name)
            };
            texts.addArrangedSubview(&strong(&title, 14.0, mtm));
            let detail = if request.incoming {
                format!("{}  ·  Empreinte  {}  ·  accepter l'associe à cet appareil", platform_label(&request.platform), request.fingerprint)
            } else if request.delivered {
                "Reçue : en attente de sa réponse".to_string()
            } else {
                "Partira dès que cet appareil sera en ligne".to_string()
            };
            texts.addArrangedSubview(&secondary(&detail, mtm));
            row.addArrangedSubview(&texts);
            if request.incoming {
                let accept = button("Accepter", sel!(friendAccept:), self, mtm);
                accept.setTag(tag);
                let refuse = button("Refuser", sel!(friendRefuse:), self, mtm);
                refuse.setTag(tag);
                row.addView_inGravity(&refuse, NSStackViewGravity::Trailing);
                row.addView_inGravity(&accept, NSStackViewGravity::Trailing);
            } else {
                let cancel = button("Annuler", sel!(friendCancel:), self, mtm);
                cancel.setTag(tag);
                row.addView_inGravity(&cancel, NSStackViewGravity::Trailing);
            }
            full_width(&inner, &row);
            full_width(list, &card);
        }
        *self.ivars().friend_ids.borrow_mut() = friend_ids;

        if snapshot.peers.is_empty() {
            let (card, inner) = CardView::with_stack(mtm);
            let row = stack(false, 12.0, mtm);
            row.addArrangedSubview(&symbol("wifi", &NSColor::secondaryLabelColor(), mtm));
            let texts = stack(true, 4.0, mtm);
            texts.addArrangedSubview(&strong("Aucun appareil MagicPlus détecté", 14.0, mtm));
            full_width(
                &texts,
                &paragraph(
                    "Ouvrez MagicPlus sur l'autre machine, branchée au même réseau privé. Sous Windows, autorisez \
                     MagicPlus dans le pare-feu (réseaux privés) quand Windows le demande.",
                    mtm,
                ),
            );
            row.addArrangedSubview(&texts);
            full_width(&inner, &row);
            full_width(list, &card);
        }

        for (index, peer) in snapshot.peers.iter().enumerate() {
            ids.push(peer.id.clone());
            let (card, inner) = CardView::with_stack(mtm);
            let row = stack(false, 12.0, mtm);
            let tint = if peer.online { NSColor::controlAccentColor() } else { NSColor::secondaryLabelColor() };
            row.addArrangedSubview(&symbol("laptopcomputer", &tint, mtm));

            let texts = stack(true, 2.0, mtm);
            texts.addArrangedSubview(&strong(&peer.name, 14.0, mtm));
            let mut details = if peer.device_name != peer.name {
                format!("{}  ·  {}", peer.device_name, platform_label(&peer.platform))
            } else {
                platform_label(&peer.platform)
            };
            if !peer.alias.is_empty() && !peer.pseudo.is_empty() {
                details.push_str(&format!("  ·  pseudo « {} »", peer.pseudo));
            }
            if !peer.address.is_empty() {
                details.push_str(&format!("  ·  {}", peer.address));
            }
            texts.addArrangedSubview(&secondary(&details, mtm));
            texts.addArrangedSubview(&secondary(&format!("Empreinte  {}", peer.fingerprint), mtm));
            row.addArrangedSubview(&texts);

            let (state, color) = if peer.key_changed {
                ("Clé modifiée", NSColor::systemRedColor())
            } else if peer.trusted && peer.online {
                ("Associé · en ligne", NSColor::systemGreenColor())
            } else if peer.trusted {
                ("Associé · hors ligne", NSColor::secondaryLabelColor())
            } else if peer.wants_pairing {
                ("Demande d'association", NSColor::systemOrangeColor())
            } else if peer.online {
                ("Détecté", NSColor::systemBlueColor())
            } else {
                ("Détecté · hors ligne", NSColor::secondaryLabelColor())
            };
            let state_label = strong(state, 12.0, mtm);
            state_label.setTextColor(Some(&color));
            row.addView_inGravity(&state_label, NSStackViewGravity::Trailing);

            let action = if peer.trusted {
                button("Oublier", sel!(forgetClicked:), self, mtm)
            } else {
                let title = if peer.key_changed { "Associer à nouveau" } else { "Associer" };
                let associate = button(title, sel!(pairClicked:), self, mtm);
                associate.setEnabled(peer.online);
                if !peer.online {
                    associate.setToolTip(Some(&ns(
                        "Ses annonces n'arrivent plus. S'il est sous Windows, cliquez sur « Autoriser MagicPlus » dans \
                         sa page Appareils (pare-feu), ou saisissez son adresse ci-dessous.",
                    )));
                }
                if !peer.key_changed {
                    associate.setKeyEquivalent(&ns(""));
                }
                associate
            };
            action.setTag(index as isize);
            if peer.trusted && !peer.key_changed {
                let profile = button("Profil", sel!(peerProfile:), self, mtm);
                profile.setTag(index as isize);
                row.addView_inGravity(&profile, NSStackViewGravity::Trailing);
                let alias = button("Surnom…", sel!(peerAlias:), self, mtm);
                alias.setTag(index as isize);
                alias.setToolTip(Some(&ns("Donner un surnom à cet appareil (visible seulement ici)")));
                row.addView_inGravity(&alias, NSStackViewGravity::Trailing);
                let talk = button("Discuter", sel!(chatWith:), self, mtm);
                talk.setTag(index as isize);
                row.addView_inGravity(&talk, NSStackViewGravity::Trailing);
            }
            row.addView_inGravity(&action, NSStackViewGravity::Trailing);

            full_width(&inner, &row);
            full_width(list, &card);
        }
        *self.ivars().peer_ids.borrow_mut() = ids;
    }

    fn refresh_manual(&self, snapshot: &Snapshot, mtm: MainThreadMarker) {
        let widgets = self.widgets();
        let mut own = if snapshot.local_addresses.is_empty() {
            "Adresse de cet appareil : aucune (Wi-Fi ou Ethernet déconnecté ?)".to_string()
        } else {
            format!("Adresse(s) de cet appareil, à saisir sur l'autre machine :  {}", snapshot.local_addresses.join("   ·   "))
        };
        if !snapshot.virtual_addresses.is_empty() {
            own.push_str(&format!(
                "\nCartes virtuelles ignorées (VMware, Hyper-V…), inutiles aux autres appareils :  {}",
                snapshot.virtual_addresses.join("   ·   ")
            ));
        }
        if !snapshot.bluetooth.is_empty() {
            own.push_str(&format!("\n{}", snapshot.bluetooth));
        }
        widgets.own_addresses.setStringValue(&ns(&own));

        let signature: String = snapshot
            .manual
            .iter()
            .map(|item| format!("{}|{:?}|{}\n", item.address, item.peer_name, item.unresolved))
            .collect();
        if *self.ivars().manual_signature.borrow() == signature {
            return;
        }
        *self.ivars().manual_signature.borrow_mut() = signature;

        clear(&widgets.manual_list);
        let mut addresses = Vec::new();
        for (index, item) in snapshot.manual.iter().enumerate() {
            addresses.push(item.address.clone());
            let row = stack(false, 10.0, mtm);
            let (icon, tint, state) = match (&item.peer_name, item.unresolved) {
                (Some(name), _) => ("checkmark.circle.fill", NSColor::systemGreenColor(), format!("MagicPlus trouvé : {name}")),
                (None, true) => ("exclamationmark.triangle", NSColor::systemOrangeColor(), "Adresse introuvable".to_string()),
                (None, false) => ("clock", NSColor::secondaryLabelColor(), "Pas de réponse pour l'instant".to_string()),
            };
            row.addArrangedSubview(&symbol(icon, &tint, mtm));
            row.addArrangedSubview(&strong(&item.address, 13.0, mtm));
            let state_label = secondary(&state, mtm);
            state_label.setTextColor(Some(&tint));
            row.addArrangedSubview(&state_label);
            let remove = button("Retirer", sel!(removeAddress:), self, mtm);
            remove.setTag(index as isize);
            row.addView_inGravity(&remove, NSStackViewGravity::Trailing);
            full_width(&widgets.manual_list, &row);
        }
        *self.ivars().manual_addresses.borrow_mut() = addresses;
    }

    fn refresh_transfers(&self, snapshot: &Snapshot, mtm: MainThreadMarker) {
        let widgets = self.widgets();
        widgets.transfers_empty.setHidden(!snapshot.transfers.is_empty());

        let same_rows = {
            let rows = self.ivars().transfer_rows.borrow();
            rows.len() == snapshot.transfers.len()
                && rows.iter().zip(&snapshot.transfers).all(|(row, transfer)| row.id == transfer.id)
        };
        if !same_rows {
            clear(&widgets.transfers_list);
            let mut rows = Vec::new();
            for transfer in &snapshot.transfers {
                let (card, inner) = CardView::with_stack(mtm);
                let row = stack(false, 12.0, mtm);
                let icon = symbol(
                    if transfer.sending { "arrow.up.circle" } else { "arrow.down.circle" },
                    &NSColor::controlAccentColor(),
                    mtm,
                );
                row.addArrangedSubview(&icon);
                let texts = stack(true, 4.0, mtm);
                texts.addArrangedSubview(&strong(&transfer.file_name, 14.0, mtm));
                let details = secondary("", mtm);
                texts.addArrangedSubview(&details);
                let progress = NSProgressIndicator::new(mtm);
                progress.setIndeterminate(false);
                progress.setMinValue(0.0);
                progress.setMaxValue(1.0);
                progress.setTranslatesAutoresizingMaskIntoConstraints(false);
                progress.widthAnchor().constraintEqualToConstant(280.0).setActive(true);
                texts.addArrangedSubview(&progress);
                row.addArrangedSubview(&texts);
                let state = strong("", 12.0, mtm);
                row.addView_inGravity(&state, NSStackViewGravity::Trailing);
                let cancel = button("Annuler", sel!(cancelTransfer:), self, mtm);
                cancel.setTag(rows.len() as isize);
                row.addView_inGravity(&cancel, NSStackViewGravity::Trailing);
                full_width(&inner, &row);
                full_width(&widgets.transfers_list, &card);
                rows.push(TransferRow { cancel, id: transfer.id.clone(), icon, details, progress, state });
            }
            *self.ivars().transfer_rows.borrow_mut() = rows;
        }

        for (row, transfer) in self.ivars().transfer_rows.borrow().iter().zip(&snapshot.transfers) {
            let direction = if transfer.sending { "vers" } else { "depuis" };
            let base = format!("{direction} {}  ·  {}", transfer.peer_name, format_size(transfer.size));
            let percent = format!("{} %", (transfer.progress * 100.0).round() as u32);
            let (details, details_color, state, color, active) = match &transfer.state {
                TransferState::Active => (base, NSColor::secondaryLabelColor(), percent, NSColor::controlAccentColor(), true),
                TransferState::Paused(reason) => (
                    format!("{base}  ·  {reason}"),
                    NSColor::systemOrangeColor(),
                    format!("En pause · {percent}"),
                    NSColor::systemOrangeColor(),
                    false,
                ),
                TransferState::Complete => {
                    (base, NSColor::secondaryLabelColor(), "Terminé".to_string(), NSColor::systemGreenColor(), false)
                }
                TransferState::Failed(reason) => {
                    (reason.clone(), NSColor::systemRedColor(), "Échec".to_string(), NSColor::systemRedColor(), false)
                }
            };
            let cancellable = matches!(transfer.state, TransferState::Active | TransferState::Paused(_));
            row.cancel.setHidden(!cancellable);
            row.details.setStringValue(&ns(&details));
            row.details.setTextColor(Some(&details_color));
            row.state.setStringValue(&ns(&state));
            row.state.setTextColor(Some(&color));
            row.icon.setContentTintColor(Some(&color));
            row.progress.setHidden(!active);
            row.progress.setDoubleValue(transfer.progress as f64);
        }
    }

    fn refresh_received(&self, snapshot: &Snapshot, mtm: MainThreadMarker) {
        let signature: String = snapshot
            .received
            .iter()
            .map(|file| format!("{}|{}\n", file.id, file.path.display()))
            .collect();
        if *self.ivars().received_signature.borrow() == signature {
            return;
        }
        *self.ivars().received_signature.borrow_mut() = signature;

        let widgets = self.widgets();
        widgets.received_header.setHidden(snapshot.received.is_empty());
        clear(&widgets.received_list);
        let mut files = Vec::new();
        for (index, file) in snapshot.received.iter().enumerate() {
            let path = file.path.to_string_lossy().to_string();
            files.push((file.id.clone(), path, file.file_name.clone()));

            let (card, inner) = CardView::with_stack(mtm);
            let row = stack(false, 12.0, mtm);
            row.addArrangedSubview(&symbol("doc", &NSColor::controlAccentColor(), mtm));
            let texts = stack(true, 2.0, mtm);
            texts.addArrangedSubview(&strong(&file.file_name, 14.0, mtm));
            let folder = file
                .path
                .parent()
                .and_then(|parent| parent.file_name())
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            texts.addArrangedSubview(&secondary(
                &format!("de {}  ·  {}  ·  dans « {folder} »", file.from, format_size(file.size)),
                mtm,
            ));
            row.addArrangedSubview(&texts);

            let primary = button("Afficher", sel!(revealReceived:), self, mtm);
            primary.setTag(index as isize);
            row.addView_inGravity(&primary, NSStackViewGravity::Trailing);
            let remove = button("Retirer", sel!(removeReceived:), self, mtm);
            remove.setTag(index as isize);
            row.addView_inGravity(&remove, NSStackViewGravity::Trailing);

            full_width(&inner, &row);
            full_width(&widgets.received_list, &card);
        }
        *self.ivars().received_files.borrow_mut() = files;
    }
}

// ------------------------------------------------------------------ construction de la fenêtre

/// Page défilante : retourne la vue de défilement et la pile où placer le contenu.
fn page(mtm: MainThreadMarker) -> (Retained<NSScrollView>, Retained<NSStackView>) {
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

    let content = stack(true, 12.0, mtm);
    document.addSubview(&content);
    pin(&content, &document, 26.0, 32.0, 26.0);
    (scroll, content)
}

fn page_title(content: &NSStackView, title: &str, subtitle: Option<&str>, mtm: MainThreadMarker) {
    let heading = label(title, mtm);
    heading.setFont(Some(&NSFont::boldSystemFontOfSize(26.0)));
    content.addArrangedSubview(&heading);
    if let Some(subtitle) = subtitle {
        full_width(content, &paragraph(subtitle, mtm));
    }
}

fn card_in(content: &NSStackView, title: Option<&str>, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let (card, inner) = CardView::with_stack(mtm);
    if let Some(title) = title {
        inner.addArrangedSubview(&strong(title, 14.0, mtm));
    }
    full_width(content, &card);
    inner
}

fn buttons_row(buttons: &[&NSButton], mtm: MainThreadMarker) -> Retained<NSStackView> {
    let row = stack(false, 8.0, mtm);
    for button in buttons {
        row.addArrangedSubview(button);
    }
    row
}

fn build_window(controller: &Controller, mtm: MainThreadMarker) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1000.0, 680.0)),
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&ns("MagicPlus"));
    window.setContentMinSize(NSSize::new(660.0, 460.0));
    let root = window.contentView().expect("vue de contenu");

    // ---- barre latérale
    let sidebar = stack(true, 4.0, mtm);
    sidebar.setEdgeInsets(NSEdgeInsets { top: 18.0, left: 12.0, bottom: 14.0, right: 12.0 });
    root.addSubview(&sidebar);
    sidebar.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    sidebar.leadingAnchor().constraintEqualToAnchor(&root.leadingAnchor()).setActive(true);
    sidebar.bottomAnchor().constraintEqualToAnchor(&root.bottomAnchor()).setActive(true);
    sidebar.widthAnchor().constraintEqualToConstant(214.0).setActive(true);

    let brand = stack(false, 8.0, mtm);
    brand.addArrangedSubview(&symbol("lock.shield.fill", &NSColor::controlAccentColor(), mtm));
    brand.addArrangedSubview(&strong("MagicPlus", 17.0, mtm));
    sidebar.addArrangedSubview(&brand);
    let gap = NSView::new(mtm);
    gap.setTranslatesAutoresizingMaskIntoConstraints(false);
    gap.heightAnchor().constraintEqualToConstant(12.0).setActive(true);
    sidebar.addArrangedSubview(&gap);

    let mut nav = Vec::new();
    for (index, (title, icon)) in PAGES.iter().enumerate() {
        let item = button(&format!("  {title}"), sel!(navClicked:), controller, mtm);
        item.setTag(index as isize);
        item.setBordered(false);
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(icon), None) {
            item.setImage(Some(&image));
            item.setImagePosition(NSCellImagePosition::ImageLeading);
            item.setImageHugsTitle(true);
        }
        item.setAlignment(NSTextAlignment::Left);
        item.setTranslatesAutoresizingMaskIntoConstraints(false);
        item.widthAnchor().constraintEqualToConstant(186.0).setActive(true);
        item.heightAnchor().constraintEqualToConstant(30.0).setActive(true);
        // « Paramètres » en bas, comme dans les applications du système.
        if index == PAGES.len() - 1 {
            sidebar.addView_inGravity(&item, NSStackViewGravity::Trailing);
        } else {
            sidebar.addArrangedSubview(&item);
        }
        nav.push(item);
    }

    let separator = NSBox::new(mtm);
    separator.setBoxType(NSBoxType::Separator);
    separator.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(&separator);
    separator.leadingAnchor().constraintEqualToAnchor(&sidebar.trailingAnchor()).setActive(true);
    separator.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    separator.bottomAnchor().constraintEqualToAnchor(&root.bottomAnchor()).setActive(true);
    separator.widthAnchor().constraintEqualToConstant(1.0).setActive(true);

    // ---- barre d'état
    let status = secondary("", mtm);
    let paired = secondary("", mtm);
    let status_bar = stack(false, 20.0, mtm);
    status_bar.addArrangedSubview(&status);
    status_bar.addView_inGravity(&paired, NSStackViewGravity::Trailing);
    root.addSubview(&status_bar);
    status_bar.leadingAnchor().constraintEqualToAnchor_constant(&separator.trailingAnchor(), 16.0).setActive(true);
    root.trailingAnchor().constraintEqualToAnchor_constant(&status_bar.trailingAnchor(), 16.0).setActive(true);
    root.bottomAnchor().constraintEqualToAnchor_constant(&status_bar.bottomAnchor(), 6.0).setActive(true);
    status_bar.heightAnchor().constraintEqualToConstant(20.0).setActive(true);
    // Un long message ne doit pas élargir la fenêtre : il se termine par « … » (texte complet au survol).
    status.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
    status.setContentCompressionResistancePriority_forOrientation(
        1.0,
        objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
    );

    // ---- zone des pages
    let area = NSView::new(mtm);
    area.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(&area);
    area.leadingAnchor().constraintEqualToAnchor(&separator.trailingAnchor()).setActive(true);
    area.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    area.trailingAnchor().constraintEqualToAnchor(&root.trailingAnchor()).setActive(true);
    status_bar.topAnchor().constraintEqualToAnchor_constant(&area.bottomAnchor(), 6.0).setActive(true);

    // Appareils
    let (devices, content) = page(mtm);
    page_title(
        &content,
        "Appareils",
        Some("Les appareils qui exécutent MagicPlus sur le même réseau Wi-Fi ou Ethernet apparaissent ici automatiquement, sans Internet."),
        mtm,
    );
    let me = card_in(&content, None, mtm);
    let me_row = stack(false, 12.0, mtm);
    me_row.addArrangedSubview(&symbol("lock.shield", &NSColor::systemGreenColor(), mtm));
    let me_texts = stack(true, 2.0, mtm);
    let me_title = strong("", 14.0, mtm);
    let me_details = secondary("", mtm);
    me_texts.addArrangedSubview(&me_title);
    me_texts.addArrangedSubview(&me_details);
    me_row.addArrangedSubview(&me_texts);
    full_width(&me, &me_row);
    let peers_header = strong("À proximité", 14.0, mtm);
    content.addArrangedSubview(&peers_header);
    let peers_list = stack(true, 10.0, mtm);
    full_width(&content, &peers_list);

    let manual_card = card_in(&content, Some("Connexion par adresse IP"), mtm);
    full_width(
        &manual_card,
        &paragraph(
            "Si l'autre appareil n'apparaît pas tout seul (réseau d'entreprise ou d'école, Wi-Fi invité, \
             autre sous-réseau), saisissez son adresse IP : MagicPlus l'appellera directement. \
             Il suffit de le faire sur une des deux machines, à condition que le pare-feu de chacune \
             laisse passer MagicPlus (sous Windows : bouton « Autoriser MagicPlus » de sa page Appareils).",
            mtm,
        ),
    );
    let own_addresses = secondary("", mtm);
    own_addresses.setSelectable(true);
    manual_card.addArrangedSubview(&own_addresses);
    let address_field = NSTextField::textFieldWithString(&ns(""), mtm);
    address_field.setPlaceholderString(Some(&ns("ex. 192.168.1.20")));
    address_field.setTranslatesAutoresizingMaskIntoConstraints(false);
    address_field.widthAnchor().constraintEqualToConstant(260.0).setActive(true);
    unsafe {
        address_field.setTarget(Some(as_target(controller)));
        address_field.setAction(Some(sel!(addAddress:)));
    }
    let add_address = button("Ajouter", sel!(addAddress:), controller, mtm);
    let address_row = stack(false, 8.0, mtm);
    address_row.addArrangedSubview(&address_field);
    address_row.addArrangedSubview(&add_address);
    manual_card.addArrangedSubview(&address_row);
    let manual_list = stack(true, 6.0, mtm);
    full_width(&manual_card, &manual_list);

    // Presse-papiers
    let (clipboard, content) = page(mtm);
    page_title(
        &content,
        "Presse-papiers",
        Some("Envoyez le texte copié vers vos appareils associés. Tout est chiffré de bout en bout."),
        mtm,
    );
    let send_card = card_in(&content, Some("Envoyer"), mtm);
    let clip_joinable = secondary("", mtm);
    send_card.addArrangedSubview(&clip_joinable);
    let send_clip = button("Envoyer le presse-papiers", sel!(sendClipboard:), controller, mtm);
    let paste_clip = button("Copier le dernier texte reçu", sel!(pasteReceived:), controller, mtm);
    send_card.addArrangedSubview(&buttons_row(&[&send_clip, &paste_clip], mtm));

    let local_card = card_in(&content, Some("Presse-papiers de cet appareil"), mtm);
    let (local_scroll, local_text) = text_area(true, mtm);
    full_width(&local_card, &local_scroll);
    let read_button = button("Lire le presse-papiers", sel!(readClipboard:), controller, mtm);
    let copy_local = button("Copier ce texte", sel!(copyLocal:), controller, mtm);
    local_card.addArrangedSubview(&buttons_row(&[&read_button, &copy_local], mtm));

    let received_card = card_in(&content, None, mtm);
    let received_title = strong("Dernier texte reçu", 14.0, mtm);
    received_card.addArrangedSubview(&received_title);
    let (received_scroll, received_text) = text_area(false, mtm);
    received_text.setString(&ns("Rien reçu pour l'instant."));
    full_width(&received_card, &received_scroll);
    let copy_received = button("Copier dans le presse-papiers", sel!(copyReceived:), controller, mtm);
    received_card.addArrangedSubview(&copy_received);

    let shortcuts = card_in(&content, Some("Raccourcis globaux"), mtm);
    let (send_label, paste_label) = hotkey_labels();
    shortcuts.addArrangedSubview(&secondary(&format!("Copier la sélection et l'envoyer :  {send_label}"), mtm));
    shortcuts.addArrangedSubview(&secondary(&format!("Remettre le dernier texte reçu dans le presse-papiers :  {paste_label}"), mtm));
    full_width(
        &shortcuts,
        &paragraph(
            "« Copier et envoyer » demande l'autorisation Accessibilité (Réglages Système > Confidentialité et sécurité).",
            mtm,
        ),
    );

    // Transferts
    let (transfers, content) = page(mtm);
    page_title(
        &content,
        "Transferts",
        Some("Les fichiers passent directement d'un appareil à l'autre par le réseau local, chiffrés, avec vérification d'intégrité."),
        mtm,
    );
    let files_card = card_in(&content, Some("Envoyer des fichiers"), mtm);
    let files_joinable = secondary("", mtm);
    files_card.addArrangedSubview(&files_joinable);
    let choose_files = button("Choisir des fichiers…", sel!(chooseFiles:), controller, mtm);
    files_card.addArrangedSubview(&choose_files);
    content.addArrangedSubview(&strong("Récents", 14.0, mtm));
    let transfers_empty = secondary("Aucun transfert pour l'instant.", mtm);
    content.addArrangedSubview(&transfers_empty);
    let transfers_list = stack(true, 10.0, mtm);
    full_width(&content, &transfers_list);
    let received_header = strong("Fichiers reçus", 14.0, mtm);
    received_header.setHidden(true);
    content.addArrangedSubview(&received_header);
    let received_list = stack(true, 10.0, mtm);
    full_width(&content, &received_list);

    // Paramètres
    let (settings, content) = page(mtm);
    page_title(&content, "Paramètres", None, mtm);

    // Mon profil : aperçu à gauche, réglages à droite.
    let profile_card_box = card_in(&content, Some("Mon profil"), mtm);
    profile_card_box.addArrangedSubview(&secondary(
        "Comme sur Discord : vos contacts (appareils associés et membres de vos groupes) voient votre photo, votre bannière et votre présentation.",
        mtm,
    ));
    let profile_row = stack(false, 20.0, mtm);
    profile_row.setAlignment(NSLayoutAttribute::Top);
    let profile_preview = stack(true, 0.0, mtm);
    profile_row.addArrangedSubview(&profile_preview);
    let profile_form = stack(true, 8.0, mtm);
    let photo_row = stack(false, 8.0, mtm);
    photo_row.addArrangedSubview(&button("Choisir une photo…", sel!(profileChoosePhoto:), controller, mtm));
    let profile_remove_photo = button("Retirer", sel!(profileRemovePhoto:), controller, mtm);
    photo_row.addArrangedSubview(&profile_remove_photo);
    profile_form.addArrangedSubview(&photo_row);
    let banner_row = stack(false, 8.0, mtm);
    banner_row.addArrangedSubview(&button("Choisir une bannière…", sel!(profileChooseBanner:), controller, mtm));
    let profile_remove_banner = button("Retirer", sel!(profileRemoveBanner:), controller, mtm);
    banner_row.addArrangedSubview(&profile_remove_banner);
    profile_form.addArrangedSubview(&banner_row);
    profile_form.addArrangedSubview(&secondary("PNG, JPEG ou GIF animé. La photo est recadrée en carré, la bannière en 5:2.", mtm));
    let profile_color = NSPopUpButton::new(mtm);
    profile_color.addItemWithTitle(&ns("Couleur de la bannière : automatique"));
    for (name, _) in profile_page::BANNER_COLORS {
        profile_color.addItemWithTitle(&ns(&format!("Couleur de la bannière : {}", name.to_lowercase())));
    }
    profile_form.addArrangedSubview(&profile_color);
    profile_form.addArrangedSubview(&label("Pronoms", mtm));
    let profile_pronouns = NSTextField::textFieldWithString(&ns(""), mtm);
    profile_pronouns.setPlaceholderString(Some(&ns("ex. elle/elle, il/lui")));
    profile_pronouns.setTranslatesAutoresizingMaskIntoConstraints(false);
    profile_pronouns.widthAnchor().constraintEqualToConstant(320.0).setActive(true);
    profile_form.addArrangedSubview(&profile_pronouns);
    profile_form.addArrangedSubview(&label("À propos de moi (Markdown, 190 caractères)", mtm));
    let profile_bio = NSTextField::textFieldWithString(&ns(""), mtm);
    profile_bio.setPlaceholderString(Some(&ns("Quelques mots sur vous… **gras**, *italique*, émojis 🙂")));
    profile_bio.setUsesSingleLineMode(false);
    profile_bio.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByWordWrapping);
    profile_bio.setTranslatesAutoresizingMaskIntoConstraints(false);
    profile_bio.widthAnchor().constraintEqualToConstant(320.0).setActive(true);
    profile_bio.heightAnchor().constraintEqualToConstant(64.0).setActive(true);
    profile_form.addArrangedSubview(&profile_bio);
    profile_form.addArrangedSubview(&button("Enregistrer le profil", sel!(profileSave:), controller, mtm));
    profile_row.addArrangedSubview(&profile_form);
    profile_card_box.addArrangedSubview(&profile_row);
    let device_card = card_in(&content, Some("Cet appareil"), mtm);
    device_card.addArrangedSubview(&label("Pseudo", mtm));
    device_card.addArrangedSubview(&secondary(
        "Votre nom pour les autres : ils le voient dès qu'ils détectent cet appareil, avant même l'association.",
        mtm,
    ));
    let pseudo_field = NSTextField::textFieldWithString(&ns(""), mtm);
    pseudo_field.setPlaceholderString(Some(&ns("ex. Léa")));
    pseudo_field.setTranslatesAutoresizingMaskIntoConstraints(false);
    pseudo_field.widthAnchor().constraintEqualToConstant(320.0).setActive(true);
    unsafe {
        pseudo_field.setTarget(Some(as_target(controller)));
        pseudo_field.setAction(Some(sel!(savePseudo:)));
    }
    let pseudo_row = stack(false, 8.0, mtm);
    pseudo_row.addArrangedSubview(&pseudo_field);
    pseudo_row.addArrangedSubview(&button("Enregistrer", sel!(savePseudo:), controller, mtm));
    device_card.addArrangedSubview(&pseudo_row);
    device_card.addArrangedSubview(&label("Nom de l'appareil", mtm));
    device_card.addArrangedSubview(&secondary("Le nom de cet ordinateur, affiché à côté du pseudo.", mtm));
    let name_field = NSTextField::textFieldWithString(&ns(""), mtm);
    name_field.setTranslatesAutoresizingMaskIntoConstraints(false);
    name_field.widthAnchor().constraintEqualToConstant(320.0).setActive(true);
    unsafe {
        name_field.setTarget(Some(as_target(controller)));
        name_field.setAction(Some(sel!(saveName:)));
    }
    let save_name = button("Enregistrer", sel!(saveName:), controller, mtm);
    let name_row = stack(false, 8.0, mtm);
    name_row.addArrangedSubview(&name_field);
    name_row.addArrangedSubview(&save_name);
    device_card.addArrangedSubview(&name_row);

    let folder_card = card_in(&content, Some("Réception des fichiers"), mtm);
    let folder_label = paragraph("", mtm);
    full_width(&folder_card, &folder_label);
    let choose_folder = button("Choisir un dossier…", sel!(chooseFolder:), controller, mtm);
    let clear_folder = button("Ne plus utiliser de dossier", sel!(clearFolder:), controller, mtm);
    folder_card.addArrangedSubview(&buttons_row(&[&choose_folder, &clear_folder], mtm));
    full_width(
        &folder_card,
        &paragraph(
            "Les fichiers arrivent par petits morceaux dans un dossier caché, puis sont rangés ici une fois \
             complets et vérifiés. Un transfert interrompu (Wi-Fi coupé, ordinateur en veille ou éteint) \
             reprend tout seul là où il s'était arrêté.",
            mtm,
        ),
    );
    let resume_popup = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(160.0, 26.0)),
        false,
    );
    for hours in RESUME_CHOICES {
        resume_popup.addItemWithTitle(&ns(&hours_label(hours)));
        if let Some(item) = resume_popup.lastItem() {
            item.setTag(hours as isize);
        }
    }
    unsafe {
        resume_popup.setTarget(Some(as_target(controller)));
        resume_popup.setAction(Some(sel!(resumeChanged:)));
    }
    let resume_row = stack(false, 8.0, mtm);
    resume_row.addArrangedSubview(&label("Reprise possible pendant :", mtm));
    resume_row.addArrangedSubview(&resume_popup);
    folder_card.addArrangedSubview(&resume_row);

    let clip_card = card_in(&content, Some("Presse-papiers"), mtm);
    let auto_copy = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &ns("Mettre automatiquement le texte reçu dans le presse-papiers"),
            Some(as_target(controller)),
            Some(sel!(autoCopyChanged:)),
            mtm,
        )
    };
    clip_card.addArrangedSubview(&auto_copy);
    full_width(
        &clip_card,
        &paragraph(
            "Activé : copiez sur l'autre appareil (ou Cmd/Ctrl + Shift + Option/Alt + C), puis collez directement ici.",
            mtm,
        ),
    );

    let background_card = card_in(&content, Some("Démarrage et notifications"), mtm);
    let launch_at_login = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &ns("Lancer MagicPlus à l'ouverture de la session (en arrière-plan, dans la barre des menus)"),
            Some(as_target(controller)),
            Some(sel!(launchAtLoginChanged:)),
            mtm,
        )
    };
    background_card.addArrangedSubview(&launch_at_login);
    full_width(
        &background_card,
        &paragraph(
            "Fermer la fenêtre ne quitte pas MagicPlus : il reste joignable par vos autres appareils et vous \
             prévient des messages. Pour le quitter : menu MagicPlus ou icône de la barre des menus → Quitter.",
            mtm,
        ),
    );

    let update_card = card_in(&content, Some("Mises à jour"), mtm);
    let update_text = paragraph("", mtm);
    update_text.setSelectable(true);
    full_width(&update_card, &update_text);
    let update_auto = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &ns("Chercher automatiquement les nouvelles versions (seul usage d'Internet)"),
            Some(as_target(controller)),
            Some(sel!(updateAutoChanged:)),
            mtm,
        )
    };
    update_card.addArrangedSubview(&update_auto);
    let update_row = stack(false, 8.0, mtm);
    let update_action = button("Télécharger et installer", sel!(updateAction:), controller, mtm);
    update_action.setHidden(true);
    let update_check = button("Rechercher maintenant", sel!(updateCheck:), controller, mtm);
    let update_page = button("Page de la version", sel!(updatePage:), controller, mtm);
    update_page.setHidden(true);
    update_row.addArrangedSubview(&update_action);
    update_row.addArrangedSubview(&update_check);
    update_row.addArrangedSubview(&update_page);
    update_row.addArrangedSubview(&button("Voir les nouveautés", sel!(showWhatsNew:), controller, mtm));
    update_card.addArrangedSubview(&update_row);

    let look_card = card_in(&content, Some("Apparence"), mtm);
    let theme_popup = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, 26.0)),
        false,
    );
    let saved = controller.engine().snapshot().settings;
    let current = crate::themes::current(saved.theme.as_deref(), saved.dark_mode);
    for (index, theme) in crate::themes::THEMES.iter().enumerate() {
        theme_popup.addItemWithTitle(&ns(theme.name));
        if let Some(item) = theme_popup.lastItem() {
            item.setTag(index as isize);
        }
        if theme.id == current.id {
            theme_popup.selectItemAtIndex(index as isize);
        }
    }
    unsafe {
        theme_popup.setTarget(Some(as_target(controller)));
        theme_popup.setAction(Some(sel!(themeChanged:)));
    }
    let theme_row = stack(false, 8.0, mtm);
    theme_row.addArrangedSubview(&label("Thème :", mtm));
    theme_row.addArrangedSubview(&theme_popup);
    look_card.addArrangedSubview(&theme_row);
    look_card.addArrangedSubview(&secondary(
        "« Système » suit l'apparence de macOS. Les thèmes de couleur choisissent l'apparence claire ou sombre et le fond de la fenêtre.",
        mtm,
    ));

    // Statut (présence) et notifications.
    let presence_card = card_in(&content, Some("Statut et notifications"), mtm);
    let saved = controller.engine().snapshot().settings;
    let popup = |items: &[(&str, isize)], selected: isize, action: Sel| {
        let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(220.0, 26.0)), false);
        for (index, (title, tag)) in items.iter().enumerate() {
            popup.addItemWithTitle(&ns(title));
            if let Some(item) = popup.lastItem() {
                item.setTag(*tag);
            }
            if *tag == selected {
                popup.selectItemAtIndex(index as isize);
            }
        }
        unsafe {
            popup.setTarget(Some(as_target(controller)));
            popup.setAction(Some(action));
        }
        popup
    };
    let presence_index = magicplus_core::Presence::CHOICES.iter().position(|p| *p == saved.presence).unwrap_or(0) as isize;
    let presence_items: Vec<(&str, isize)> =
        magicplus_core::Presence::CHOICES.iter().enumerate().map(|(i, p)| (p.label(), i as isize)).collect();
    let row = stack(false, 8.0, mtm);
    row.addArrangedSubview(&label("Statut :", mtm));
    row.addArrangedSubview(&popup(&presence_items, presence_index, sel!(presenceChanged:)));
    presence_card.addArrangedSubview(&row);
    let row = stack(false, 8.0, mtm);
    row.addArrangedSubview(&label("Inactif sans clavier ni souris pendant :", mtm));
    row.addArrangedSubview(&popup(
        &[("Jamais", 0), ("5 minutes", 5), ("10 minutes", 10), ("15 minutes", 15), ("30 minutes", 30)],
        saved.auto_idle_minutes as isize,
        sel!(autoIdleChanged:),
    ));
    presence_card.addArrangedSubview(&row);
    let prefs = &saved.notifications;
    for (tag, title, on) in [
        (1, "Notifications du système pour les nouveaux messages", prefs.enabled),
        (2, "Discussions à deux", prefs.direct),
        (3, "Afficher le texte du message", prefs.preview),
        (4, "Jouer un son", prefs.sound),
        (5, "Même quand la discussion est ouverte à l'écran", prefs.when_focused),
    ] {
        let check = unsafe { NSButton::checkboxWithTitle_target_action(&ns(title), Some(as_target(controller)), Some(sel!(notifToggled:)), mtm) };
        check.setTag(tag);
        check.setState(if on { NSControlStateValueOn } else { 0 });
        presence_card.addArrangedSubview(&check);
    }
    let groups_index = match prefs.groups {
        magicplus_core::GroupNotifications::All => 0,
        magicplus_core::GroupNotifications::Mentions => 1,
        magicplus_core::GroupNotifications::Never => 2,
    };
    let row = stack(false, 8.0, mtm);
    row.addArrangedSubview(&label("Groupes :", mtm));
    row.addArrangedSubview(&popup(
        &[("Tous les messages", 0), ("Seulement si on me mentionne (@)", 1), ("Jamais", 2)],
        groups_index,
        sel!(notifGroupsChanged:),
    ));
    presence_card.addArrangedSubview(&row);
    presence_card.addArrangedSubview(&button("Tester une notification", sel!(notifTest:), controller, mtm));
    full_width(
        &presence_card,
        &paragraph("« Ne pas déranger » coupe toutes les notifications. Réglages fins : Réglages Système → Notifications → MagicPlus.", mtm),
    );

    let about_card = card_in(&content, Some("À propos"), mtm);
    full_width(
        &about_card,
        &paragraph(
            &format!(
                "MagicPlus {}  ·  Aucun serveur, aucun compte : tout reste sur votre réseau local. \
                 Découverte UDP 45454, transferts TCP 45455.",
                env!("CARGO_PKG_VERSION")
            ),
            mtm,
        ),
    );

    let (chat, chat_widgets) = chat_page::build(controller, mtm);
    let _ = controller.ivars().chat_widgets.set(chat_widgets);
    let pages: Vec<Retained<NSView>> = vec![
        Retained::into_super(devices),
        chat,
        Retained::into_super(clipboard),
        Retained::into_super(transfers),
        Retained::into_super(settings),
    ];
    for page in &pages {
        area.addSubview(page);
        pin(page, &area, 0.0, 0.0, 0.0);
    }

    let _ = controller.ivars().widgets.set(Widgets {
        nav,
        pages,
        status,
        paired,
        me_title,
        me_details,
        peers_header,
        peers_list,
        own_addresses,
        address_field,
        manual_list,
        clip_joinable,
        send_clip,
        paste_clip,
        local_text,
        received_title,
        received_text,
        copy_received,
        files_joinable,
        choose_files,
        transfers_empty,
        transfers_list,
        received_header,
        received_list,
        name_field,
        pseudo_field,
        folder_label,
        clear_folder,
        resume_popup,
        auto_copy,
        profile_preview,
        profile_bio,
        profile_pronouns,
        profile_color,
        profile_remove_photo,
        profile_remove_banner,
        update_text,
        update_auto,
        launch_at_login,
        update_check,
        update_action,
        update_page,
    });
    window
}

/// Menus « MagicPlus » (Quitter) et « Édition » : sans eux, Cmd+Q, Cmd+C et Cmd+V ne marchent pas.
fn build_menu(app: &NSApplication, mtm: MainThreadMarker) {
    let item = |title: &str, action: Sel, key: &str| unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), Some(action), &ns(key))
    };
    let main = NSMenu::new(mtm);

    let app_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("MagicPlus"));
    app_menu.addItem(&item("Masquer MagicPlus", sel!(hide:), "h"));
    app_menu.addItem(&NSMenuItem::separatorItem(mtm));
    app_menu.addItem(&item("Quitter MagicPlus", sel!(terminate:), "q"));
    let app_entry = NSMenuItem::new(mtm);
    app_entry.setSubmenu(Some(&app_menu));
    main.addItem(&app_entry);

    let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Édition"));
    edit_menu.addItem(&item("Annuler", sel!(undo:), "z"));
    edit_menu.addItem(&NSMenuItem::separatorItem(mtm));
    edit_menu.addItem(&item("Couper", sel!(cut:), "x"));
    edit_menu.addItem(&item("Copier", sel!(copy:), "c"));
    // « Coller » passe par le contrôleur : une image ou des fichiers copiés deviennent des pièces
    // jointes dans la zone de saisie d'une discussion ; ailleurs, collage normal.
    edit_menu.addItem(&item("Coller", sel!(smartPaste:), "v"));
    edit_menu.addItem(&item("Tout sélectionner", sel!(selectAll:), "a"));
    let edit_entry = NSMenuItem::new(mtm);
    edit_entry.setSubmenu(Some(&edit_menu));
    main.addItem(&edit_entry);

    let window_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Fenêtre"));
    window_menu.addItem(&item("Réduire", sel!(performMiniaturize:), "m"));
    window_menu.addItem(&item("Fermer", sel!(performClose:), "w"));
    let window_entry = NSMenuItem::new(mtm);
    window_entry.setSubmenu(Some(&window_menu));
    main.addItem(&window_entry);

    app.setMainMenu(Some(&main));
}

/// Thème de couleur : apparence claire ou sombre (ou celle du système) et fond des fenêtres.
fn apply_theme(theme: &crate::themes::ThemeDef, mtm: MainThreadMarker) {
    use objc2_app_kit::{NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua};
    let app = NSApplication::sharedApplication(mtm);
    let dark = theme.palette.map(|p| p.dark).or(theme.platform_dark);
    let appearance = dark.and_then(|dark| NSAppearance::appearanceNamed(unsafe { if dark { NSAppearanceNameDarkAqua } else { NSAppearanceNameAqua } }));
    app.setAppearance(appearance.as_deref());
    let background = match theme.palette {
        Some(p) => {
            let channel = |shift: u32| ((p.window >> shift) & 0xff) as f64 / 255.0;
            NSColor::colorWithSRGBRed_green_blue_alpha(channel(16), channel(8), channel(0), 1.0)
        }
        None => NSColor::windowBackgroundColor(),
    };
    for window in app.windows().iter() {
        window.setBackgroundColor(Some(&background));
    }
}

pub fn run(engine: Engine) {
    let mtm = MainThreadMarker::new().expect("l'interface doit démarrer sur le fil principal");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let controller = Controller::new(engine.clone(), mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*controller)));
    build_menu(&app, mtm);
    let window = build_window(&controller, mtm);
    let settings = engine.snapshot().settings;
    apply_theme(crate::themes::current(settings.theme.as_deref(), settings.dark_mode), mtm);
    let _ = controller.ivars().main_window.set(window.clone());
    controller.setup_system(mtm);

    let start_page = std::env::var("MAGICPLUS_START_PAGE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|page| *page < PAGES.len())
        .unwrap_or(0);
    controller.show_page(start_page);
    // Développement : ouvre la n-ième discussion (captures d'écran).
    if let Some(index) = std::env::var("MAGICPLUS_START_CHAT").ok().and_then(|v| v.parse::<isize>().ok()) {
        controller.chat_select(index);
        // Développement : ouvre directement les canevas de cette conversation.
        if let Ok(mode) = std::env::var("MAGICPLUS_START_CANVAS") {
            controller.canvas_toggle();
            if mode == "historique" {
                controller.canvas_history_toggle();
                controller.canvas_version_select(0);
            }
        }
    }
    controller.refresh(true);
    let _ = engine.take_dirty();
    // Développement : profil en grand (`moi`, ou nom d'un appareil) pour les captures.
    if let Ok(name) = std::env::var("MAGICPLUS_START_PROFILE") {
        let snapshot = engine.snapshot();
        let id = if name == "moi" {
            Some(snapshot.identity.id.clone())
        } else {
            snapshot.peers.iter().find(|p| p.name == name).map(|p| p.id.clone())
        };
        if let Some(id) = id {
            controller.show_full_profile(&id);
        }
    }

    let _hotkeys = setup_hotkeys(&engine);
    // L'interface relit l'état du moteur toutes les 150 ms, et seulement s'il a changé.
    let _timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            0.15,
            as_target(&controller),
            sel!(tick:),
            None,
            true,
        )
    };

    window.center();
    // Lancé à l'ouverture de la session : en arrière-plan, fenêtre cachée.
    if !magicplus_core::autostart::launched_at_login() {
        window.makeKeyAndOrderFront(None);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        // Après une mise à jour : les nouveautés, une fois la fenêtre affichée.
        let _ = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                0.6,
                as_target(&controller),
                sel!(whatsNewAtLaunch:),
                None,
                false,
            )
        };
    }
    app.run();
}
