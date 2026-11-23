//! Canevas (Windows) : relie le global `CanvasData` (ui/canvas.slint) au moteur.

use super::{chat::Chat, markdown, AppWindow, CanvasData, CanvasItem, FormatButton, FormatResult, VersionItem};
use crate::shared::{open_path, time_label};
use magicplus_core::{
    canvas::{apply_format, transform_offset, COLORS, FORMATS},
    CanvasVersionView, Engine,
};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::{cell::RefCell, rc::Rc, time::{Duration, Instant}};

#[derive(Default)]
struct State {
    conversation: Option<String>,
    current: Option<String>,
    ids: Vec<String>,
    seen: u64,
    /// Texte du moteur déjà donné à l'éditeur.
    text: String,
    /// Texte de l'aperçu et date de sa dernière mise à jour.
    preview: Option<String>,
    preview_at: Option<Instant>,
    versions: Vec<CanvasVersionView>,
    selected_version: Option<usize>,
    history: bool,
}

pub struct Canvas {
    engine: Engine,
    chat: Rc<Chat>,
    weak: slint::Weak<AppWindow>,
    state: RefCell<State>,
}

impl Canvas {
    pub fn new(engine: Engine, chat: Rc<Chat>, ui: &AppWindow) -> Rc<Self> {
        let canvas = Rc::new(Canvas { engine, chat, weak: ui.as_weak(), state: RefCell::new(State::default()) });
        let data = ui.global::<CanvasData>();
        data.set_formats(ModelRc::new(VecModel::from(
            FORMATS
                .iter()
                .map(|(kind, label, tooltip)| FormatButton { kind: (*kind).into(), label: (*label).into(), tooltip: (*tooltip).into() })
                .collect::<Vec<_>>(),
        )));
        data.set_colors(ModelRc::new(VecModel::from(COLORS.iter().map(|(_, word)| SharedString::from(*word)).collect::<Vec<_>>())));
        install(&canvas, &data);
        canvas
    }

    fn data(&self) -> Option<(AppWindow, ())> {
        self.weak.upgrade().map(|ui| (ui, ()))
    }

    /// Toutes les 150 ms : refait l'affichage si un canevas a changé.
    pub fn tick(&self) {
        let Some((ui, _)) = self.data() else { return };
        let data = ui.global::<CanvasData>();
        if !data.get_open() {
            return;
        }
        let conversation = self.chat.current_conversation();
        let (seen, changed) = {
            let st = self.state.borrow();
            (st.seen, st.conversation != conversation)
        };
        if changed || seen != self.engine.canvas_revision() {
            self.refresh(&ui, changed);
        }
        self.update_preview(&ui, false);
    }

    fn refresh(&self, ui: &AppWindow, conversation_changed: bool) {
        let data = ui.global::<CanvasData>();
        let Some(conversation) = self.chat.current_conversation() else {
            data.set_open(false);
            return;
        };
        let canvases = self.engine.canvases(&conversation);
        let mut st = self.state.borrow_mut();
        if conversation_changed || st.conversation.as_deref() != Some(&conversation) {
            st.conversation = Some(conversation.clone());
            st.current = None;
            st.history = false;
        }
        if st.current.as_ref().is_none_or(|id| !canvases.iter().any(|c| &c.id == id)) {
            st.current = canvases.first().map(|c| c.id.clone());
            st.text = String::new();
            st.preview = None;
            st.history = false;
        }
        st.seen = self.engine.canvas_revision();
        st.ids = canvases.iter().map(|c| c.id.clone()).collect();
        let current = st.current.clone();
        let items: Vec<CanvasItem> = canvases
            .iter()
            .map(|c| CanvasItem {
                title: c.title.clone().into(),
                detail: if c.editing.is_empty() { format!("par {}", c.author) } else { format!("{} modifie…", c.editing.join(", ")) }.into(),
                selected: current.as_deref() == Some(&c.id),
                editing: !c.editing.is_empty(),
            })
            .collect();
        data.set_canvases(ModelRc::new(VecModel::from(items)));

        let Some(canvas) = current.and_then(|id| canvases.into_iter().find(|c| c.id == id)) else {
            data.set_has_canvas(false);
            return;
        };
        data.set_has_canvas(true);
        data.set_title(canvas.title.clone().into());
        data.set_editing(
            match canvas.editing.len() {
                0 => String::new(),
                1 => format!("● {} modifie…", canvas.editing[0]),
                _ => format!("● {} modifient…", canvas.editing.join(", ")),
            }
            .into(),
        );
        data.set_history(st.history);
        if st.text != canvas.text {
            // Nouveau texte (d'un autre membre, ou autre canevas) : l'éditeur le reprend.
            st.text = canvas.text.clone();
            data.set_text(canvas.text.clone().into());
            data.set_remote_request(data.get_remote_request() + 1);
        }
        let history = st.history;
        drop(st);
        if history {
            self.refresh_history(ui);
        }
    }

    /// Aperçu refait au plus quatre fois par seconde (ou tout de suite si `now`).
    fn update_preview(&self, ui: &AppWindow, now: bool) {
        let text = self.state.borrow().text.clone();
        let due = {
            let st = self.state.borrow();
            st.preview.as_deref() != Some(text.as_str()) && (now || st.preview_at.is_none_or(|at| at.elapsed() > Duration::from_millis(250)))
        };
        if due {
            ui.global::<CanvasData>().set_preview(markdown::blocks(&text));
            let mut st = self.state.borrow_mut();
            st.preview = Some(text);
            st.preview_at = Some(Instant::now());
        }
    }

    fn current(&self) -> Option<(String, String)> {
        let st = self.state.borrow();
        Some((st.conversation.clone()?, st.current.clone()?))
    }

    fn edited(&self, text: &str) {
        let Some((conversation, id)) = self.current() else { return };
        match self.engine.set_canvas_text(&conversation, &id, text) {
            Ok(()) => {
                let mut st = self.state.borrow_mut();
                st.text = text.to_string();
                st.seen = self.engine.canvas_revision();
            }
            Err(error) => self.engine.set_status(error.to_string()),
        }
    }

    fn refresh_history(&self, ui: &AppWindow) {
        let Some((conversation, id)) = self.current() else { return };
        let versions = self.engine.canvas_versions(&conversation, &id);
        let data = ui.global::<CanvasData>();
        let selected = self.state.borrow().selected_version;
        let items: Vec<VersionItem> = versions
            .iter()
            .enumerate()
            .map(|(index, v)| VersionItem {
                title: if v.label.is_empty() { time_label(v.at_ms) } else { format!("⭐ {}", v.label) }.into(),
                detail: format!("{} · +{} −{}", v.authors.join(", "), v.added, v.removed).into(),
                summary: v.summary.replace('☐', "⬜").replace('☑', "✅").into(),
                selected: selected == Some(index),
            })
            .collect();
        data.set_versions(ModelRc::new(VecModel::from(items)));
        match selected.and_then(|i| versions.get(i)) {
            Some(version) => {
                data.set_has_version(true);
                data.set_version_info(
                    format!("{} · {} · +{} −{} caractères", time_label(version.at_ms), version.authors.join(", "), version.added, version.removed).into(),
                );
                data.set_version_label(version.label.clone().into());
                data.set_version_preview(markdown::blocks(&version.text));
            }
            None => {
                data.set_has_version(false);
                data.set_version_info("Choisissez une version à gauche pour la voir.".into());
                data.set_version_preview(ModelRc::default());
            }
        }
        self.state.borrow_mut().versions = versions;
    }

    fn selected_version(&self) -> Option<CanvasVersionView> {
        let st = self.state.borrow();
        st.selected_version.and_then(|i| st.versions.get(i).cloned())
    }
}

fn install(canvas: &Rc<Canvas>, data: &CanvasData) {
    macro_rules! on {
        ($setter:ident, |$canvas:ident $(, $arg:ident)*| $body:expr) => {{
            let weak = Rc::downgrade(canvas);
            data.$setter(move |$($arg),*| {
                #[allow(unused_variables)]
                let Some($canvas) = weak.upgrade() else { return Default::default() };
                $body
            });
        }};
    }
    on!(on_toggle, |canvas| {
        if let Some((ui, _)) = canvas.data() {
            let data = ui.global::<CanvasData>();
            let open = !data.get_open() && canvas.chat.current_conversation().is_some();
            data.set_open(open);
            if open {
                canvas.state.borrow_mut().text.clear();
                canvas.refresh(&ui, true);
                canvas.update_preview(&ui, true);
            }
        }
    });
    on!(on_select, |canvas, index| {
        let id = canvas.state.borrow().ids.get(index.max(0) as usize).cloned();
        if let (Some(id), Some((ui, _))) = (id, canvas.data()) {
            {
                let mut st = canvas.state.borrow_mut();
                st.current = Some(id);
                st.text.clear();
                st.history = false;
            }
            canvas.refresh(&ui, false);
            canvas.update_preview(&ui, true);
        }
    });
    on!(on_create, |canvas| {
        let Some(conversation) = canvas.chat.current_conversation() else { return };
        match canvas.engine.create_canvas(&conversation, "Nouveau canevas") {
            Ok(id) => {
                if let Some((ui, _)) = canvas.data() {
                    {
                        let mut st = canvas.state.borrow_mut();
                        st.current = Some(id);
                        st.text.clear();
                        st.history = false;
                    }
                    canvas.refresh(&ui, false);
                    canvas.engine.set_status("Canevas créé : donnez-lui un titre en haut, puis écrivez");
                }
            }
            Err(error) => canvas.engine.set_status(error.to_string()),
        }
    });
    on!(on_rename, |canvas, title| {
        if let Some((conversation, id)) = canvas.current() {
            if let Err(error) = canvas.engine.rename_canvas(&conversation, &id, &title) {
                canvas.engine.set_status(error.to_string());
            }
        }
    });
    on!(on_delete, |canvas| {
        if let Some((conversation, id)) = canvas.current() {
            if let Err(error) = canvas.engine.delete_canvas(&conversation, &id) {
                canvas.engine.set_status(error.to_string());
            }
        }
    });
    on!(on_edited, |canvas, text| canvas.edited(&text));
    on!(on_transform, |canvas, old, new, offset| transform_offset(&old, &new, offset.max(0) as usize) as i32);
    on!(on_format, |canvas, kind, text, anchor, cursor| {
        let (new, start, end) = apply_format(&text, anchor.max(0) as usize, cursor.max(0) as usize, &kind);
        FormatResult { text: new.into(), anchor: start as i32, cursor: end as i32 }
    });
    on!(on_history_toggle, |canvas| {
        let Some((conversation, id)) = canvas.current() else { return };
        let opening = !canvas.state.borrow().history;
        if opening {
            let _ = canvas.engine.save_canvas_version(&conversation, &id);
        }
        {
            let mut st = canvas.state.borrow_mut();
            st.history = opening;
            st.selected_version = None;
        }
        if let Some((ui, _)) = canvas.data() {
            canvas.refresh(&ui, false);
            canvas.refresh_history(&ui);
        }
    });
    on!(on_version_select, |canvas, index| {
        canvas.state.borrow_mut().selected_version = Some(index.max(0) as usize);
        if let Some((ui, _)) = canvas.data() {
            canvas.refresh_history(&ui);
        }
    });
    on!(on_restore, |canvas| {
        let (Some((conversation, id)), Some(version)) = (canvas.current(), canvas.selected_version()) else { return };
        match canvas.engine.restore_canvas_version(&conversation, &id, &version.id) {
            Ok(()) => {
                canvas.engine.set_status("Version restaurée (l'état précédent reste dans l'historique)");
                canvas.state.borrow_mut().history = false;
                if let Some((ui, _)) = canvas.data() {
                    canvas.refresh(&ui, false);
                }
            }
            Err(error) => canvas.engine.set_status(error.to_string()),
        }
    });
    on!(on_name_version, |canvas, label| {
        let (Some((conversation, id)), Some(version)) = (canvas.current(), canvas.selected_version()) else { return };
        if let Err(error) = canvas.engine.name_canvas_version(&conversation, &id, &version.id, &label) {
            canvas.engine.set_status(error.to_string());
        }
        if let Some((ui, _)) = canvas.data() {
            canvas.refresh_history(&ui);
        }
    });
    on!(on_open_link, |canvas, url| {
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:") {
            open_path(&url);
        }
    });
}
