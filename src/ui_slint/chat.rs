//! Page « Discussions » de l'interface Slint (Windows) : relie le global `ChatData`
//! (ui/chat.slint) au moteur. Mêmes fonctions que la page AppKit du Mac (src/ui_mac_chat.rs).

use super::{AppWindow, AttachmentItem, ChatData, ChoiceItem, ConversationItem, MemberItem, MessageItem, ReactionItem, StickerItem};
use crate::shared::{avatar_hue, initials, jumbo, open_path, time_label};
use magicplus_core::{
    markdown, protocol::MAX_ATTACHMENTS, util::format_size, AttachmentKind, AttachmentView, ConversationKind, ConversationView,
    Engine, MessageStatus, MessageView,
};
use slint::{Color, ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, StyledText, VecModel};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

/// Numéro de la page dans `AppWindow.page`.
pub const CHAT_PAGE: i32 = 4;
const MESSAGE_LIMIT: usize = 300;
/// Au-delà de cet écart, un nouveau message de la même personne affiche à nouveau son nom.
const GROUP_WINDOW_MS: u64 = 5 * 60 * 1000;
const PENDING_SHOWN: usize = 4;
/// Mémoire maximale des images d'un GIF ; au-delà, une image sur deux (puis trois…) est gardée.
const GIF_BUDGET_BYTES: usize = 24 * 1024 * 1024;
const MAX_GIF_FRAMES: usize = 500;

// ------------------------------------------------------------------ images et GIF

/// Cadre d'affichage d'une image.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Frame {
    Photo,
    Sticker,
    Thumb,
}

impl Frame {
    /// Taille maximale à l'écran, en pixels logiques.
    fn bounds(self) -> (f32, f32) {
        match self {
            Frame::Photo => (360.0, 270.0),
            Frame::Sticker => (150.0, 150.0),
            Frame::Thumb => (68.0, 68.0),
        }
    }
}

type VisualKey = (PathBuf, Frame);
/// Images décodées par le fil de travail, en attente d'être rangées par l'interface.
type DecodedInbox = Arc<Mutex<Vec<(VisualKey, Option<Decoded>)>>>;

/// Image décodée dans le fil de travail (envoyable d'un fil à l'autre).
struct Decoded {
    frames: Vec<SharedPixelBuffer<Rgba8Pixel>>,
    frame_ms: i32,
    width: f32,
    height: f32,
}

#[derive(Clone)]
struct Ready {
    still: Image,
    frames: Vec<Image>,
    frame_ms: i32,
    width: f32,
    height: f32,
}

enum Visual {
    Loading,
    Ready(Ready),
    Failed,
}

fn fit(width: u32, height: u32, (max_w, max_h): (f32, f32)) -> (f32, f32) {
    let scale = (max_w / width.max(1) as f32).min(max_h / height.max(1) as f32).min(1.0);
    ((width as f32 * scale).max(24.0), (height as f32 * scale).max(24.0))
}

fn pixel_buffer(image: &image::RgbaImage) -> SharedPixelBuffer<Rgba8Pixel> {
    SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(image.as_raw(), image.width(), image.height())
}

/// Décode une image ou toutes les images d'un GIF, réduites à leur taille d'affichage.
fn decode(path: &Path, frame: Frame) -> Option<Decoded> {
    use image::{imageops::FilterType, AnimationDecoder, ImageFormat, ImageReader};
    let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    if reader.format() == Some(ImageFormat::Gif) {
        let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
        let decoder = image::codecs::gif::GifDecoder::new(file).ok()?;
        let mut images = Vec::new();
        let mut total_ms: u64 = 0;
        let mut size = None;
        for next in decoder.into_frames().take(MAX_GIF_FRAMES) {
            let Ok(gif_frame) = next else { break };
            let (numer, denom) = gif_frame.delay().numer_denom_ms();
            total_ms += numer.checked_div(denom).map_or(100, |ms| ms.max(20)) as u64;
            let buffer = gif_frame.into_buffer();
            let (w, h) = *size.get_or_insert_with(|| {
                let (w, h) = fit(buffer.width(), buffer.height(), frame.bounds());
                (w.round() as u32, h.round() as u32)
            });
            images.push(if buffer.dimensions() == (w, h) { buffer } else { image::imageops::resize(&buffer, w, h, FilterType::Triangle) });
        }
        let (w, h) = size?;
        let per_frame = (w * h * 4) as usize;
        let step = (per_frame * images.len()).div_ceil(GIF_BUDGET_BYTES).max(1);
        let kept: Vec<_> = images.iter().step_by(step).map(pixel_buffer).collect();
        let frame_ms = (total_ms / kept.len().max(1) as u64).clamp(20, 2000) as i32;
        return Some(Decoded { frames: kept, frame_ms, width: w as f32, height: h as f32 });
    }
    let image = reader.decode().ok()?;
    let (w, h) = fit(image.width(), image.height(), frame.bounds());
    // Décodée au double de sa taille d'affichage pour rester nette sur les écrans à haute densité.
    let (max_w, max_h) = ((w * 2.0) as u32, (h * 2.0) as u32);
    let image = if image.width() > max_w || image.height() > max_h { image.thumbnail(max_w, max_h) } else { image };
    Some(Decoded { frames: vec![pixel_buffer(&image.to_rgba8())], frame_ms: 0, width: w, height: h })
}

fn avatar_color(seed: &str) -> Color {
    // Même teinte que sur le Mac (saturation 0,45, luminosité 0,72).
    let (s, v) = (0.45_f32, 0.72_f32);
    let h = avatar_hue(seed) * 6.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    Color::from_rgb_f32(r + m, g + m, b + m)
}

fn styled(markdown_text: &str) -> StyledText {
    StyledText::from_markdown(&markdown::to_commonmark(markdown_text)).unwrap_or_else(|error| {
        if std::env::var_os("MAGICPLUS_DEBUG").is_some() {
            eprintln!("[discussions] Markdown refusé par Slint : {error}");
        }
        StyledText::from_plain_text(&markdown::plain(markdown_text))
    })
}

fn status_label(message: &MessageView) -> String {
    match &message.status {
        MessageStatus::Sending => "Envoi… (partira dès que le destinataire est joignable)".to_string(),
        MessageStatus::Delivered => "Distribué".to_string(),
        MessageStatus::Read(names) if names.len() <= 1 => "Lu".to_string(),
        MessageStatus::Read(names) => format!("Lu par {}", names.join(", ")),
        MessageStatus::None => String::new(),
    }
}

// ------------------------------------------------------------------ état

#[derive(Default)]
struct State {
    current: Option<String>,
    seen_revision: u64,
    was_visible: bool,
    list_ids: Vec<String>,
    list_signature: String,
    /// Messages affichés et empreinte de leur rendu (pour ne refaire que les lignes changées).
    messages: Vec<MessageView>,
    signatures: Vec<String>,
    conversation: Option<ConversationView>,
    /// Réponse en préparation : identifiant, auteur, extrait.
    reply_to: Option<(String, String, String)>,
    editing: Option<String>,
    pending: Vec<PathBuf>,
    delete_target: Option<String>,
    choice_ids: Vec<String>,
    choice_checked: Vec<bool>,
    has_progress: bool,
    last_refresh: Option<Instant>,
    visuals: HashMap<VisualKey, Visual>,
    used: HashSet<VisualKey>,
    animated: bool,
    sticker_signature: String,
}

pub struct Chat {
    engine: Engine,
    weak: slint::Weak<AppWindow>,
    state: RefCell<State>,
    conversations: Rc<VecModel<ConversationItem>>,
    messages: Rc<VecModel<MessageItem>>,
    stickers: Rc<VecModel<StickerItem>>,
    decode_requests: mpsc::Sender<VisualKey>,
    decoded: DecodedInbox,
}

impl Chat {
    pub fn new(engine: Engine, ui: &AppWindow) -> Rc<Self> {
        let (sender, receiver) = mpsc::channel::<VisualKey>();
        let decoded: DecodedInbox = Arc::new(Mutex::new(Vec::new()));
        {
            // Un seul fil décode les images, l'une après l'autre, sans bloquer l'interface.
            let decoded = decoded.clone();
            thread::spawn(move || {
                for key in receiver {
                    let result = decode(&key.0, key.1);
                    decoded.lock().unwrap_or_else(|e| e.into_inner()).push((key, result));
                }
            });
        }
        let chat = Rc::new(Chat {
            engine,
            weak: ui.as_weak(),
            state: RefCell::new(State::default()),
            conversations: Rc::new(VecModel::default()),
            messages: Rc::new(VecModel::default()),
            stickers: Rc::new(VecModel::default()),
            decode_requests: sender,
            decoded,
        });
        let data = ui.global::<ChatData>();
        data.set_conversations(ModelRc::from(chat.conversations.clone()));
        data.set_messages(ModelRc::from(chat.messages.clone()));
        data.set_stickers(ModelRc::from(chat.stickers.clone()));
        install(&chat, &data);
        chat
    }

    fn ui(&self) -> Option<AppWindow> {
        self.weak.upgrade()
    }

    fn current(&self) -> Option<String> {
        self.state.borrow().current.clone()
    }

    fn status(&self, text: impl Into<String>) {
        self.engine.set_status(text);
    }

    /// Une GIF est affichée : l'animation doit avancer.
    pub fn animating(&self) -> bool {
        self.state.borrow().animated
    }

    /// Appelé toutes les 150 ms : ne refait l'affichage que si quelque chose a changé.
    pub fn tick(&self) {
        let Some(ui) = self.ui() else { return };
        let visible = ui.get_page() == CHAT_PAGE;
        let decoded = self.collect_decoded();
        let revision = self.engine.chat_revision();
        let (due, appeared) = {
            let mut st = self.state.borrow_mut();
            let progress_due = st.has_progress && st.last_refresh.is_none_or(|at| at.elapsed() > Duration::from_millis(700));
            let appeared = visible && !st.was_visible;
            st.was_visible = visible;
            (revision != st.seen_revision || progress_due || decoded, appeared)
        };
        if appeared {
            self.refresh(&ui, true);
            self.scroll_to_bottom(&ui);
        } else if due {
            self.refresh(&ui, false);
        }
    }

    /// Range les images décodées par le fil de travail. Vrai s'il y en a de nouvelles.
    fn collect_decoded(&self) -> bool {
        let finished: Vec<_> = std::mem::take(&mut *self.decoded.lock().unwrap_or_else(|e| e.into_inner()));
        if finished.is_empty() {
            return false;
        }
        let mut st = self.state.borrow_mut();
        for (key, result) in finished {
            let visual = match result {
                Some(decoded) => {
                    let frames: Vec<Image> = decoded.frames.into_iter().map(Image::from_rgba8).collect();
                    Visual::Ready(Ready {
                        still: frames.first().cloned().unwrap_or_default(),
                        frames: if frames.len() > 1 { frames } else { Vec::new() },
                        frame_ms: decoded.frame_ms,
                        width: decoded.width,
                        height: decoded.height,
                    })
                }
                None => Visual::Failed,
            };
            st.visuals.insert(key, visual);
        }
        true
    }

    /// État d'une image ; la demande au fil de travail si elle n'a jamais été vue.
    fn visual(&self, st: &mut State, path: &Path, frame: Frame) -> Option<Option<Ready>> {
        let key = (path.to_path_buf(), frame);
        st.used.insert(key.clone());
        match st.visuals.get(&key) {
            Some(Visual::Ready(ready)) => Some(Some(ready.clone())),
            Some(Visual::Loading) => Some(None),
            Some(Visual::Failed) => None,
            None => {
                st.visuals.insert(key.clone(), Visual::Loading);
                let _ = self.decode_requests.send(key);
                Some(None)
            }
        }
    }

    // ---------------------------------------------------------------- affichage

    fn refresh(&self, ui: &AppWindow, force: bool) {
        let data = ui.global::<ChatData>();
        let engine = &self.engine;
        let revision = engine.chat_revision();
        let conversations = engine.conversations();
        let visible = ui.get_page() == CHAT_PAGE;
        data.set_unread(conversations.iter().map(|c| c.unread).sum::<usize>() as i32);

        // Conversation affichée : toujours une qui existe ; à l'ouverture de la page, la plus récente.
        let mut force = force;
        let current = {
            let mut st = self.state.borrow_mut();
            if st.current.as_ref().is_some_and(|id| !conversations.iter().any(|c| &c.id == id)) {
                st.current = None;
            }
            if st.current.is_none() && visible {
                if let Some(first) = conversations.first() {
                    st.current = Some(first.id.clone());
                    force = true;
                }
            }
            st.current.clone()
        };
        self.refresh_list(&conversations, current.as_deref());

        let Some(conversation) = current.and_then(|id| conversations.into_iter().find(|c| c.id == id)) else {
            data.set_has_conversation(false);
            if self.messages.row_count() > 0 {
                self.messages.set_vec(Vec::new());
            }
            let mut st = self.state.borrow_mut();
            st.conversation = None;
            st.messages.clear();
            st.signatures.clear();
            st.animated = false;
            st.seen_revision = revision;
            st.last_refresh = Some(Instant::now());
            return;
        };

        data.set_has_conversation(true);
        data.set_title(conversation.title.clone().into());
        let others: Vec<_> = conversation.members.iter().filter(|m| !m.me).collect();
        let group = conversation.kind == ConversationKind::Group;
        let subtitle = if group {
            let names: Vec<String> = conversation
                .members
                .iter()
                .map(|m| if m.me { "vous".to_string() } else if m.online { m.name.clone() } else { format!("{} (hors ligne)", m.name) })
                .collect();
            format!("{} membres · {}", conversation.members.len(), names.join(", "))
        } else {
            match others.first() {
                Some(member) if member.online => "En ligne · chiffré de bout en bout".to_string(),
                Some(_) => "Hors ligne · les messages partiront à son retour".to_string(),
                None => String::new(),
            }
        };
        data.set_subtitle(subtitle.into());
        let seed = if group { conversation.id.clone() } else { others.first().map(|m| m.id.clone()).unwrap_or_default() };
        data.set_initials(initials(&conversation.title).into());
        data.set_color(avatar_color(&seed));
        data.set_is_group(group);
        data.set_online(others.iter().any(|m| m.online));
        data.set_is_creator(conversation.is_creator);
        data.set_closed(conversation.closed);
        let members: Vec<MemberItem> = conversation
            .members
            .iter()
            .map(|m| MemberItem {
                name: m.name.clone().into(),
                state: (if m.me { "vous" } else if m.online { "en ligne" } else { "hors ligne" }).into(),
            })
            .collect();
        data.set_members(ModelRc::new(VecModel::from(members)));
        data.set_typing(
            match conversation.typing.len() {
                0 => String::new(),
                1 => format!("{} écrit…", conversation.typing[0]),
                _ => format!("{} écrivent…", conversation.typing.join(", ")),
            }
            .into(),
        );

        if !visible && !force {
            let mut st = self.state.borrow_mut();
            st.conversation = Some(conversation);
            st.seen_revision = revision;
            st.last_refresh = Some(Instant::now());
            return;
        }

        let messages = engine.messages(&conversation.id, MESSAGE_LIMIT);
        data.set_history_start(messages.len() < MESSAGE_LIMIT);
        self.refresh_messages(messages, force);
        self.refresh_stickers();

        // Lu dès que la conversation est affichée.
        if conversation.unread > 0 && visible {
            engine.mark_read(&conversation.id);
        }
        let mut st = self.state.borrow_mut();
        st.conversation = Some(conversation);
        st.seen_revision = revision;
        st.last_refresh = Some(Instant::now());
    }

    fn refresh_list(&self, conversations: &[ConversationView], current: Option<&str>) {
        let signature: String = conversations
            .iter()
            .map(|c| {
                let online = c.members.iter().any(|m| !m.me && m.online);
                format!("{}|{}|{}|{}|{}|{}|{}\n", c.id, c.title, c.unread, c.last_preview, c.typing.is_empty(), online, current == Some(c.id.as_str()))
            })
            .collect();
        let mut st = self.state.borrow_mut();
        if st.list_signature == signature {
            return;
        }
        let items: Vec<ConversationItem> = conversations
            .iter()
            .map(|c| {
                let group = c.kind == ConversationKind::Group;
                let seed = if group { c.id.clone() } else { c.members.iter().find(|m| !m.me).map(|m| m.id.clone()).unwrap_or_default() };
                ConversationItem {
                    id: c.id.clone().into(),
                    title: c.title.clone().into(),
                    preview: c.last_preview.clone().into(),
                    unread: c.unread as i32,
                    group,
                    online: c.members.iter().any(|m| !m.me && m.online),
                    typing: !c.typing.is_empty(),
                    selected: current == Some(c.id.as_str()),
                    initials: initials(&c.title).into(),
                    color: avatar_color(&seed),
                }
            })
            .collect();
        st.list_ids = conversations.iter().map(|c| c.id.clone()).collect();
        st.list_signature = signature;
        drop(st);
        self.conversations.set_vec(items);
    }

    /// Met à jour les lignes de messages : seules les lignes ajoutées ou changées sont refaites.
    fn refresh_messages(&self, messages: Vec<MessageView>, force: bool) {
        let mut st = self.state.borrow_mut();
        st.used.retain(|key| key.1 == Frame::Thumb);
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

        let mut rendered: Vec<(String, Option<MessageItem>)> = Vec::with_capacity(messages.len());
        let mut animated = false;
        for (index, message) in messages.iter().enumerate() {
            let attachments: Vec<(AttachmentItem, String)> =
                message.attachments.iter().map(|a| self.attachment_item(&mut st, a)).collect();
            animated |= attachments.iter().any(|(item, _)| item.frames.row_count() > 1);
            let reactions: String = message.reactions.iter().map(|r| format!("{}{}{}", r.emoji, r.count, r.mine)).collect();
            let signature = format!(
                "{}|{}|{}|{}|{:?}|{}|{}|{}|{}|{}",
                message.id,
                message.text,
                message.edited,
                message.deleted,
                message.status,
                reactions,
                attachments.iter().map(|(_, s)| s.as_str()).collect::<String>(),
                headers[index],
                Some(index) == last_mine,
                message.reply.as_ref().map(|r| r.excerpt.as_str()).unwrap_or(""),
            );
            let unchanged = !force && st.signatures.get(index) == Some(&signature);
            let item = (!unchanged).then(|| {
                message_item(message, headers[index], Some(index) == last_mine, attachments.into_iter().map(|(item, _)| item).collect())
            });
            rendered.push((signature, item));
        }

        let same_prefix = !force
            && st.messages.len() <= messages.len()
            && st.messages.iter().zip(&messages).all(|(old, new)| old.id == new.id);
        if same_prefix {
            for (index, (_, item)) in rendered.iter_mut().enumerate() {
                if let Some(item) = item.take() {
                    if index < self.messages.row_count() {
                        self.messages.set_row_data(index, item);
                    } else {
                        self.messages.push(item);
                    }
                }
            }
        } else {
            // Tout refaire (autre conversation, message retiré…) : les lignes inchangées aussi.
            let items: Vec<MessageItem> = rendered
                .iter_mut()
                .enumerate()
                .map(|(index, (_, item))| {
                    item.take().unwrap_or_else(|| {
                        let message = &messages[index];
                        let attachments = message.attachments.iter().map(|a| self.attachment_item(&mut st, a).0).collect();
                        message_item(message, headers[index], Some(index) == last_mine, attachments)
                    })
                })
                .collect();
            self.messages.set_vec(items);
        }

        // Les images qui ne sont plus affichées libèrent leur mémoire.
        let used = std::mem::take(&mut st.used);
        st.visuals.retain(|key, visual| used.contains(key) || matches!(visual, Visual::Loading));
        st.used = used;
        st.has_progress = messages.iter().any(|m| m.attachments.iter().any(|a| a.path.is_none()));
        st.animated = animated || st.sticker_signature.contains("gif");
        st.signatures = rendered.into_iter().map(|(signature, _)| signature).collect();
        st.messages = messages;
    }

    fn attachment_item(&self, st: &mut State, attachment: &AttachmentView) -> (AttachmentItem, String) {
        let mut item = AttachmentItem {
            name: attachment.file_name.clone().into(),
            path: attachment.path.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default().into(),
            frame_ms: 100,
            ..AttachmentItem::default()
        };
        let visual_kind = matches!(attachment.kind, AttachmentKind::Image | AttachmentKind::Gif | AttachmentKind::Sticker);
        let frame = if attachment.kind == AttachmentKind::Sticker { Frame::Sticker } else { Frame::Photo };
        let state = match (&attachment.path, visual_kind) {
            (Some(path), true) => match self.visual(st, path, frame) {
                Some(Some(ready)) => {
                    item.kind = 0;
                    item.image = ready.still;
                    item.frames = ModelRc::new(VecModel::from(ready.frames));
                    item.frame_ms = ready.frame_ms;
                    item.width = ready.width;
                    item.height = ready.height;
                    "prête"
                }
                Some(None) => {
                    item.kind = 3;
                    let (w, h) = if frame == Frame::Sticker { (150.0, 150.0) } else { (240.0, 160.0) };
                    item.width = w;
                    item.height = h;
                    "chargement"
                }
                None => "illisible",
            },
            _ => "",
        };
        if item.kind == 0 || item.kind == 3 {
            return (item, format!("{}:{state};", attachment.id));
        }
        let what = match attachment.kind {
            AttachmentKind::Video => "Vidéo",
            AttachmentKind::Image | AttachmentKind::Gif | AttachmentKind::Sticker => "Image",
            AttachmentKind::File => "Fichier",
        };
        match attachment.path {
            Some(_) => {
                item.kind = 1;
                item.label = format!("{what} · {} · cliquer pour ouvrir", format_size(attachment.size)).into();
            }
            None => {
                item.kind = 2;
                item.progress = attachment.progress.unwrap_or(0.0);
                item.label = match attachment.progress {
                    Some(progress) => format!("Réception… {} %  ·  {}", (progress * 100.0).round() as u32, format_size(attachment.size)),
                    None => format!("{} · en attente de l'expéditeur", format_size(attachment.size)),
                }
                .into();
            }
        }
        let progress = attachment.progress.map(|p| (p * 50.0) as u32).unwrap_or(0);
        let signature = format!("{}:{}:{}:{state};", attachment.id, item.kind, progress);
        (item, signature)
    }

    fn refresh_stickers(&self) {
        let stickers = self.engine.stickers();
        let mut st = self.state.borrow_mut();
        let mut signature = String::new();
        let mut items = Vec::new();
        for sticker in &stickers {
            let ready = self.visual(&mut st, &sticker.path, Frame::Thumb).flatten();
            signature.push_str(&format!("{}:{}:{};", sticker.id, ready.is_some(), if sticker.animated { "gif" } else { "" }));
            let ready = ready.unwrap_or(Ready { still: Image::default(), frames: Vec::new(), frame_ms: 100, width: 0.0, height: 0.0 });
            items.push(StickerItem {
                id: sticker.id.clone().into(),
                image: ready.still,
                frames: ModelRc::new(VecModel::from(ready.frames)),
                frame_ms: ready.frame_ms,
            });
        }
        if signature != st.sticker_signature {
            st.sticker_signature = signature;
            drop(st);
            self.stickers.set_vec(items);
        }
    }

    fn scroll_to_bottom(&self, ui: &AppWindow) {
        let data = ui.global::<ChatData>();
        data.set_scroll_request(data.get_scroll_request() + 1);
    }

    fn focus_input(&self, ui: &AppWindow) {
        let data = ui.global::<ChatData>();
        data.set_focus_request(data.get_focus_request() + 1);
    }

    fn update_banner(&self, data: &ChatData) {
        let st = self.state.borrow();
        let text = if st.editing.is_some() {
            "Modification du message  ·  Entrée pour enregistrer, Échap pour annuler".to_string()
        } else {
            st.reply_to.as_ref().map(|(_, author, excerpt)| format!("Réponse à {author} : {excerpt}")).unwrap_or_default()
        };
        data.set_banner(text.into());
    }

    fn update_pending(&self, data: &ChatData) {
        let st = self.state.borrow();
        let names: Vec<SharedString> = st
            .pending
            .iter()
            .take(PENDING_SHOWN)
            .map(|path| path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default().into())
            .collect();
        data.set_pending(ModelRc::new(VecModel::from(names)));
        data.set_pending_more(st.pending.len().saturating_sub(PENDING_SHOWN) as i32);
    }

    // ---------------------------------------------------------------- actions

    pub fn open_conversation(&self, id: &str) {
        let Some(ui) = self.ui() else { return };
        let data = ui.global::<ChatData>();
        let changed = {
            let mut st = self.state.borrow_mut();
            let changed = st.current.as_deref() != Some(id);
            if changed {
                st.current = Some(id.to_string());
                st.reply_to = None;
                st.editing = None;
                st.pending.clear();
            }
            st.list_signature.clear();
            changed
        };
        if changed {
            data.set_draft("".into());
        }
        self.update_banner(&data);
        self.update_pending(&data);
        self.refresh(&ui, true);
        self.scroll_to_bottom(&ui);
        self.focus_input(&ui);
    }

    /// Ouvre la n-ième conversation de la liste (captures d'écran de développement).
    pub fn select_index(&self, index: usize) {
        let id = self.state.borrow().list_ids.get(index).cloned();
        if let Some(id) = id {
            self.open_conversation(&id);
        }
    }

    /// Ouvre la discussion avec un appareil associé (menu + ou page Appareils).
    pub fn chat_with(&self, peer: &str) {
        match self.engine.open_direct(peer) {
            Ok(conversation) => {
                if let Some(ui) = self.ui() {
                    ui.set_page(CHAT_PAGE);
                    self.state.borrow_mut().was_visible = true;
                }
                self.open_conversation(&conversation);
            }
            Err(error) => self.status(error.to_string()),
        }
    }

    fn send(&self) {
        let Some(ui) = self.ui() else { return };
        let data = ui.global::<ChatData>();
        let text = data.get_draft().to_string();
        let (conversation, editing, reply, files) = {
            let st = self.state.borrow();
            (st.current.clone(), st.editing.clone(), st.reply_to.as_ref().map(|r| r.0.clone()), st.pending.clone())
        };
        let Some(conversation) = conversation else { return };
        if text.trim().is_empty() && files.is_empty() {
            return;
        }
        let result = match editing {
            Some(message) => self.engine.edit_chat(&conversation, &message, &text),
            None => self.engine.send_chat(&conversation, &text, reply.as_deref(), &files),
        };
        match result {
            Ok(()) => {
                data.set_draft("".into());
                {
                    let mut st = self.state.borrow_mut();
                    st.editing = None;
                    st.reply_to = None;
                    st.pending.clear();
                }
                self.update_banner(&data);
                self.update_pending(&data);
                self.refresh(&ui, false);
                self.scroll_to_bottom(&ui);
            }
            Err(error) => self.status(format!("Message non envoyé : {error}")),
        }
    }

    fn cancel_banner(&self) -> bool {
        let (active, was_editing) = {
            let mut st = self.state.borrow_mut();
            let active = st.editing.is_some() || st.reply_to.is_some();
            let was_editing = st.editing.take().is_some();
            st.reply_to = None;
            (active, was_editing)
        };
        if let Some(ui) = self.ui() {
            let data = ui.global::<ChatData>();
            if was_editing {
                data.set_draft("".into());
            }
            self.update_banner(&data);
        }
        active
    }

    fn start_edit(&self, message: &MessageView) {
        let Some(ui) = self.ui() else { return };
        let data = ui.global::<ChatData>();
        data.set_draft(message.text.clone().into());
        {
            let mut st = self.state.borrow_mut();
            st.reply_to = None;
            st.editing = Some(message.id.clone());
        }
        self.update_banner(&data);
        self.focus_input(&ui);
    }

    fn edit_last(&self) -> bool {
        let last = self
            .state
            .borrow()
            .messages
            .iter()
            .rev()
            .find(|m| m.mine && !m.system && !m.deleted && !m.text.is_empty())
            .cloned();
        match last {
            Some(message) if self.state.borrow().conversation.as_ref().is_some_and(|c| !c.closed) => {
                self.start_edit(&message);
                true
            }
            _ => false,
        }
    }

    fn message_action(&self, index: i32, action: &str) {
        let Some(ui) = self.ui() else { return };
        let data = ui.global::<ChatData>();
        let (message, conversation) = {
            let st = self.state.borrow();
            (st.messages.get(index.max(0) as usize).cloned(), st.current.clone())
        };
        let (Some(message), Some(conversation)) = (message, conversation) else { return };
        match action {
            "reply" => {
                let excerpt: String = markdown::plain(&message.text).chars().take(80).collect();
                let excerpt = if excerpt.is_empty() { "pièce jointe".to_string() } else { excerpt };
                {
                    let mut st = self.state.borrow_mut();
                    if st.editing.take().is_some() {
                        data.set_draft("".into());
                    }
                    st.reply_to = Some((message.id.clone(), message.author_name.clone(), excerpt));
                }
                self.update_banner(&data);
                self.focus_input(&ui);
            }
            "edit" => self.start_edit(&message),
            "delete" => {
                self.state.borrow_mut().delete_target = Some(message.id.clone());
                self.open_dialog(&data, 4, "Supprimer ce message ?", "Il disparaîtra aussi chez les autres membres de la discussion.");
            }
            "copy" => match self.engine.write_local_clipboard(&markdown::plain(&message.text)) {
                Ok(()) => self.status("Texte du message copié"),
                Err(error) => self.status(error.to_string()),
            },
            _ => {
                if let Some(emoji) = action.strip_prefix("react:") {
                    if let Err(error) = self.engine.toggle_reaction(&conversation, &message.id, emoji) {
                        self.status(error.to_string());
                    }
                }
            }
        }
    }

    fn toggle_reaction(&self, index: i32, emoji: &str) {
        let (message, conversation) = {
            let st = self.state.borrow();
            (st.messages.get(index.max(0) as usize).map(|m| m.id.clone()), st.current.clone())
        };
        if let (Some(message), Some(conversation)) = (message, conversation) {
            if let Err(error) = self.engine.toggle_reaction(&conversation, &message, emoji) {
                self.status(error.to_string());
            }
        }
    }

    fn add_pending(&self, files: Vec<PathBuf>) {
        {
            let mut st = self.state.borrow_mut();
            for file in files {
                if st.pending.len() >= MAX_ATTACHMENTS {
                    self.engine.set_status(format!("{MAX_ATTACHMENTS} pièces jointes au plus par message"));
                    break;
                }
                if !st.pending.contains(&file) {
                    st.pending.push(file);
                }
            }
        }
        if let Some(ui) = self.ui() {
            self.update_pending(&ui.global::<ChatData>());
            self.focus_input(&ui);
        }
    }

    /// Ctrl+V dans la zone de saisie : des fichiers ou une image copiés deviennent des pièces
    /// jointes. Faux s'il faut laisser la zone de saisie coller du texte normalement.
    fn paste_media(&self) -> bool {
        let Ok(mut clipboard) = arboard::Clipboard::new() else { return false };
        if let Ok(files) = clipboard.get().file_list() {
            let files: Vec<PathBuf> = files.into_iter().filter(|path| path.is_file()).collect();
            if !files.is_empty() {
                self.add_pending(files);
                return true;
            }
        }
        if clipboard.get_text().is_ok_and(|text| !text.is_empty()) {
            return false;
        }
        if clipboard.get_image().is_err() {
            return false;
        }
        match self.engine.clipboard_image_to_file() {
            Ok(path) => {
                self.add_pending(vec![path]);
                true
            }
            Err(error) => {
                self.status(error.to_string());
                false
            }
        }
    }

    // ---------------------------------------------------------------- boîtes de dialogue

    fn open_dialog(&self, data: &ChatData, kind: i32, title: &str, text: &str) {
        data.set_dialog_title(title.into());
        data.set_dialog_text(text.into());
        data.set_dialog_error("".into());
        data.set_dialog(kind);
    }

    /// Prépare les cases à cocher : les appareils associés, sauf ceux de `exclude`.
    fn set_choices(&self, data: &ChatData, exclude: &[String]) -> bool {
        let candidates: Vec<(String, String)> = self
            .engine
            .snapshot()
            .peers
            .into_iter()
            .filter(|p| p.trusted && !exclude.contains(&p.id))
            .map(|p| (p.id, p.name))
            .collect();
        if candidates.is_empty() {
            self.status("Aucun autre appareil associé : associez-en un dans la page Appareils");
            return false;
        }
        let items: Vec<ChoiceItem> =
            candidates.iter().map(|(id, name)| ChoiceItem { id: id.clone().into(), name: name.clone().into(), checked: false }).collect();
        data.set_choices(ModelRc::new(VecModel::from(items)));
        let mut st = self.state.borrow_mut();
        st.choice_checked = vec![false; candidates.len()];
        st.choice_ids = candidates.into_iter().map(|(id, _)| id).collect();
        true
    }

    fn new_group(&self) {
        let Some(ui) = self.ui() else { return };
        let data = ui.global::<ChatData>();
        if self.set_choices(&data, &[]) {
            data.set_dialog_name("".into());
            self.open_dialog(
                &data,
                1,
                "Nouveau groupe",
                "Choisissez des appareils que vous avez associés. MagicPlus présente leurs clés aux autres membres : ils pourront se parler dans ce groupe sans s'associer entre eux.",
            );
        }
    }

    fn add_members(&self) {
        let Some(ui) = self.ui() else { return };
        let data = ui.global::<ChatData>();
        let existing: Vec<String> =
            self.state.borrow().conversation.as_ref().map(|c| c.members.iter().map(|m| m.id.clone()).collect()).unwrap_or_default();
        if self.set_choices(&data, &existing) {
            self.open_dialog(&data, 2, "Ajouter des membres", "Les nouveaux membres reçoivent l'historique du groupe et les clés des autres membres.");
        }
    }

    fn rename(&self) {
        let Some(ui) = self.ui() else { return };
        let data = ui.global::<ChatData>();
        let title = self.state.borrow().conversation.as_ref().map(|c| c.title.clone()).unwrap_or_default();
        data.set_dialog_name(title.into());
        self.open_dialog(&data, 3, "Renommer le groupe", "");
    }

    fn leave(&self) {
        let Some(ui) = self.ui() else { return };
        self.open_dialog(
            &ui.global::<ChatData>(),
            5,
            "Quitter ce groupe ?",
            "Vous ne recevrez plus ses messages. L'historique reste consultable sur cet appareil.",
        );
    }

    fn toggle_choice(&self, index: i32, checked: bool) {
        let index = index.max(0) as usize;
        if let Some(slot) = self.state.borrow_mut().choice_checked.get_mut(index) {
            *slot = checked;
        }
    }

    fn dialog_accepted(&self) {
        let Some(ui) = self.ui() else { return };
        let data = ui.global::<ChatData>();
        let name = data.get_dialog_name().to_string();
        let (conversation, chosen, target) = {
            let st = self.state.borrow();
            let chosen: Vec<String> =
                st.choice_ids.iter().zip(&st.choice_checked).filter(|(_, on)| **on).map(|(id, _)| id.clone()).collect();
            (st.current.clone().unwrap_or_default(), chosen, st.delete_target.clone())
        };
        let result = match data.get_dialog() {
            1 => self.engine.create_group(&name, &chosen).map(|id| {
                data.set_dialog(0);
                self.open_conversation(&id);
            }),
            2 if chosen.is_empty() => Err(anyhow::anyhow!("choisissez au moins un appareil")),
            2 => self.engine.add_group_members(&conversation, &chosen),
            3 => self.engine.rename_group(&conversation, &name),
            4 => match target {
                Some(message) => self.engine.delete_chat(&conversation, &message),
                None => Ok(()),
            },
            5 => self.engine.leave_group(&conversation),
            _ => Ok(()),
        };
        match result {
            Ok(()) => data.set_dialog(0),
            Err(error) => {
                let text = error.to_string();
                let mut chars = text.chars();
                let text = chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
                data.set_dialog_error(text.into());
            }
        }
    }

    // ---------------------------------------------------------------- autocollants

    fn send_sticker(&self, id: &str) {
        let Some(conversation) = self.current() else { return };
        match self.engine.send_sticker(&conversation, id) {
            Ok(()) => {
                if let Some(ui) = self.ui() {
                    self.scroll_to_bottom(&ui);
                }
            }
            Err(error) => self.status(error.to_string()),
        }
    }

    fn add_sticker(&self) {
        let picked = rfd::FileDialog::new()
            .set_title("Image ou GIF pour un autocollant")
            .add_filter("Images et GIF", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
            .pick_file();
        if let Some(path) = picked {
            match self.engine.add_sticker(&path) {
                Ok(_) => self.status("Autocollant ajouté à votre bibliothèque"),
                Err(error) => self.status(error.to_string()),
            }
            self.refresh_stickers();
        }
    }

    fn remove_sticker(&self, id: &str) {
        self.engine.remove_sticker(id);
        self.refresh_stickers();
    }
}

fn message_item(message: &MessageView, header: bool, last_mine: bool, attachments: Vec<AttachmentItem>) -> MessageItem {
    let status = if message.mine && (last_mine || message.status == MessageStatus::Sending) { status_label(message) } else { String::new() };
    MessageItem {
        id: message.id.clone().into(),
        author: if message.system { markdown::plain(&message.text).into() } else { message.author_name.clone().into() },
        initials: initials(&message.author_name).into(),
        color: avatar_color(&message.author_id),
        mine: message.mine,
        system: message.system,
        header,
        time: time_label(message.sent_ms).into(),
        text: if message.deleted || message.system { StyledText::default() } else { styled(&message.text) },
        has_text: !message.text.trim().is_empty() && !message.deleted,
        deleted: message.deleted,
        edited: message.edited,
        jumbo: jumbo(&message.text),
        reply: message.reply.as_ref().map(|r| format!("{} : {}", r.author_name, r.excerpt)).unwrap_or_default().into(),
        attachments: ModelRc::new(VecModel::from(attachments)),
        reactions: ModelRc::new(VecModel::from(
            message
                .reactions
                .iter()
                .map(|r| ReactionItem { emoji: r.emoji.clone().into(), count: r.count as i32, mine: r.mine, names: r.names.join(", ").into() })
                .collect::<Vec<_>>(),
        )),
        status: status.into(),
    }
}

/// Branche les actions de la page sur le moteur.
fn install(chat: &Rc<Chat>, data: &ChatData) {
    // Les fermetures gardent un lien faible : la page ne se retient pas elle-même en mémoire.
    macro_rules! on {
        ($setter:ident, |$chat:ident $(, $arg:ident)*| $body:expr) => {{
            let weak = Rc::downgrade(chat);
            data.$setter(move |$($arg),*| {
                #[allow(unused_variables)]
                let Some($chat) = weak.upgrade() else { return Default::default() };
                $body
            });
        }};
    }
    on!(on_select, |chat, index| {
        let id = chat.state.borrow().list_ids.get(index.max(0) as usize).cloned();
        if let Some(id) = id {
            chat.open_conversation(&id);
        }
    });
    on!(on_send, |chat| chat.send());
    on!(on_typing_changed, |chat| {
        if let (Some(conversation), Some(ui)) = (chat.current(), chat.ui()) {
            if !ui.global::<ChatData>().get_draft().trim().is_empty() {
                chat.engine.notify_typing(&conversation);
            }
        }
    });
    on!(on_paste_media, |chat| chat.paste_media());
    on!(on_cancel_banner, |chat| chat.cancel_banner());
    on!(on_edit_last, |chat| chat.edit_last());
    on!(on_message_action, |chat, index, action| chat.message_action(index, &action));
    on!(on_toggle_reaction, |chat, index, emoji| chat.toggle_reaction(index, &emoji));
    on!(on_open_path, |chat, path| {
        if !path.is_empty() {
            open_path(&path);
        }
    });
    on!(on_open_link, |chat, url| {
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:") {
            open_path(&url);
        }
    });
    on!(on_attach, |chat| {
        if let Some(files) = rfd::FileDialog::new().set_title("Joindre des fichiers (jusqu'à 15, 1 Go chacun)").pick_files() {
            chat.add_pending(files);
        }
    });
    on!(on_remove_pending, |chat, index| {
        {
            let mut st = chat.state.borrow_mut();
            let index = index.max(0) as usize;
            if index < st.pending.len() {
                st.pending.remove(index);
            }
        }
        if let Some(ui) = chat.ui() {
            chat.update_pending(&ui.global::<ChatData>());
        }
    });
    on!(on_open_direct, |chat, peer| chat.chat_with(&peer));
    on!(on_new_group, |chat| chat.new_group());
    on!(on_add_members, |chat| chat.add_members());
    on!(on_rename, |chat| chat.rename());
    on!(on_leave, |chat| chat.leave());
    on!(on_toggle_choice, |chat, index, checked| chat.toggle_choice(index, checked));
    on!(on_dialog_accepted, |chat| chat.dialog_accepted());
    on!(on_send_sticker, |chat, id| chat.send_sticker(&id));
    on!(on_add_sticker, |chat| chat.add_sticker());
    on!(on_remove_sticker, |chat, id| chat.remove_sticker(&id));
}

#[cfg(test)]
mod tests {
    use magicplus_core::markdown;
    use slint::StyledText;

    /// Aucun message, même piégé, ne doit faire échouer l'analyseur Markdown de Slint.
    #[test]
    fn slint_accepte_tous_les_messages() {
        let messages = [
            "Salut **B** 👋",
            "# titre\n## sous-titre\n- liste\n1. numéro\n> citation\n---\n===",
            "<b>html</b> <u>souligné</u> <font color=red>x</font> &amp; &#169;",
            "**gras _italique ~~barré~~_** `code` [lien](https://exemple.fr/a_(b)) https://nu.fr/x?y=1&z=2.",
            "    bloc indenté\n\tavec tabulation",
            "```\nlet x = `a` + \"**\";\n```\naprès",
            "{} {0} \u{E541} \\ \\\\ * _ ~ ` [ ] ( ) < > | !",
            "**(parenthèses)**a*b*c **  espaces  ** ~~~triple~~~",
            "a\n\n\nb\r\nc",
            "[](https://vide.fr) [texte]( https://espace.fr ) [x](javascript:alert(1))",
            "",
            "🎉🎉",
        ];
        for message in messages {
            let commonmark = markdown::to_commonmark(message);
            assert!(StyledText::from_markdown(&commonmark).is_ok(), "refusé : {message:?} → {commonmark:?}");
        }
    }
}
