//! Canevas dans l'interface Mac : liste des canevas de la conversation, éditeur Markdown avec
//! barre de mise en forme et aperçu en direct, présence (« X modifie… ») et historique des versions.

use super::chat_page::{bubble, scrolling_stack};
use super::markdown_view::markdown_view;
use super::*;
use crate::shared::time_label;
use magicplus_core::{
    canvas::{apply_format, transform_offset, COLORS, FORMATS},
    CanvasVersionView, CanvasView,
};
use objc2_app_kit::{NSControlSize, NSLayoutConstraintOrientation, NSLineBreakMode, NSStackViewDistribution, NSTextInputClient};
use objc2_foundation::NSRange;
use std::time::Instant;

pub(super) struct CanvasWidgets {
    root: Retained<NSView>,
    /// Vues de la discussion, cachées pendant que les canevas sont ouverts.
    chat_views: Vec<Retained<NSView>>,
    toggle: Retained<NSButton>,
    list: Retained<NSStackView>,
    empty: Retained<NSTextField>,
    main: Retained<NSView>,
    title: Retained<NSTextField>,
    editing: Retained<NSTextField>,
    history_button: Retained<NSButton>,
    body: Retained<NSStackView>,
    editor: Retained<NSTextView>,
    preview_scroll: Retained<NSScrollView>,
    preview: Retained<NSStackView>,
    history: Retained<NSView>,
    versions: Retained<NSStackView>,
    version_scroll: Retained<NSScrollView>,
    version_preview: Retained<NSStackView>,
    version_info: Retained<NSTextField>,
    restore: Retained<NSButton>,
    name_version: Retained<NSButton>,
}

#[derive(Default)]
pub(super) struct CanvasUi {
    open: bool,
    conversation: Option<String>,
    current: Option<String>,
    ids: Vec<String>,
    seen: u64,
    list_signature: String,
    /// Texte affiché dans l'éditeur (dernier connu du moteur).
    text: String,
    preview_text: Option<String>,
    preview_at: Option<Instant>,
    preview_width: f64,
    history: bool,
    versions: Vec<CanvasVersionView>,
    selected_version: Option<usize>,
}

fn utf16_to_byte(text: &str, offset: usize) -> usize {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        if units >= offset {
            return index;
        }
        units += c.len_utf16();
    }
    text.len()
}

fn byte_to_utf16(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].encode_utf16().count()
}

fn pin_edges(view: &NSView, to: &NSView, inset: f64) {
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.leadingAnchor().constraintEqualToAnchor_constant(&to.leadingAnchor(), inset).setActive(true);
    to.trailingAnchor().constraintEqualToAnchor_constant(&view.trailingAnchor(), inset).setActive(true);
}

/// Construit la vue des canevas (cachée au départ).
pub(super) fn build(
    controller: &Controller,
    toggle: Retained<NSButton>,
    chat_views: Vec<Retained<NSView>>,
    mtm: MainThreadMarker,
) -> Retained<NSView> {
    let root = NSView::new(mtm);
    root.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.setHidden(true);

    // ---- liste à gauche
    let left = stack(true, 8.0, mtm);
    left.setEdgeInsets(NSEdgeInsets { top: 12.0, left: 12.0, bottom: 12.0, right: 12.0 });
    root.addSubview(&left);
    left.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    left.bottomAnchor().constraintEqualToAnchor(&root.bottomAnchor()).setActive(true);
    left.leadingAnchor().constraintEqualToAnchor(&root.leadingAnchor()).setActive(true);
    left.widthAnchor().constraintEqualToConstant(220.0).setActive(true);
    let heading = stack(false, 8.0, mtm);
    heading.addArrangedSubview(&strong("Canevas", 15.0, mtm));
    let new = button("Nouveau", sel!(canvasNew:), controller, mtm);
    new.setControlSize(NSControlSize::Small);
    heading.addView_inGravity(&new, NSStackViewGravity::Trailing);
    full_width(&left, &heading);
    let (list_scroll, _, list) = scrolling_stack(3.0, 0.0, 0.0, mtm);
    full_width(&left, &list_scroll);
    list_scroll.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);
    let line = NSBox::new(mtm);
    line.setBoxType(NSBoxType::Separator);
    line.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(&line);
    line.leadingAnchor().constraintEqualToAnchor(&left.trailingAnchor()).setActive(true);
    line.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    line.bottomAnchor().constraintEqualToAnchor(&root.bottomAnchor()).setActive(true);
    line.widthAnchor().constraintEqualToConstant(1.0).setActive(true);

    // ---- partie droite
    let right = NSView::new(mtm);
    right.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(&right);
    right.leadingAnchor().constraintEqualToAnchor(&line.trailingAnchor()).setActive(true);
    right.trailingAnchor().constraintEqualToAnchor(&root.trailingAnchor()).setActive(true);
    right.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    right.bottomAnchor().constraintEqualToAnchor(&root.bottomAnchor()).setActive(true);

    let empty = paragraph(
        "Un canevas est une page que tous les membres écrivent ensemble, en direct : listes, tableaux, titres, couleurs, code… \
         Cliquez sur « Nouveau » pour en créer un.",
        mtm,
    );
    empty.setTextColor(Some(&NSColor::secondaryLabelColor()));
    empty.setAlignment(NSTextAlignment::Center);
    right.addSubview(&empty);
    empty.setTranslatesAutoresizingMaskIntoConstraints(false);
    empty.centerYAnchor().constraintEqualToAnchor(&right.centerYAnchor()).setActive(true);
    pin_edges(&empty, &right, 60.0);
    empty.setPreferredMaxLayoutWidth(420.0);

    let main = NSView::new(mtm);
    main.setTranslatesAutoresizingMaskIntoConstraints(false);
    right.addSubview(&main);
    pin(&main, &right, 12.0, 16.0, 12.0);

    // En-tête : titre, présence, actions.
    let top = stack(false, 10.0, mtm);
    main.addSubview(&top);
    top.topAnchor().constraintEqualToAnchor(&main.topAnchor()).setActive(true);
    pin_edges(&top, &main, 0.0);
    let title = strong("", 18.0, mtm);
    title.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    title.setContentCompressionResistancePriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
    top.addArrangedSubview(&title);
    let editing = secondary("", mtm);
    editing.setTextColor(Some(&NSColor::systemGreenColor()));
    top.addArrangedSubview(&editing);
    let delete = button("Supprimer", sel!(canvasDelete:), controller, mtm);
    let rename = button("Renommer…", sel!(canvasRename:), controller, mtm);
    let history_button = button("Historique", sel!(canvasHistory:), controller, mtm);
    for item in [&delete, &rename, &history_button] {
        item.setControlSize(NSControlSize::Small);
        top.addView_inGravity(item, NSStackViewGravity::Trailing);
    }

    // Barre de mise en forme.
    let toolbar = stack(false, 4.0, mtm);
    main.addSubview(&toolbar);
    toolbar.topAnchor().constraintEqualToAnchor_constant(&top.bottomAnchor(), 8.0).setActive(true);
    toolbar.leadingAnchor().constraintEqualToAnchor(&main.leadingAnchor()).setActive(true);
    for (index, (_, label_text, tooltip)) in FORMATS.iter().enumerate() {
        let item = button(label_text, sel!(canvasFormat:), controller, mtm);
        item.setControlSize(NSControlSize::Small);
        item.setTag(index as isize);
        item.setToolTip(Some(&ns(tooltip)));
        toolbar.addArrangedSubview(&item);
    }
    let colors = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(90.0, 22.0)), true);
    colors.setControlSize(NSControlSize::Small);
    colors.addItemWithTitle(&ns("Couleur"));
    for (name, _) in COLORS {
        colors.addItemWithTitle(&ns(name));
    }
    unsafe {
        colors.setTarget(Some(as_target(controller)));
        colors.setAction(Some(sel!(canvasColor:)));
    }
    toolbar.addArrangedSubview(&colors);

    // Corps : éditeur et aperçu côte à côte.
    let body = stack(false, 12.0, mtm);
    body.setDistribution(NSStackViewDistribution::FillEqually);
    body.setAlignment(NSLayoutAttribute::Height);
    main.addSubview(&body);
    body.topAnchor().constraintEqualToAnchor_constant(&toolbar.bottomAnchor(), 8.0).setActive(true);
    body.bottomAnchor().constraintEqualToAnchor(&main.bottomAnchor()).setActive(true);
    pin_edges(&body, &main, 0.0);
    let (editor_scroll, editor) = text_area(true, mtm);
    if let Some(constraint) = editor_scroll.constraints().iter().find(|c| c.firstAttribute() == NSLayoutAttribute::Height) {
        constraint.setActive(false);
    }
    editor.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(13.0, unsafe { objc2_app_kit::NSFontWeightRegular })));
    editor.setAllowsUndo(true);
    editor.setAutomaticQuoteSubstitutionEnabled(false);
    editor.setAutomaticDashSubstitutionEnabled(false);
    editor.setDelegate(Some(ProtocolObject::from_ref(controller)));
    body.addArrangedSubview(&editor_scroll);
    let (preview_scroll, _, preview) = scrolling_stack(5.0, 10.0, 12.0, mtm);
    preview_scroll.setBorderType(NSBorderType::BezelBorder);
    body.addArrangedSubview(&preview_scroll);

    // Historique (à la place du corps).
    let history = NSView::new(mtm);
    history.setTranslatesAutoresizingMaskIntoConstraints(false);
    history.setHidden(true);
    main.addSubview(&history);
    history.topAnchor().constraintEqualToAnchor(&body.topAnchor()).setActive(true);
    history.bottomAnchor().constraintEqualToAnchor(&body.bottomAnchor()).setActive(true);
    pin_edges(&history, &main, 0.0);
    let (version_scroll, _, versions) = scrolling_stack(3.0, 4.0, 0.0, mtm);
    history.addSubview(&version_scroll);
    version_scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
    version_scroll.leadingAnchor().constraintEqualToAnchor(&history.leadingAnchor()).setActive(true);
    version_scroll.topAnchor().constraintEqualToAnchor(&history.topAnchor()).setActive(true);
    version_scroll.bottomAnchor().constraintEqualToAnchor(&history.bottomAnchor()).setActive(true);
    version_scroll.widthAnchor().constraintEqualToConstant(290.0).setActive(true);
    let detail = stack(true, 8.0, mtm);
    history.addSubview(&detail);
    detail.setTranslatesAutoresizingMaskIntoConstraints(false);
    detail.leadingAnchor().constraintEqualToAnchor_constant(&version_scroll.trailingAnchor(), 12.0).setActive(true);
    detail.trailingAnchor().constraintEqualToAnchor(&history.trailingAnchor()).setActive(true);
    detail.topAnchor().constraintEqualToAnchor(&history.topAnchor()).setActive(true);
    detail.bottomAnchor().constraintEqualToAnchor(&history.bottomAnchor()).setActive(true);
    let version_info = secondary("Choisissez une version à gauche pour la voir.", mtm);
    full_width(&detail, &version_info);
    let actions = stack(false, 8.0, mtm);
    let restore = button("Restaurer cette version", sel!(canvasRestore:), controller, mtm);
    let name_version = button("Nommer…", sel!(canvasNameVersion:), controller, mtm);
    let close = button("Fermer l'historique", sel!(canvasHistory:), controller, mtm);
    for item in [&restore, &name_version, &close] {
        item.setControlSize(NSControlSize::Small);
        actions.addArrangedSubview(item);
    }
    full_width(&detail, &actions);
    let (version_preview_scroll, _, version_preview) = scrolling_stack(5.0, 10.0, 12.0, mtm);
    version_preview_scroll.setBorderType(NSBorderType::BezelBorder);
    full_width(&detail, &version_preview_scroll);
    version_preview_scroll.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);

    let widgets = CanvasWidgets {
        root: root.clone(),
        chat_views,
        toggle,
        list,
        empty,
        main,
        title,
        editing,
        history_button,
        body,
        editor,
        preview_scroll,
        preview,
        history,
        versions,
        version_scroll,
        version_preview,
        version_info,
        restore,
        name_version,
    };
    let _ = controller.ivars().canvas_widgets.set(widgets);
    root
}

impl Controller {
    fn canvas_widgets(&self) -> Option<&CanvasWidgets> {
        self.ivars().canvas_widgets.get()
    }

    fn canvas_conversation(&self) -> Option<String> {
        self.ivars().chat.borrow().current_id()
    }

    /// Bouton de l'en-tête : bascule entre la discussion et ses canevas.
    pub(super) fn canvas_toggle(&self) {
        let open = !self.ivars().canvas.borrow().open;
        self.ivars().canvas.borrow_mut().open = open;
        self.canvas_apply_visibility();
        if open {
            self.canvas_refresh(true);
        }
    }

    /// Montre la discussion ou les canevas selon l'état (appelé aussi après chaque rafraîchissement).
    pub(super) fn canvas_apply_visibility(&self) {
        let Some(widgets) = self.canvas_widgets() else { return };
        let has_conversation = self.canvas_conversation().is_some();
        let open = self.ivars().canvas.borrow().open && has_conversation;
        widgets.root.setHidden(!open);
        for view in &widgets.chat_views {
            view.setHidden(open || !has_conversation);
        }
        let symbol = if open { "bubble.left.and.bubble.right" } else { "doc.richtext" };
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(symbol), None) {
            widgets.toggle.setImage(Some(&image));
        }
        widgets.toggle.setToolTip(Some(&ns(if open { "Revenir à la discussion" } else { "Canevas partagés : pages écrites ensemble" })));
    }

    /// Toutes les 150 ms : refait l'affichage si un canevas a changé, et l'aperçu si besoin.
    pub(super) fn canvas_tick(&self) {
        if !self.ivars().canvas.borrow().open {
            return;
        }
        let revision = self.engine().canvas_revision();
        let (seen, conversation_changed) = {
            let ui = self.ivars().canvas.borrow();
            (ui.seen, ui.conversation != self.canvas_conversation())
        };
        if revision != seen || conversation_changed {
            self.canvas_refresh(conversation_changed);
        }
        self.canvas_update_preview(false);
    }

    fn canvas_refresh(&self, conversation_changed: bool) {
        let mtm = self.mtm();
        let Some(widgets) = self.canvas_widgets() else { return };
        let Some(conversation) = self.canvas_conversation() else { return };
        let engine = self.engine();
        let canvases = engine.canvases(&conversation);
        {
            let mut ui = self.ivars().canvas.borrow_mut();
            if conversation_changed || ui.conversation.as_deref() != Some(&conversation) {
                ui.conversation = Some(conversation.clone());
                ui.current = None;
                ui.history = false;
                ui.list_signature.clear();
            }
            if ui.current.as_ref().is_none_or(|id| !canvases.iter().any(|c| &c.id == id)) {
                ui.current = canvases.first().map(|c| c.id.clone());
                ui.text = String::new();
                ui.preview_text = None;
                ui.history = false;
            }
            ui.seen = engine.canvas_revision();
        }
        let current = self.ivars().canvas.borrow().current.clone();

        // Liste.
        let signature: String = canvases
            .iter()
            .map(|c| format!("{}|{}|{}|{}\n", c.id, c.title, c.editing.join(","), current.as_deref() == Some(&c.id)))
            .collect();
        if self.ivars().canvas.borrow().list_signature != signature {
            clear(&widgets.list);
            for (index, canvas) in canvases.iter().enumerate() {
                let selected = current.as_deref() == Some(&canvas.id);
                let (row, inner) = bubble(if selected { 2 } else { 3 }, 6.0, 8.0, mtm);
                inner.addArrangedSubview(&strong(&format!("📄 {}", canvas.title), 13.0, mtm));
                let detail = if canvas.editing.is_empty() { format!("par {}", canvas.author) } else { format!("{} modifie…", canvas.editing.join(", ")) };
                inner.addArrangedSubview(&secondary(&detail, mtm));
                let hit = button("", sel!(canvasSelect:), self, mtm);
                hit.setBordered(false);
                hit.setTransparent(true);
                hit.setTag(index as isize);
                row.addSubview(&hit);
                pin(&hit, &row, 0.0, 0.0, 0.0);
                full_width(&widgets.list, &row);
            }
            let mut ui = self.ivars().canvas.borrow_mut();
            ui.ids = canvases.iter().map(|c| c.id.clone()).collect();
            ui.list_signature = signature;
        }

        let Some(canvas) = current.and_then(|id| canvases.into_iter().find(|c| c.id == id)) else {
            widgets.main.setHidden(true);
            widgets.empty.setHidden(false);
            return;
        };
        widgets.main.setHidden(false);
        widgets.empty.setHidden(true);
        widgets.title.setStringValue(&ns(&canvas.title));
        widgets.editing.setStringValue(&ns(&if canvas.editing.is_empty() {
            String::new()
        } else {
            format!("● {} {}…", canvas.editing.join(", "), if canvas.editing.len() > 1 { "modifient" } else { "modifie" })
        }));
        self.canvas_apply_text(&canvas);
        let history = self.ivars().canvas.borrow().history;
        widgets.body.setHidden(history);
        widgets.history.setHidden(!history);
        widgets.history_button.setTitle(&ns(if history { "Modifier" } else { "Historique" }));
        if history {
            self.canvas_refresh_history();
        }
    }

    /// Met le texte du moteur dans l'éditeur sans perdre la position du curseur.
    fn canvas_apply_text(&self, canvas: &CanvasView) {
        let Some(widgets) = self.canvas_widgets() else { return };
        let editor = &widgets.editor;
        let shown = editor.string().to_string();
        if shown == canvas.text {
            self.ivars().canvas.borrow_mut().text = canvas.text.clone();
            return;
        }
        if editor.hasMarkedText() {
            return; // saisie en cours (accents, émojis…) : on attend la fin
        }
        let selection = editor.selectedRange();
        let start = utf16_to_byte(&shown, selection.location);
        let end = utf16_to_byte(&shown, selection.location + selection.length);
        let new_start = transform_offset(&shown, &canvas.text, start);
        let new_end = transform_offset(&shown, &canvas.text, end);
        editor.setString(&ns(&canvas.text));
        let location = byte_to_utf16(&canvas.text, new_start);
        let length = byte_to_utf16(&canvas.text, new_end).saturating_sub(location);
        editor.setSelectedRange(NSRange::new(location, length));
        self.ivars().canvas.borrow_mut().text = canvas.text.clone();
    }

    /// L'utilisateur a tapé dans l'éditeur : la différence part tout de suite aux autres membres.
    pub(super) fn canvas_text_changed(&self, object: Option<&AnyObject>) -> bool {
        let Some(widgets) = self.canvas_widgets() else { return false };
        let editor: &AnyObject = &widgets.editor;
        if !object.is_some_and(|o| std::ptr::eq(o, editor)) {
            return false;
        }
        let (conversation, current) = {
            let ui = self.ivars().canvas.borrow();
            (ui.conversation.clone(), ui.current.clone())
        };
        if let (Some(conversation), Some(id)) = (conversation, current) {
            let text = widgets.editor.string().to_string();
            match self.engine().set_canvas_text(&conversation, &id, &text) {
                Ok(()) => self.ivars().canvas.borrow_mut().text = text,
                Err(error) => self.engine().set_status(error.to_string()),
            }
        }
        true
    }

    /// Aperçu refait au plus quatre fois par seconde (ou tout de suite si `now`).
    fn canvas_update_preview(&self, now: bool) {
        let Some(widgets) = self.canvas_widgets() else { return };
        let text = widgets.editor.string().to_string();
        let width = (widgets.preview_scroll.contentView().bounds().size.width - 24.0).max(200.0);
        let due = {
            let ui = self.ivars().canvas.borrow();
            let changed = ui.preview_text.as_deref() != Some(text.as_str()) || (ui.preview_width - width).abs() > 20.0;
            changed && (now || ui.preview_at.is_none_or(|at| at.elapsed().as_millis() > 250))
        };
        if !due {
            return;
        }
        clear(&widgets.preview);
        widgets.preview.addArrangedSubview(&markdown_view(&text, 13.5, width, self.mtm()));
        let mut ui = self.ivars().canvas.borrow_mut();
        ui.preview_text = Some(text);
        ui.preview_at = Some(Instant::now());
        ui.preview_width = width;
    }

    pub(super) fn canvas_select(&self, index: isize) {
        let id = self.ivars().canvas.borrow().ids.get(index.max(0) as usize).cloned();
        if let Some(id) = id {
            let mut ui = self.ivars().canvas.borrow_mut();
            ui.current = Some(id);
            ui.text.clear();
            ui.preview_text = None;
            ui.history = false;
            ui.list_signature.clear();
            drop(ui);
            self.canvas_refresh(false);
            self.canvas_update_preview(true);
        }
    }

    fn ask_text(&self, title: &str, info: &str, value: &str) -> Option<String> {
        let mtm = self.mtm();
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&ns(title));
        if !info.is_empty() {
            alert.setInformativeText(&ns(info));
        }
        let field = NSTextField::textFieldWithString(&ns(value), mtm);
        field.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 24.0)));
        alert.setAccessoryView(Some(&field));
        alert.addButtonWithTitle(&ns("Valider"));
        alert.addButtonWithTitle(&ns("Annuler"));
        (alert.runModal() == NSAlertFirstButtonReturn).then(|| field.stringValue().to_string())
    }

    pub(super) fn canvas_new(&self) {
        let Some(conversation) = self.canvas_conversation() else { return };
        let Some(title) = self.ask_text("Nouveau canevas", "Une page que tous les membres pourront écrire ensemble.", "") else { return };
        match self.engine().create_canvas(&conversation, &title) {
            Ok(id) => {
                let mut ui = self.ivars().canvas.borrow_mut();
                ui.current = Some(id);
                ui.history = false;
                ui.list_signature.clear();
                drop(ui);
                self.canvas_refresh(false);
                if let (Some(widgets), Some(window)) = (self.canvas_widgets(), self.canvas_widgets().and_then(|w| w.editor.window())) {
                    let responder: &NSResponder = &widgets.editor;
                    window.makeFirstResponder(Some(responder));
                }
            }
            Err(error) => self.engine().set_status(error.to_string()),
        }
    }

    fn current_canvas(&self) -> Option<(String, String)> {
        let ui = self.ivars().canvas.borrow();
        Some((ui.conversation.clone()?, ui.current.clone()?))
    }

    pub(super) fn canvas_rename(&self) {
        let Some((conversation, id)) = self.current_canvas() else { return };
        let title = self.canvas_widgets().map(|w| w.title.stringValue().to_string()).unwrap_or_default();
        if let Some(title) = self.ask_text("Renommer le canevas", "", &title) {
            if let Err(error) = self.engine().rename_canvas(&conversation, &id, &title) {
                self.engine().set_status(error.to_string());
            }
        }
    }

    pub(super) fn canvas_delete(&self) {
        let Some((conversation, id)) = self.current_canvas() else { return };
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&ns("Supprimer ce canevas ?"));
        alert.setInformativeText(&ns("Il disparaîtra aussi chez les autres membres, avec son historique."));
        alert.addButtonWithTitle(&ns("Supprimer"));
        alert.addButtonWithTitle(&ns("Annuler"));
        if alert.runModal() == NSAlertFirstButtonReturn {
            if let Err(error) = self.engine().delete_canvas(&conversation, &id) {
                self.engine().set_status(error.to_string());
            }
        }
    }

    /// Bouton de mise en forme (ou couleur) : agit sur la sélection de l'éditeur.
    pub(super) fn canvas_format(&self, kind: &str) {
        let Some(widgets) = self.canvas_widgets() else { return };
        let editor = &widgets.editor;
        let text = editor.string().to_string();
        let selection = editor.selectedRange();
        let start = utf16_to_byte(&text, selection.location);
        let end = utf16_to_byte(&text, selection.location + selection.length);
        let (new, new_start, new_end) = apply_format(&text, start, end, kind);
        if new == text {
            return;
        }
        // Remplacement par l'éditeur lui-même : annulable (Cmd+Z) et transmis comme une frappe.
        let all = NSRange::new(0, text.encode_utf16().count());
        let replacement = ns(&new);
        // SAFETY : `replacement` est une NSString, ce qu'attend cette méthode.
        unsafe { editor.insertText_replacementRange(&replacement, all) };
        let location = byte_to_utf16(&new, new_start);
        let length = byte_to_utf16(&new, new_end).saturating_sub(location);
        editor.setSelectedRange(NSRange::new(location, length));
        if let Some(window) = editor.window() {
            let responder: &NSResponder = editor;
            window.makeFirstResponder(Some(responder));
        }
        self.canvas_update_preview(true);
    }

    pub(super) fn canvas_format_index(&self, index: isize) {
        if let Some((kind, _, _)) = FORMATS.get(index.max(0) as usize) {
            self.canvas_format(kind);
        }
    }

    pub(super) fn canvas_color(&self, sender: &NSPopUpButton) {
        let index = sender.indexOfSelectedItem();
        if index >= 1 {
            if let Some((_, word)) = COLORS.get(index as usize - 1) {
                self.canvas_format(&format!("color:{word}"));
            }
        }
    }

    // ---------------------------------------------------------------- historique

    pub(super) fn canvas_history_toggle(&self) {
        let Some((conversation, id)) = self.current_canvas() else { return };
        let opening = !self.ivars().canvas.borrow().history;
        if opening {
            // La séance en cours devient une version, pour la voir tout de suite.
            let _ = self.engine().save_canvas_version(&conversation, &id);
        }
        {
            let mut ui = self.ivars().canvas.borrow_mut();
            ui.history = opening;
            ui.selected_version = None;
        }
        self.canvas_refresh(false);
    }

    fn canvas_refresh_history(&self) {
        let mtm = self.mtm();
        let Some(widgets) = self.canvas_widgets() else { return };
        let Some((conversation, id)) = self.current_canvas() else { return };
        let versions = self.engine().canvas_versions(&conversation, &id);
        clear(&widgets.versions);
        if versions.is_empty() {
            full_width(&widgets.versions, &paragraph("Pas encore de version : elles se créent toutes seules après chaque pause d'écriture.", mtm));
        }
        let selected = self.ivars().canvas.borrow().selected_version;
        for (index, version) in versions.iter().enumerate() {
            let (row, inner) = bubble(if selected == Some(index) { 2 } else { 3 }, 6.0, 8.0, mtm);
            let title = if version.label.is_empty() { time_label(version.at_ms) } else { format!("⭐ {}", version.label) };
            inner.addArrangedSubview(&strong(&title, 12.5, mtm));
            let detail = format!("{} · +{} −{}", version.authors.join(", "), version.added, version.removed);
            inner.addArrangedSubview(&secondary(&detail, mtm));
            let summary = secondary(&version.summary, mtm);
            summary.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            summary.widthAnchor().constraintLessThanOrEqualToConstant(260.0).setActive(true);
            inner.addArrangedSubview(&summary);
            let hit = button("", sel!(canvasVersionSelect:), self, mtm);
            hit.setBordered(false);
            hit.setTransparent(true);
            hit.setTag(index as isize);
            row.addSubview(&hit);
            pin(&hit, &row, 0.0, 0.0, 0.0);
            full_width(&widgets.versions, &row);
        }
        clear(&widgets.version_preview);
        let chosen = selected.and_then(|i| versions.get(i));
        widgets.restore.setEnabled(chosen.is_some());
        widgets.name_version.setEnabled(chosen.is_some());
        match chosen {
            Some(version) => {
                widgets.version_info.setStringValue(&ns(&format!(
                    "{} · {} · +{} −{} caractères",
                    time_label(version.at_ms),
                    version.authors.join(", "),
                    version.added,
                    version.removed
                )));
                let width = (widgets.version_scroll.frame().size.width.max(300.0)).max(300.0);
                widgets.version_preview.addArrangedSubview(&markdown_view(&version.text, 13.0, width, mtm));
            }
            None => widgets.version_info.setStringValue(&ns("Choisissez une version à gauche pour la voir.")),
        }
        self.ivars().canvas.borrow_mut().versions = versions;
    }

    pub(super) fn canvas_version_select(&self, index: isize) {
        self.ivars().canvas.borrow_mut().selected_version = Some(index.max(0) as usize);
        self.canvas_refresh_history();
    }

    fn selected_version(&self) -> Option<CanvasVersionView> {
        let ui = self.ivars().canvas.borrow();
        ui.selected_version.and_then(|i| ui.versions.get(i).cloned())
    }

    pub(super) fn canvas_restore(&self) {
        let (Some((conversation, id)), Some(version)) = (self.current_canvas(), self.selected_version()) else { return };
        match self.engine().restore_canvas_version(&conversation, &id, &version.id) {
            Ok(()) => {
                self.engine().set_status("Version restaurée (l'état précédent reste dans l'historique)");
                self.ivars().canvas.borrow_mut().history = false;
                self.canvas_refresh(false);
            }
            Err(error) => self.engine().set_status(error.to_string()),
        }
    }

    pub(super) fn canvas_name_version(&self) {
        let (Some((conversation, id)), Some(version)) = (self.current_canvas(), self.selected_version()) else { return };
        let Some(label) = self.ask_text("Nommer cette version", "Une version nommée n'est jamais supprimée de l'historique.", &version.label) else {
            return;
        };
        if let Err(error) = self.engine().name_canvas_version(&conversation, &id, &version.id, &label) {
            self.engine().set_status(error.to_string());
        }
        self.canvas_refresh_history();
    }
}
