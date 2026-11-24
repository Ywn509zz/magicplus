//! Discussions : conversations à deux et groupes, sans serveur.
//!
//! **Fiabilité.** Chaque appareil numérote ses propres événements dans une conversation (message,
//! modification, suppression, réaction, membres) : 1, 2, 3… Un destinataire n'applique l'événement
//! `n` d'un auteur qu'après `n - 1`, et répond par le dernier numéro appliqué sans trou. L'auteur
//! renvoie tout ce qui n'est pas confirmé, même après un redémarrage ou des jours hors ligne : les
//! messages arrivent **complets et dans l'ordre**. Une horloge logique (Lamport) donne à tous les
//! membres le **même ordre d'affichage**.
//!
//! **Groupes.** Le créateur choisit des appareils qu'il a associés ; l'événement `GroupUpdate` qu'il
//! signe (canal Noise authentifié) présente leurs clés aux autres membres. Un membre accepte alors les
//! messages d'un autre membre du groupe, prouvés par sa clé, sans association directe — mais
//! seulement pour ce groupe. Chaque auteur envoie ses événements directement à chaque membre.
//!
//! **Stockage** (`discussions/` dans le dossier de données) : `etat.json` (compteurs, membres),
//! `evenements/<conversation>.jsonl` (journal, rejoué au démarrage), `pieces/` (pièces jointes
//! reçues), `autocollants/`, `coller/` (images collées).

use crate::{
    crypto,
    engine::{explain_refusal, lock, read_ack, refusal, Engine, Inner, Target},
    protocol::*,
    store::TrustedPeer,
    util::*,
};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};

const STATE_FILE: &str = "etat.json";
const EVENTS_DIR: &str = "evenements";
const MEDIA_DIR: &str = "pieces";
const STICKERS_DIR: &str = "autocollants";
const PASTE_DIR: &str = "coller";
/// Envoi par lots : au plus tant d'événements ou d'octets par message réseau.
const BATCH_EVENTS: usize = 200;
const BATCH_BYTES: usize = 1024 * 1024;
const MAX_GROUP_MEMBERS: usize = 64;
const TYPING_SHOWN: Duration = Duration::from_secs(6);
const TYPING_THROTTLE: Duration = Duration::from_secs(3);
const SYNC_RETRY_MIN: Duration = Duration::from_secs(2);
const SYNC_RETRY_MAX: Duration = Duration::from_secs(30);
const STICKER_SIZE: u32 = 320;
const MAX_STICKER_BYTES: u64 = 8 * 1024 * 1024;

// ------------------------------------------------------------------ vues pour l'interface

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConversationKind {
    Direct,
    Group,
}

#[derive(Debug, Clone)]
pub struct MemberView {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub me: bool,
    /// Ami (associé), demande en cours, ou rien : « Ajouter en ami ».
    pub friend: crate::friends::FriendState,
}

#[derive(Debug, Clone)]
pub struct ConversationView {
    pub id: String,
    pub kind: ConversationKind,
    pub title: String,
    pub members: Vec<MemberView>,
    pub unread: usize,
    pub last_preview: String,
    pub last_ms: u64,
    /// Noms des membres en train d'écrire.
    pub typing: Vec<String>,
    pub is_creator: bool,
    /// Nous avons quitté ce groupe : lecture seule.
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessageStatus {
    /// Message d'un autre membre.
    None,
    /// Pas encore confirmé par tous les destinataires.
    Sending,
    Delivered,
    /// Lu par ces membres (tous ou une partie).
    Read(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct ReplyView {
    pub id: String,
    pub author_name: String,
    pub excerpt: String,
}

#[derive(Debug, Clone)]
pub struct AttachmentView {
    pub id: String,
    pub file_name: String,
    pub size: u64,
    pub kind: AttachmentKind,
    /// Fichier sur le disque, s'il est disponible (envoyé par nous ou reçu en entier).
    pub path: Option<PathBuf>,
    /// Réception en cours (0 à 1).
    pub progress: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct ReactionView {
    pub emoji: String,
    pub count: usize,
    pub mine: bool,
    pub names: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MessageView {
    pub id: String,
    pub author_id: String,
    pub author_name: String,
    pub mine: bool,
    /// Message du système (groupe créé, membre parti…).
    pub system: bool,
    /// Texte en Markdown.
    pub text: String,
    pub sent_ms: u64,
    pub edited: bool,
    pub deleted: bool,
    pub reply: Option<ReplyView>,
    pub attachments: Vec<AttachmentView>,
    pub reactions: Vec<ReactionView>,
    pub status: MessageStatus,
    pub lamport: u64,
    /// Message transféré : nom de son auteur d'origine.
    pub forwarded: Option<String>,
    /// Épinglé (par qui).
    pub pinned_by: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StickerView {
    pub id: String,
    pub path: PathBuf,
    pub animated: bool,
}

// ------------------------------------------------------------------ état enregistré

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    id: String,
    kind: ConversationKind,
    #[serde(default)]
    name: String,
    creator: String,
    #[serde(default)]
    members: Vec<MemberInfo>,
    #[serde(default)]
    closed: bool,
    #[serde(default)]
    my_seq: u64,
    #[serde(default)]
    lamport: u64,
    /// Dernier numéro appliqué, par auteur.
    #[serde(default)]
    applied: BTreeMap<String, u64>,
    /// Dernier de nos numéros confirmé, par membre.
    #[serde(default)]
    acked: BTreeMap<String, u64>,
    #[serde(default)]
    my_read: u64,
    #[serde(default)]
    read_by: BTreeMap<String, u64>,
    #[serde(default)]
    read_sent: BTreeMap<String, u64>,
    /// Nos pièces jointes : identifiant → chemin du fichier envoyé.
    #[serde(default)]
    local_files: BTreeMap<String, String>,
    #[serde(default)]
    created_ms: u64,
    /// Membres partis : une liste de membres un peu ancienne ne les fait pas revenir
    /// (seul un nouvel ajout explicite le peut).
    #[serde(default)]
    left: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    author: String,
    event: ChatEvent,
    /// Texte du message système produit par cet événement (groupe créé, membre parti…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Clone)]
struct Msg {
    id: String,
    author: String,
    seq: u64,
    lamport: u64,
    sent_ms: u64,
    text: String,
    reply_to: Option<String>,
    attachments: Vec<AttachmentInfo>,
    edited: bool,
    deleted: bool,
    system: bool,
    reactions: BTreeMap<String, BTreeSet<String>>,
    /// Message transféré : auteur d'origine.
    forwarded: Option<String>,
    /// Épinglé par ce membre.
    pinned: Option<String>,
}

struct Conversation {
    meta: Meta,
    events: Vec<Stored>,
    /// Messages dans l'ordre d'affichage (horloge logique, puis auteur).
    messages: Vec<Msg>,
}

pub(crate) struct ChatStore {
    dir: PathBuf,
    me: String,
    conversations: HashMap<String, Conversation>,
    revision: u64,
    typing: HashMap<(String, String), Instant>,
    typing_sent: HashMap<String, Instant>,
    syncing: HashSet<(String, String)>,
    retry: HashMap<(String, String), (Instant, u32)>,
}

/// Expéditeur d'une connexion entrante, prouvé par sa clé.
pub(crate) struct SenderInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub public_key: String,
    /// Associé par nous (sinon : seulement membre d'un groupe, présenté par son créateur).
    pub trusted: bool,
}

pub(crate) fn valid_name(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Identifiant d'une conversation à deux : le même des deux côtés, sans négociation.
pub fn direct_id(a: &str, b: &str) -> String {
    let (first, second) = if a <= b { (a, b) } else { (b, a) };
    let digest = Sha256::digest(format!("{first}|{second}").as_bytes());
    let hex: String = digest.iter().take(12).map(|byte| format!("{byte:02x}")).collect();
    format!("d-{hex}")
}

fn attachment_kind(path: &Path) -> AttachmentKind {
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "gif" => AttachmentKind::Gif,
        "png" | "jpg" | "jpeg" | "heic" | "webp" | "bmp" | "tif" | "tiff" => AttachmentKind::Image,
        "mp4" | "mov" | "m4v" | "webm" | "avi" | "mkv" => AttachmentKind::Video,
        _ => AttachmentKind::File,
    }
}

/// Aperçu sur une ligne, sans la syntaxe Markdown.
fn excerpt(text: &str) -> String {
    let flat: String = crate::markdown::plain(text).split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 80 {
        format!("{}…", flat.chars().take(80).collect::<String>())
    } else {
        flat
    }
}

impl ChatStore {
    pub(crate) fn load(dir: PathBuf, me: &str) -> Self {
        let _ = fs::create_dir_all(dir.join(EVENTS_DIR));
        let metas: Vec<Meta> = fs::read(dir.join(STATE_FILE))
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        let mut conversations = HashMap::new();
        for meta in metas {
            if !valid_name(&meta.id) {
                continue;
            }
            let mut conversation = Conversation { meta, events: Vec::new(), messages: Vec::new() };
            let path = dir.join(EVENTS_DIR).join(format!("{}.jsonl", conversation.meta.id));
            if let Ok(raw) = fs::read_to_string(path) {
                for line in raw.lines() {
                    if let Ok(stored) = serde_json::from_str::<Stored>(line) {
                        apply_to_messages(&mut conversation.messages, &stored);
                        conversation.events.push(stored);
                    }
                }
            }
            conversations.insert(conversation.meta.id.clone(), conversation);
        }
        Self {
            dir,
            me: me.to_string(),
            conversations,
            revision: 1,
            typing: HashMap::new(),
            typing_sent: HashMap::new(),
            syncing: HashSet::new(),
            retry: HashMap::new(),
        }
    }

    fn save(&self) {
        let metas: Vec<&Meta> = self.conversations.values().map(|c| &c.meta).collect();
        let Ok(raw) = serde_json::to_vec_pretty(&metas) else { return };
        let temporary = self.dir.join(format!("{STATE_FILE}.tmp"));
        if fs::write(&temporary, raw).is_ok() {
            let _ = fs::rename(temporary, self.dir.join(STATE_FILE));
        }
    }

    fn bump(&mut self) {
        self.revision += 1;
    }

    /// Ajoute un événement au journal et met à jour l'état (sans vérifier l'ordre : l'appelant le fait).
    fn record(&mut self, conversation_id: &str, author: &str, event: ChatEvent) {
        let me = self.me.clone();
        let Some(conversation) = self.conversations.get_mut(conversation_id) else { return };
        let meta = &mut conversation.meta;
        meta.lamport = meta.lamport.max(event.lamport);
        if author != me {
            meta.applied.insert(author.to_string(), event.seq);
        }

        let name_of = |members: &[MemberInfo], id: &str| {
            members.iter().find(|m| m.id == id).map(|m| m.name.clone()).unwrap_or_else(|| "Un membre".to_string())
        };
        let mut note = None;
        match &event.kind {
            ChatKind::GroupUpdate { name, members } if author == meta.creator => {
                let creator = name_of(members, author);
                let first = event.seq == 1 || meta.members.is_empty();
                // Jamais de retrait ici : un membre ajouté par un autre, ou plus tard, reste membre.
                let added = merge_members(&mut meta.members, members, &meta.left);
                if first {
                    note = Some(format!("{creator} a créé le groupe « {name} »"));
                } else if !added.is_empty() {
                    note = Some(format!("{creator} a ajouté {}", added.join(", ")));
                } else if *name != meta.name {
                    note = Some(format!("{creator} a renommé le groupe en « {name} »"));
                }
                meta.name = name.clone();
            }
            ChatKind::AddMembers { members } if meta.kind == ConversationKind::Group => {
                let adder = name_of(&meta.members, author);
                // Ajout explicite : même un membre parti peut revenir.
                for member in members {
                    meta.left.remove(&member.id);
                }
                merge_members(&mut meta.members, members, &BTreeSet::new());
                if members.iter().any(|m| m.id == me) && author != me {
                    meta.closed = false;
                }
                // Tous les noms de l'événement : un membre déjà appris par un autre chemin compte aussi.
                let names: Vec<String> = members.iter().map(|m| m.name.clone()).collect();
                note = Some(format!("{adder} a ajouté {}", names.join(", ")));
            }
            ChatKind::Leave => {
                note = Some(format!("{} a quitté le groupe", name_of(&meta.members, author)));
                meta.left.insert(author.to_string());
                if author == me {
                    meta.closed = true;
                } else {
                    meta.members.retain(|m| m.id != author);
                }
            }
            _ => {}
        }

        let stored = Stored { author: author.to_string(), event, note };
        let path = self.dir.join(EVENTS_DIR).join(format!("{conversation_id}.jsonl"));
        if let (Ok(mut file), Ok(line)) = (OpenOptions::new().create(true).append(true).open(path), serde_json::to_string(&stored)) {
            let _ = writeln!(file, "{line}");
        }
        apply_to_messages(&mut conversation.messages, &stored);
        conversation.events.push(stored);
        self.save();
        self.bump();
    }

    /// Événement écrit par nous (numéroté, horodaté).
    fn emit(&mut self, conversation_id: &str, kind: ChatKind) -> Result<ChatEvent> {
        let conversation = self.conversations.get_mut(conversation_id).ok_or_else(|| anyhow!("conversation introuvable"))?;
        if conversation.meta.closed && !matches!(kind, ChatKind::Leave) {
            bail!("vous avez quitté ce groupe");
        }
        conversation.meta.my_seq += 1;
        let event = ChatEvent {
            seq: conversation.meta.my_seq,
            lamport: conversation.meta.lamport + 1,
            sent_ms: now_ms(),
            kind,
        };
        let me = self.me.clone();
        self.record(conversation_id, &me, event.clone());
        Ok(event)
    }

    fn create(&mut self, meta: Meta) {
        let id = meta.id.clone();
        self.conversations.insert(id, Conversation { meta, events: Vec::new(), messages: Vec::new() });
        self.save();
        self.bump();
    }

    /// Le membre `id` de cette conversation, s'il existe.
    fn member(&self, conversation: &str, id: &str) -> Option<MemberInfo> {
        self.conversations.get(conversation)?.meta.members.iter().find(|m| m.id == id).cloned()
    }

    /// Membres (hors nous) d'une conversation où l'on peut encore écrire.
    pub(crate) fn writable_members(&self, conversation: &str) -> Option<Vec<String>> {
        let state = self.conversations.get(conversation)?;
        (!state.meta.closed).then(|| state.meta.members.iter().filter(|m| m.id != self.me).map(|m| m.id.clone()).collect())
    }

    /// Nom d'un membre de l'une de nos conversations.
    pub(crate) fn member_name(&self, id: &str) -> Option<String> {
        self.conversations.values().flat_map(|c| c.meta.members.iter()).find(|m| m.id == id).map(|m| m.name.clone())
    }

    /// Membres de nos groupes actifs (hors nous), avec un groupe qui permet de les joindre.
    pub(crate) fn group_contacts(&self) -> Vec<(String, String)> {
        let mut contacts: Vec<(String, String)> = Vec::new();
        for state in self.conversations.values() {
            if state.meta.kind != ConversationKind::Group || state.meta.closed {
                continue;
            }
            for member in &state.meta.members {
                if member.id != self.me && !contacts.iter().any(|(id, _)| id == &member.id) {
                    contacts.push((member.id.clone(), state.meta.id.clone()));
                }
            }
        }
        contacts
    }

    /// Un membre (présenté dans un groupe) qui a prouvé cette clé.
    pub(crate) fn member_by_key(&self, id: &str, public_key: &str) -> Option<MemberInfo> {
        self.conversations
            .values()
            .flat_map(|c| c.meta.members.iter())
            .find(|m| m.id == id && m.public_key == public_key)
            .cloned()
    }

    pub(crate) fn is_member(&self, conversation: &str, sender: &SenderInfo) -> bool {
        self.member(conversation, &sender.id).is_some_and(|m| m.public_key == sender.public_key)
    }

    fn media_path(&self, conversation: &str, attachment: &str, file_name: &str) -> PathBuf {
        self.dir.join(MEDIA_DIR).join(conversation).join(attachment).join(sanitize_file_name(file_name))
    }
}

/// Ajoute à `members` ceux de `incoming` qui n'y sont pas (et ne sont pas partis).
/// Retourne les noms des membres ajoutés.
fn merge_members(members: &mut Vec<MemberInfo>, incoming: &[MemberInfo], left: &BTreeSet<String>) -> Vec<String> {
    let mut added = Vec::new();
    for member in incoming {
        if members.len() >= MAX_GROUP_MEMBERS {
            break;
        }
        if !left.contains(&member.id) && !members.iter().any(|m| m.id == member.id) {
            members.push(member.clone());
            added.push(member.name.clone());
        }
    }
    added
}

/// Met à jour la liste des messages affichés à partir d'un événement du journal.
fn apply_to_messages(messages: &mut Vec<Msg>, stored: &Stored) {
    let event = &stored.event;
    let author = stored.author.as_str();
    let find = |messages: &mut Vec<Msg>, id: &str| messages.iter().position(|m| m.id == id);
    let insert = |messages: &mut Vec<Msg>, message: Msg| {
        if messages.iter().any(|m| m.id == message.id) {
            return;
        }
        let key = (message.lamport, message.author.clone(), message.id.clone());
        let position = messages.partition_point(|m| (m.lamport, m.author.clone(), m.id.clone()) < key);
        messages.insert(position, message);
    };

    match &event.kind {
        ChatKind::Post { id, text, reply_to, attachments, forwarded } => insert(
            messages,
            Msg {
                forwarded: forwarded.clone(),
                pinned: None,
                id: id.clone(),
                author: author.to_string(),
                seq: event.seq,
                lamport: event.lamport,
                sent_ms: event.sent_ms,
                text: text.clone(),
                reply_to: reply_to.clone(),
                attachments: attachments.clone(),
                edited: false,
                deleted: false,
                system: false,
                reactions: BTreeMap::new(),
            },
        ),
        ChatKind::Edit { target, text } => {
            if let Some(index) = find(messages, target) {
                let message = &mut messages[index];
                if message.author == author && !message.deleted {
                    message.text = text.clone();
                    message.edited = true;
                }
            }
        }
        ChatKind::Delete { target } => {
            if let Some(index) = find(messages, target) {
                let message = &mut messages[index];
                if message.author == author {
                    message.deleted = true;
                    message.text.clear();
                    message.attachments.clear();
                    message.reactions.clear();
                }
            }
        }
        ChatKind::React { target, emoji, add } => {
            if let Some(index) = find(messages, target) {
                let reactions = &mut messages[index].reactions;
                if *add {
                    reactions.entry(emoji.clone()).or_default().insert(author.to_string());
                } else if let Some(who) = reactions.get_mut(emoji) {
                    who.remove(author);
                    if who.is_empty() {
                        reactions.remove(emoji);
                    }
                }
            }
        }
        ChatKind::GroupUpdate { .. } | ChatKind::AddMembers { .. } | ChatKind::Leave => {
            if let Some(note) = &stored.note {
                insert(
                    messages,
                    Msg {
                        id: format!("systeme-{author}-{}", event.seq),
                        author: author.to_string(),
                        seq: event.seq,
                        lamport: event.lamport,
                        sent_ms: event.sent_ms,
                        text: note.clone(),
                        reply_to: None,
                        attachments: Vec::new(),
                        edited: false,
                        deleted: false,
                        system: true,
                        reactions: BTreeMap::new(),
                        forwarded: None,
                        pinned: None,
                    },
                );
            }
        }
        ChatKind::Pin { target, pinned } => {
            if let Some(index) = find(messages, target) {
                if messages[index].deleted {
                    return;
                }
                messages[index].pinned = pinned.then(|| author.to_string());
                if *pinned {
                    // « X a épinglé un message » (texte « 📌 » ; la cible est dans `reply_to`).
                    insert(
                        messages,
                        Msg {
                            id: format!("epingle-{author}-{}", event.seq),
                            author: author.to_string(),
                            seq: event.seq,
                            lamport: event.lamport,
                            sent_ms: event.sent_ms,
                            text: PIN_NOTE.to_string(),
                            reply_to: Some(target.clone()),
                            attachments: Vec::new(),
                            edited: false,
                            deleted: false,
                            system: true,
                            reactions: BTreeMap::new(),
                            forwarded: None,
                            pinned: None,
                        },
                    );
                }
            }
        }
        ChatKind::Unknown => {}
    }
}

/// Texte des messages système « a épinglé un message ».
const PIN_NOTE: &str = "📌";

/// Vérifie un événement reçu (longueurs, identifiants) avant de l'appliquer.
fn validate_members(members: &[MemberInfo]) -> Result<()> {
    if members.len() > MAX_GROUP_MEMBERS
        || members.iter().any(|m| !valid_name(&m.id) || m.name.chars().count() > 80 || m.platform.len() > 32)
        || members.iter().any(|m| crypto::public_from_b64(&m.public_key).is_err())
    {
        bail!("groupe invalide");
    }
    Ok(())
}

fn validate(kind: &ChatKind) -> Result<()> {
    match kind {
        ChatKind::Post { id, text, reply_to, attachments, forwarded } => {
            if forwarded.as_deref().is_some_and(|name| name.chars().count() > 80) {
                bail!("message invalide");
            }
            if !valid_name(id) || text.len() > MAX_CHAT_TEXT * 4 || attachments.len() > MAX_ATTACHMENTS {
                bail!("message invalide");
            }
            if reply_to.as_deref().is_some_and(|reply| !valid_name(reply)) {
                bail!("message invalide");
            }
            if attachments.iter().any(|a| !valid_name(&a.id) || a.size > MAX_ATTACHMENT_BYTES) {
                bail!("pièce jointe invalide");
            }
        }
        ChatKind::Edit { target, text } => {
            if !valid_name(target) || text.len() > MAX_CHAT_TEXT * 4 {
                bail!("modification invalide");
            }
        }
        ChatKind::Delete { target } => {
            if !valid_name(target) {
                bail!("suppression invalide");
            }
        }
        ChatKind::React { target, emoji, .. } => {
            if !valid_name(target) || emoji.is_empty() || emoji.len() > 32 {
                bail!("réaction invalide");
            }
        }
        ChatKind::GroupUpdate { name, members } => {
            if name.chars().count() > 80 {
                bail!("groupe invalide");
            }
            validate_members(members)?;
        }
        ChatKind::AddMembers { members } => {
            if members.is_empty() {
                bail!("groupe invalide");
            }
            validate_members(members)?;
        }
        ChatKind::Leave | ChatKind::Unknown => {}
        ChatKind::Pin { target, .. } => {
            if !valid_name(target) {
                bail!("épingle invalide");
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ réception (appelée par le moteur)

impl Inner {
    fn me_as_member(&self) -> MemberInfo {
        let identity = self.identity.read().unwrap_or_else(|p| p.into_inner());
        MemberInfo {
            id: identity.id.clone(),
            name: identity.name.clone(),
            platform: identity.platform.clone(),
            public_key: identity.public_key.clone(),
        }
    }

    /// Applique les événements d'un auteur. Retourne le dernier numéro appliqué sans trou.
    pub(crate) fn receive_chat_events(
        &self,
        sender: &SenderInfo,
        conversation_id: &str,
        events: Vec<ChatEvent>,
        group: Option<GroupInfo>,
    ) -> Result<u64> {
        if !valid_name(conversation_id) {
            bail!("conversation invalide");
        }
        let me = self.me_as_member();
        let mut chat = lock(&self.chat);

        if !chat.conversations.contains_key(conversation_id) {
            // Première fois : conversation à deux, ou groupe présenté par son créateur.
            let first = events.iter().find(|event| event.seq == 1);
            let meta = if conversation_id == direct_id(&me.id, &sender.id) && sender.trusted {
                Meta {
                    id: conversation_id.to_string(),
                    kind: ConversationKind::Direct,
                    name: String::new(),
                    creator: sender.id.clone(),
                    members: vec![
                        me.clone(),
                        MemberInfo {
                            id: sender.id.clone(),
                            name: sender.name.clone(),
                            platform: sender.platform.clone(),
                            public_key: sender.public_key.clone(),
                        },
                    ],
                    closed: false,
                    my_seq: 0,
                    lamport: 0,
                    applied: BTreeMap::new(),
                    acked: BTreeMap::new(),
                    my_read: 0,
                    read_by: BTreeMap::new(),
                    read_sent: BTreeMap::new(),
                    local_files: BTreeMap::new(),
                    created_ms: now_ms(),
                    left: BTreeSet::new(),
                }
            } else if let Some(info) = group.as_ref().filter(|info| {
                // Groupe présenté par un appareil associé qui en est membre (ajout par n'importe quel
                // membre, ou ajout après la création) : la liste vient de lui.
                sender.trusted
                    && conversation_id.starts_with("g-")
                    && info.name.chars().count() <= 80
                    && valid_name(&info.creator)
                    && validate_members(&info.members).is_ok()
                    && info.members.iter().any(|m| m.id == me.id && m.public_key == me.public_key)
                    && info.members.iter().any(|m| m.id == sender.id && m.public_key == sender.public_key)
            }) {
                Meta {
                    id: conversation_id.to_string(),
                    kind: ConversationKind::Group,
                    name: info.name.clone(),
                    creator: info.creator.clone(),
                    members: info.members.clone(),
                    closed: false,
                    my_seq: 0,
                    lamport: 0,
                    applied: BTreeMap::new(),
                    acked: BTreeMap::new(),
                    my_read: 0,
                    read_by: BTreeMap::new(),
                    read_sent: BTreeMap::new(),
                    local_files: BTreeMap::new(),
                    created_ms: now_ms(),
                    left: BTreeSet::new(),
                }
            } else if let Some(ChatEvent { kind: ChatKind::GroupUpdate { members, .. }, .. }) = first {
                if !sender.trusted
                    || !conversation_id.starts_with("g-")
                    || !members.iter().any(|m| m.id == me.id && m.public_key == me.public_key)
                    || !members.iter().any(|m| m.id == sender.id && m.public_key == sender.public_key)
                {
                    return Err(refusal("unknown-conversation", "groupe inconnu"));
                }
                Meta {
                    id: conversation_id.to_string(),
                    kind: ConversationKind::Group,
                    name: String::new(),
                    creator: sender.id.clone(),
                    members: Vec::new(),
                    closed: false,
                    my_seq: 0,
                    lamport: 0,
                    applied: BTreeMap::new(),
                    acked: BTreeMap::new(),
                    my_read: 0,
                    read_by: BTreeMap::new(),
                    read_sent: BTreeMap::new(),
                    local_files: BTreeMap::new(),
                    created_ms: now_ms(),
                    left: BTreeSet::new(),
                }
            } else {
                return Err(refusal("unknown-conversation", "conversation inconnue pour l'instant"));
            };
            chat.create(meta);
        }

        {
            let conversation = &chat.conversations[conversation_id];
            let is_new_group = conversation.meta.members.is_empty();
            if !is_new_group && !chat.is_member(conversation_id, sender) {
                return Err(refusal("not-member", "vous n'êtes pas membre de cette conversation"));
            }
        }

        let mut last = chat.conversations[conversation_id].meta.applied.get(&sender.id).copied().unwrap_or(0);
        // Messages nouveaux (pour les notifications) : texte et types de pièces jointes.
        let mut posts: Vec<(String, Vec<AttachmentKind>)> = Vec::new();
        let mut events = events;
        events.sort_by_key(|event| event.seq);
        for event in events {
            if event.seq <= last {
                continue; // déjà reçu
            }
            if event.seq != last + 1 {
                break; // trou : l'auteur renverra à partir de `last + 1`
            }
            if validate(&event.kind).is_err() {
                // Événement illisible : on avance quand même pour ne pas bloquer la suite.
                let conversation = chat.conversations.get_mut(conversation_id).expect("présente");
                conversation.meta.applied.insert(sender.id.clone(), event.seq);
                last = event.seq;
                continue;
            }
            if let ChatKind::Post { text, attachments, .. } = &event.kind {
                posts.push((text.clone(), attachments.iter().map(|a| a.kind).collect()));
            }
            chat.record(conversation_id, &sender.id, event.clone());
            last = event.seq;
        }
        chat.typing.remove(&(conversation_id.to_string(), sender.id.clone()));

        // Membres connus de l'expéditeur et pas encore de nous (ajout dont l'événement n'est pas
        // encore arrivé, ou écrit par une version qui ne le comprenait pas).
        if let Some(info) = group.filter(|info| validate_members(&info.members).is_ok()) {
            if let Some(conversation) = chat.conversations.get_mut(conversation_id) {
                let meta = &mut conversation.meta;
                if meta.kind == ConversationKind::Group && !meta.closed {
                    let left = meta.left.clone();
                    if !merge_members(&mut meta.members, &info.members, &left).is_empty() {
                        chat.save();
                        chat.bump();
                    }
                }
            }
        }
        let group = chat.conversations.get(conversation_id).and_then(|c| {
            (c.meta.kind == ConversationKind::Group).then(|| c.meta.name.clone())
        });
        let closed = chat.conversations.get(conversation_id).is_some_and(|c| c.meta.closed);
        drop(chat);
        if !posts.is_empty() && !closed {
            self.notify_posts(conversation_id, group, sender, &posts);
        }
        Ok(last)
    }

    /// Notifications des messages reçus (trois au plus, puis un résumé).
    fn notify_posts(&self, conversation: &str, group: Option<String>, sender: &SenderInfo, posts: &[(String, Vec<AttachmentKind>)]) {
        let author = self.display_name(&sender.id).unwrap_or_else(|| sender.name.clone());
        let describe = |text: &str, kinds: &[AttachmentKind]| -> String {
            let plain = crate::markdown::plain(text);
            let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
            if !plain.is_empty() {
                return if plain.chars().count() > 180 { format!("{}…", plain.chars().take(180).collect::<String>()) } else { plain };
            }
            match kinds.first() {
                Some(AttachmentKind::Image) => "🖼️ Photo".to_string(),
                Some(AttachmentKind::Gif) => "GIF".to_string(),
                Some(AttachmentKind::Video) => "🎬 Vidéo".to_string(),
                Some(AttachmentKind::Sticker) => "Autocollant".to_string(),
                Some(AttachmentKind::File) => "📎 Fichier".to_string(),
                None => "Nouveau message".to_string(),
            }
        };
        let (title, prefix) = match &group {
            Some(name) if !name.is_empty() => (name.clone(), format!("{author} : ")),
            Some(_) => ("Groupe".to_string(), format!("{author} : ")),
            None => (author.clone(), String::new()),
        };
        let shown = if posts.len() > 3 { 2 } else { posts.len() };
        for (text, kinds) in &posts[..shown] {
            self.notify(crate::engine::Notice {
                conversation: Some(conversation.to_string()),
                title: title.clone(),
                body: format!("{prefix}{}", describe(text, kinds)),
            });
        }
        if posts.len() > shown {
            self.notify(crate::engine::Notice {
                conversation: Some(conversation.to_string()),
                title,
                body: format!("{prefix}{} nouveaux messages", posts.len() - shown),
            });
        }
    }

    pub(crate) fn receive_chat_typing(&self, sender: &SenderInfo, conversation_id: &str) -> Result<()> {
        let mut chat = lock(&self.chat);
        if chat.is_member(conversation_id, sender) {
            chat.typing.insert((conversation_id.to_string(), sender.id.clone()), Instant::now());
            chat.bump();
        }
        Ok(())
    }

    pub(crate) fn receive_chat_read(&self, sender: &SenderInfo, conversation_id: &str, lamport: u64) -> Result<()> {
        let mut chat = lock(&self.chat);
        if !chat.is_member(conversation_id, sender) {
            return Ok(());
        }
        if let Some(conversation) = chat.conversations.get_mut(conversation_id) {
            let read = conversation.meta.read_by.entry(sender.id.clone()).or_insert(0);
            if lamport > *read {
                *read = lamport;
                chat.save();
                chat.bump();
            }
        }
        Ok(())
    }

    /// Vérifie qu'une pièce jointe entrante vient bien d'un membre de la conversation.
    pub(crate) fn chat_link_allowed(&self, sender: &SenderInfo, link: &ChatLink) -> bool {
        valid_name(&link.conversation) && valid_name(&link.attachment) && lock(&self.chat).is_member(&link.conversation, sender)
    }

    /// Emplacement définitif d'une pièce jointe reçue.
    pub(crate) fn chat_media_path(&self, link: &ChatLink, file_name: &str) -> PathBuf {
        lock(&self.chat).media_path(&link.conversation, &link.attachment, file_name)
    }

    pub(crate) fn chat_changed(&self) {
        lock(&self.chat).bump();
    }

    /// Membre `id` de la conversation (avec la clé présentée dans le groupe).
    pub(crate) fn group_member(&self, conversation: &str, id: &str) -> Option<MemberInfo> {
        lock(&self.chat).member(conversation, id)
    }

    /// Groupes ouverts dont `id` est membre : (conversation, nom).
    pub(crate) fn shared_groups(&self, id: &str) -> Vec<(String, String)> {
        let chat = lock(&self.chat);
        let mut groups: Vec<(String, String)> = chat
            .conversations
            .values()
            .filter(|c| c.meta.kind == ConversationKind::Group && !c.meta.closed && c.meta.members.iter().any(|m| m.id == id))
            .map(|c| (c.meta.id.clone(), c.meta.name.clone()))
            .collect();
        groups.sort_by(|a, b| a.1.cmp(&b.1));
        groups
    }

    /// Ce membre, tel que présenté dans un de nos groupes.
    pub(crate) fn any_member(&self, id: &str) -> Option<MemberInfo> {
        lock(&self.chat).conversations.values().flat_map(|c| c.meta.members.iter()).find(|m| m.id == id).cloned()
    }

    /// Nom d'un groupe (vide pour une conversation à deux ou inconnue).
    pub(crate) fn group_title(&self, conversation: &str) -> String {
        lock(&self.chat).conversations.get(conversation).map(|c| c.meta.name.clone()).unwrap_or_default()
    }

    /// Clé d'un membre de groupe (présenté par son créateur), pour une connexion entrante.
    pub(crate) fn chat_member_by_key(&self, id: &str, public_key: &str) -> Option<MemberInfo> {
        lock(&self.chat).member_by_key(id, public_key)
    }

    /// Appareil joignable pour cette conversation : associé, ou présenté dans le groupe.
    pub(crate) fn chat_target(&self, conversation: &str, member_id: &str) -> Option<Target> {
        let member = lock(&self.chat).member(conversation, member_id)?;
        if let Some(trusted) = lock(&self.trusted).get(member_id) {
            if trusted.public_key != member.public_key {
                return None; // clé différente de celle que nous avons vérifiée : on n'envoie rien
            }
        }
        let peers = lock(&self.peers);
        let peer = peers.get(member_id)?;
        if peer.device.public_key != member.public_key {
            return None;
        }
        Some(Target {
            id: member_id.to_string(),
            name: member.name.clone(),
            route: peer.route()?,
            public_key: crypto::public_from_b64(&member.public_key).ok()?,
        })
    }

    // ---------------------------------------------------------------- synchronisation

    /// Envoie à `member` nos événements non confirmés et notre accusé de lecture.
    fn sync_member(&self, conversation: &str, member: &str) -> Result<()> {
        let target = self.chat_target(conversation, member).ok_or_else(|| anyhow!("hors ligne"))?;
        let mut channel = self.connect(&target)?;
        loop {
            let (batch, group): (Vec<ChatEvent>, Option<GroupInfo>) = {
                let chat = lock(&self.chat);
                let Some(state) = chat.conversations.get(conversation) else { return Ok(()) };
                let group = (state.meta.kind == ConversationKind::Group).then(|| GroupInfo {
                    name: state.meta.name.clone(),
                    creator: state.meta.creator.clone(),
                    members: state.meta.members.clone(),
                });
                let acked = state.meta.acked.get(member).copied().unwrap_or(0);
                let me = &chat.me;
                let mut size = 0;
                let batch: Vec<ChatEvent> = state
                    .events
                    .iter()
                    .filter(|stored| &stored.author == me && stored.event.seq > acked)
                    .take_while(|stored| {
                        size += serde_json::to_vec(&stored.event).map(|v| v.len()).unwrap_or(0);
                        size < BATCH_BYTES
                    })
                    .take(BATCH_EVENTS)
                    .map(|stored| stored.event.clone())
                    .collect();
                (batch, group)
            };
            let Some(last) = batch.last().map(|event| event.seq) else { break };
            let message = Message::ChatEvents { conversation: conversation.to_string(), events: batch, group };
            self.write_message(&mut channel, &message, &[])?;
            let ack = read_ack(&mut channel).ok_or_else(|| anyhow!("{} ne répond pas", target.name))?;
            if !ack.ok {
                bail!("{}", explain_refusal(&ack, &target.name));
            }
            let applied = ack.offset.unwrap_or(0);
            {
                let mut chat = lock(&self.chat);
                if let Some(state) = chat.conversations.get_mut(conversation) {
                    state.meta.acked.insert(member.to_string(), applied);
                }
                chat.save();
                chat.bump();
            }
            if applied < last {
                break; // l'autre côté a un trou (ou a tout perdu) : on reprendra depuis `applied`
            }
        }

        let read = {
            let chat = lock(&self.chat);
            chat.conversations.get(conversation).and_then(|state| {
                let sent = state.meta.read_sent.get(member).copied().unwrap_or(0);
                (state.meta.my_read > sent).then_some(state.meta.my_read)
            })
        };
        if let Some(lamport) = read {
            self.write_message(&mut channel, &Message::ChatRead { conversation: conversation.to_string(), lamport }, &[])?;
        }
        let _ = channel.finish();
        match read_ack(&mut channel) {
            Some(ack) if ack.ok => {
                if let Some(lamport) = read {
                    let mut chat = lock(&self.chat);
                    if let Some(state) = chat.conversations.get_mut(conversation) {
                        state.meta.read_sent.insert(member.to_string(), lamport);
                    }
                    chat.save();
                }
                Ok(())
            }
            Some(ack) => bail!("{}", explain_refusal(&ack, &target.name)),
            None => Ok(()),
        }
    }
}

/// Fil de synchronisation : pousse les événements non confirmés vers chaque membre joignable.
pub(crate) fn spawn_chat_scheduler(inner: Arc<Inner>) {
    thread::spawn(move || {
        while !inner.shutdown.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(200));

            let work: Vec<(String, String)> = {
                let mut chat = lock(&inner.chat);
                let before = chat.typing.len();
                chat.typing.retain(|_, at| at.elapsed() < TYPING_SHOWN);
                if chat.typing.len() != before {
                    chat.bump();
                }
                let me = chat.me.clone();
                let now = Instant::now();
                let mut work = Vec::new();
                for state in chat.conversations.values() {
                    let my_last = state.meta.my_seq;
                    for member in state.meta.members.iter().filter(|m| m.id != me) {
                        let key = (state.meta.id.clone(), member.id.clone());
                        let acked = state.meta.acked.get(&member.id).copied().unwrap_or(0);
                        let read_sent = state.meta.read_sent.get(&member.id).copied().unwrap_or(0);
                        let pending = acked < my_last || state.meta.my_read > read_sent;
                        let waiting = chat.retry.get(&key).is_some_and(|(next, _)| *next > now);
                        if pending && !waiting && !chat.syncing.contains(&key) {
                            work.push(key);
                        }
                    }
                }
                work
            };

            for (conversation, member) in work {
                if inner.chat_target(&conversation, &member).is_none() {
                    continue;
                }
                lock(&inner.chat).syncing.insert((conversation.clone(), member.clone()));
                let worker = inner.clone();
                thread::spawn(move || {
                    let result = worker.sync_member(&conversation, &member);
                    let mut chat = lock(&worker.chat);
                    let key = (conversation, member);
                    chat.syncing.remove(&key);
                    match result {
                        Ok(()) => {
                            chat.retry.remove(&key);
                        }
                        Err(error) => {
                            let failures = chat.retry.get(&key).map(|(_, f)| f + 1).unwrap_or(1);
                            let delay = SYNC_RETRY_MIN.saturating_mul(1 << (failures - 1).min(4)).min(SYNC_RETRY_MAX);
                            eprintln!("[discussion] envoi à {} reporté ({error}), nouvel essai dans {delay:?}", key.1);
                            chat.retry.insert(key, (Instant::now() + delay, failures));
                        }
                    }
                });
            }
        }
    });
}

// ------------------------------------------------------------------ API pour l'interface

impl Engine {
    /// Numéro qui change à chaque modification des discussions (l'interface ne relit que dans ce cas).
    pub fn chat_revision(&self) -> u64 {
        lock(&self.inner.chat).revision
    }

    /// Ouvre (ou crée) la conversation à deux avec un appareil associé.
    pub fn open_direct(&self, peer_id: &str) -> Result<String> {
        let inner = &self.inner;
        let peer: TrustedPeer = lock(&inner.trusted)
            .get(peer_id)
            .cloned()
            .ok_or_else(|| anyhow!("associez d'abord cet appareil"))?;
        let me = inner.me_as_member();
        let id = direct_id(&me.id, peer_id);
        let mut chat = lock(&inner.chat);
        if !chat.conversations.contains_key(&id) {
            chat.create(Meta {
                id: id.clone(),
                kind: ConversationKind::Direct,
                name: String::new(),
                creator: me.id.clone(),
                members: vec![
                    me,
                    MemberInfo { id: peer.id.clone(), name: peer.name.clone(), platform: peer.platform.clone(), public_key: peer.public_key.clone() },
                ],
                closed: false,
                my_seq: 0,
                lamport: 0,
                applied: BTreeMap::new(),
                acked: BTreeMap::new(),
                my_read: 0,
                read_by: BTreeMap::new(),
                read_sent: BTreeMap::new(),
                local_files: BTreeMap::new(),
                created_ms: now_ms(),
                    left: BTreeSet::new(),
            });
        }
        Ok(id)
    }

    fn trusted_members(&self, ids: &[String]) -> Result<Vec<MemberInfo>> {
        let trusted = lock(&self.inner.trusted);
        ids.iter()
            .map(|id| {
                trusted
                    .get(id)
                    .map(|peer| MemberInfo {
                        id: peer.id.clone(),
                        name: peer.name.clone(),
                        platform: peer.platform.clone(),
                        public_key: peer.public_key.clone(),
                    })
                    .ok_or_else(|| anyhow!("seuls les appareils que vous avez associés peuvent être ajoutés"))
            })
            .collect()
    }

    /// Crée un groupe avec des appareils associés ; leurs clés sont présentées aux autres membres.
    pub fn create_group(&self, name: &str, member_ids: &[String]) -> Result<String> {
        let name = name.trim();
        if name.is_empty() {
            bail!("donnez un nom au groupe");
        }
        if member_ids.is_empty() {
            bail!("choisissez au moins un appareil");
        }
        let mut members = vec![self.inner.me_as_member()];
        members.extend(self.trusted_members(member_ids)?);
        let id = format!("g-{}", uuid::Uuid::new_v4());
        let mut chat = lock(&self.inner.chat);
        let me = chat.me.clone();
        chat.create(Meta {
            id: id.clone(),
            kind: ConversationKind::Group,
            name: String::new(),
            creator: me,
            members: Vec::new(),
            closed: false,
            my_seq: 0,
            lamport: 0,
            applied: BTreeMap::new(),
            acked: BTreeMap::new(),
            my_read: 0,
            read_by: BTreeMap::new(),
            read_sent: BTreeMap::new(),
            local_files: BTreeMap::new(),
            created_ms: now_ms(),
                    left: BTreeSet::new(),
        });
        chat.emit(&id, ChatKind::GroupUpdate { name: name.chars().take(80).collect(), members })?;
        Ok(id)
    }

    /// Ajoute au groupe des appareils associés. N'importe quel membre peut le faire : leurs clés
    /// sont présentées aux autres membres, qui peuvent ensuite leur demander d'être amis.
    pub fn add_group_members(&self, conversation: &str, member_ids: &[String]) -> Result<()> {
        let added = self.trusted_members(member_ids)?;
        let mut chat = lock(&self.inner.chat);
        let me = chat.me.clone();
        let state = chat.conversations.get(conversation).ok_or_else(|| anyhow!("groupe introuvable"))?;
        if state.meta.kind != ConversationKind::Group {
            bail!("on n'ajoute des membres qu'à un groupe");
        }
        if state.meta.closed {
            bail!("vous avez quitté ce groupe");
        }
        let new: Vec<MemberInfo> =
            added.into_iter().filter(|member| !state.meta.members.iter().any(|m| m.id == member.id)).collect();
        if new.is_empty() {
            bail!("ces appareils sont déjà dans le groupe");
        }
        if state.meta.members.len() + new.len() > MAX_GROUP_MEMBERS {
            bail!("un groupe compte au plus {MAX_GROUP_MEMBERS} membres");
        }
        // Le créateur garde l'ancien événement (compris aussi par les versions 0.9), sauf pour
        // faire revenir un membre parti.
        let kind = if state.meta.creator == me && new.iter().all(|m| !state.meta.left.contains(&m.id)) {
            let mut members = state.meta.members.clone();
            members.extend(new);
            ChatKind::GroupUpdate { name: state.meta.name.clone(), members }
        } else {
            ChatKind::AddMembers { members: new }
        };
        chat.emit(conversation, kind)?;
        Ok(())
    }

    pub fn rename_group(&self, conversation: &str, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            bail!("donnez un nom au groupe");
        }
        let mut chat = lock(&self.inner.chat);
        let state = chat.conversations.get(conversation).ok_or_else(|| anyhow!("groupe introuvable"))?;
        if state.meta.creator != chat.me {
            bail!("seul le créateur du groupe peut le renommer");
        }
        let members = state.meta.members.clone();
        chat.emit(conversation, ChatKind::GroupUpdate { name: name.chars().take(80).collect(), members })?;
        Ok(())
    }

    pub fn leave_group(&self, conversation: &str) -> Result<()> {
        let mut chat = lock(&self.inner.chat);
        let kind = chat.conversations.get(conversation).map(|c| c.meta.kind).ok_or_else(|| anyhow!("groupe introuvable"))?;
        if kind != ConversationKind::Group {
            bail!("on ne quitte qu'un groupe");
        }
        chat.emit(conversation, ChatKind::Leave)?;
        Ok(())
    }

    /// Envoie un message (Markdown), éventuellement en réponse, avec des pièces jointes.
    pub fn send_chat(&self, conversation: &str, text: &str, reply_to: Option<&str>, files: &[PathBuf]) -> Result<()> {
        self.send_chat_with_kind(conversation, text, reply_to, files, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn send_chat_with_kind(
        &self,
        conversation: &str,
        text: &str,
        reply_to: Option<&str>,
        files: &[PathBuf],
        forced_kind: Option<AttachmentKind>,
        forwarded: Option<String>,
    ) -> Result<()> {
        let text = text.trim_end();
        if text.trim().is_empty() && files.is_empty() {
            bail!("message vide");
        }
        if text.chars().count() > MAX_CHAT_TEXT {
            bail!("message trop long ({} caractères au plus)", MAX_CHAT_TEXT);
        }
        if files.len() > MAX_ATTACHMENTS {
            bail!("{MAX_ATTACHMENTS} pièces jointes au plus par message");
        }
        let mut attachments = Vec::new();
        for path in files {
            let meta = fs::metadata(path).with_context(|| format!("« {} » est illisible", path.display()))?;
            if !meta.is_file() {
                bail!("« {} » n'est pas un fichier", path.display());
            }
            if meta.len() > MAX_ATTACHMENT_BYTES {
                bail!("« {} » dépasse 1 Go", path.display());
            }
            attachments.push((
                path.clone(),
                AttachmentInfo {
                    id: uuid::Uuid::new_v4().to_string(),
                    file_name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "fichier".into()),
                    size: meta.len(),
                    kind: forced_kind.unwrap_or_else(|| attachment_kind(path)),
                },
            ));
        }

        let (members, me) = {
            let mut chat = lock(&self.inner.chat);
            let state = chat.conversations.get_mut(conversation).ok_or_else(|| anyhow!("conversation introuvable"))?;
            for (path, info) in &attachments {
                state.meta.local_files.insert(info.id.clone(), path.to_string_lossy().to_string());
            }
            let members = state.meta.members.clone();
            let me = chat.me.clone();
            chat.emit(
                conversation,
                ChatKind::Post {
                    id: uuid::Uuid::new_v4().to_string(),
                    text: text.to_string(),
                    reply_to: reply_to.map(str::to_string),
                    attachments: attachments.iter().map(|(_, info)| info.clone()).collect(),
                    forwarded,
                },
            )?;
            (members, me)
        };

        // Les pièces jointes partent par les transferts reprenables, vers chaque membre.
        for (path, info) in &attachments {
            for member in members.iter().filter(|m| m.id != me) {
                self.inner.queue_chat_file(
                    path,
                    &info.file_name,
                    info.size,
                    &member.id,
                    &member.name,
                    ChatLink { conversation: conversation.to_string(), attachment: info.id.clone() },
                );
            }
        }
        Ok(())
    }

    pub fn edit_chat(&self, conversation: &str, message: &str, text: &str) -> Result<()> {
        let text = text.trim_end();
        if text.trim().is_empty() {
            bail!("message vide");
        }
        let mut chat = lock(&self.inner.chat);
        let me = chat.me.clone();
        let mine = chat
            .conversations
            .get(conversation)
            .and_then(|c| c.messages.iter().find(|m| m.id == message))
            .is_some_and(|m| m.author == me && !m.deleted && !m.system);
        if !mine {
            bail!("vous ne pouvez modifier que vos propres messages");
        }
        chat.emit(conversation, ChatKind::Edit { target: message.to_string(), text: text.to_string() })?;
        Ok(())
    }

    pub fn delete_chat(&self, conversation: &str, message: &str) -> Result<()> {
        let mut chat = lock(&self.inner.chat);
        let me = chat.me.clone();
        let mine = chat
            .conversations
            .get(conversation)
            .and_then(|c| c.messages.iter().find(|m| m.id == message))
            .is_some_and(|m| m.author == me && !m.system);
        if !mine {
            bail!("vous ne pouvez supprimer que vos propres messages");
        }
        chat.emit(conversation, ChatKind::Delete { target: message.to_string() })?;
        Ok(())
    }

    /// Ajoute ou retire notre réaction.
    pub fn toggle_reaction(&self, conversation: &str, message: &str, emoji: &str) -> Result<()> {
        let mut chat = lock(&self.inner.chat);
        let me = chat.me.clone();
        let add = !chat
            .conversations
            .get(conversation)
            .and_then(|c| c.messages.iter().find(|m| m.id == message))
            .ok_or_else(|| anyhow!("message introuvable"))?
            .reactions
            .get(emoji)
            .is_some_and(|who| who.contains(&me));
        chat.emit(conversation, ChatKind::React { target: message.to_string(), emoji: emoji.to_string(), add })?;
        Ok(())
    }

    /// La conversation est affichée : tout ce qui y est est lu.
    pub fn mark_read(&self, conversation: &str) {
        let mut chat = lock(&self.inner.chat);
        if let Some(state) = chat.conversations.get_mut(conversation) {
            if state.meta.lamport > state.meta.my_read {
                state.meta.my_read = state.meta.lamport;
                chat.save();
                chat.bump();
            }
        }
    }

    /// L'utilisateur tape : prévient les membres joignables (au plus toutes les 3 s).
    pub fn notify_typing(&self, conversation: &str) {
        let members = {
            let mut chat = lock(&self.inner.chat);
            if chat.typing_sent.get(conversation).is_some_and(|at| at.elapsed() < TYPING_THROTTLE) {
                return;
            }
            chat.typing_sent.insert(conversation.to_string(), Instant::now());
            let me = chat.me.clone();
            chat.conversations
                .get(conversation)
                .filter(|state| !state.meta.closed)
                .map(|state| state.meta.members.iter().filter(|m| m.id != me).map(|m| m.id.clone()).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        for member in members {
            let Some(target) = self.inner.chat_target(conversation, &member) else { continue };
            // « En train d'écrire » : pas en Bluetooth (une connexion lente, qui retarderait les messages).
            if matches!(target.route, crate::engine::Route::Bluetooth(_)) {
                continue;
            }
            let inner = self.inner.clone();
            let conversation = conversation.to_string();
            thread::spawn(move || {
                if let Ok(mut channel) = inner.connect(&target) {
                    let _ = inner.write_message(&mut channel, &Message::ChatTyping { conversation }, &[]);
                    let _ = channel.finish();
                    let _ = read_ack(&mut channel);
                }
            });
        }
    }

    pub fn conversations(&self) -> Vec<ConversationView> {
        let online: HashSet<String> = lock(&self.inner.peers)
            .iter()
            .filter(|(_, peer)| peer.online())
            .map(|(id, _)| id.clone())
            .collect();
        let display = self.inner.display_names();
        let friend_state = self.inner.friend_states();
        let chat = lock(&self.inner.chat);
        let me = chat.me.clone();
        let mut list: Vec<ConversationView> = chat
            .conversations
            .values()
            .map(|state| {
                let meta = &state.meta;
                let name_of = |id: &str| {
                    display.get(id).cloned().unwrap_or_else(|| meta.members.iter().find(|m| m.id == id).map(|m| m.name.clone()).unwrap_or_default())
                };
                let title = match meta.kind {
                    ConversationKind::Direct => meta.members.iter().find(|m| m.id != me).map(|m| name_of(&m.id)).unwrap_or_default(),
                    ConversationKind::Group => meta.name.clone(),
                };
                let last = state.messages.iter().rev().find(|m| !m.deleted);
                let last_preview = last
                    .map(|m| {
                        let body = if m.text.is_empty() && !m.attachments.is_empty() {
                            format!("📎 {} pièce(s) jointe(s)", m.attachments.len())
                        } else {
                            excerpt(&m.text)
                        };
                        if m.system || meta.kind == ConversationKind::Direct || m.author == me {
                            if m.author == me && !m.system { format!("Vous : {body}") } else { body }
                        } else {
                            format!("{} : {body}", name_of(&m.author))
                        }
                    })
                    .unwrap_or_else(|| "Aucun message".to_string());
                let unread = state
                    .messages
                    .iter()
                    .filter(|m| m.author != me && !m.system && !m.deleted && m.lamport > meta.my_read)
                    .count();
                let typing = chat
                    .typing
                    .iter()
                    .filter(|((conversation, _), at)| conversation == &meta.id && at.elapsed() < TYPING_SHOWN)
                    .map(|((_, author), _)| name_of(author))
                    .collect();
                ConversationView {
                    id: meta.id.clone(),
                    kind: meta.kind,
                    title,
                    members: meta
                        .members
                        .iter()
                        .map(|m| MemberView {
                            id: m.id.clone(),
                            name: name_of(&m.id),
                            online: m.id == me || online.contains(&m.id),
                            me: m.id == me,
                            friend: friend_state(m, m.id == me),
                        })
                        .collect(),
                    unread,
                    last_preview,
                    last_ms: last.map(|m| m.sent_ms).unwrap_or(meta.created_ms),
                    typing,
                    is_creator: meta.creator == me,
                    closed: meta.closed,
                }
            })
            .collect();
        list.sort_by_key(|conversation| std::cmp::Reverse(conversation.last_ms));
        list
    }

    /// Les `limit` derniers messages d'une conversation, dans l'ordre d'affichage.
    pub fn messages(&self, conversation: &str, limit: usize) -> Vec<MessageView> {
        let progress: HashMap<String, f32> = lock(&self.inner.transfers)
            .iter()
            .filter(|t| !t.sending)
            .filter_map(|t| t.attachment.clone().map(|id| (id, t.progress)))
            .collect();
        let display = self.inner.display_names();
        let chat = lock(&self.inner.chat);
        let me = chat.me.clone();
        let Some(state) = chat.conversations.get(conversation) else { return Vec::new() };
        let meta = &state.meta;
        let name_of = |id: &str| {
            if id == me {
                "Vous".to_string()
            } else if let Some(name) = display.get(id) {
                name.clone()
            } else {
                meta.members.iter().find(|m| m.id == id).map(|m| m.name.clone()).unwrap_or_else(|| "Ancien membre".to_string())
            }
        };
        let others: Vec<&MemberInfo> = meta.members.iter().filter(|m| m.id != me).collect();
        let start = state.messages.len().saturating_sub(limit);
        state.messages[start..]
            .iter()
            .map(|m| {
                let status = if m.author != me || m.system {
                    MessageStatus::None
                } else {
                    let readers: Vec<String> = others
                        .iter()
                        .filter(|o| meta.read_by.get(&o.id).copied().unwrap_or(0) >= m.lamport)
                        .map(|o| o.name.clone())
                        .collect();
                    if !readers.is_empty() {
                        MessageStatus::Read(readers)
                    } else if others.iter().all(|o| meta.acked.get(&o.id).copied().unwrap_or(0) >= m.seq) {
                        MessageStatus::Delivered
                    } else {
                        MessageStatus::Sending
                    }
                };
                let reply = m.reply_to.as_ref().map(|target| {
                    let original = state.messages.iter().find(|o| &o.id == target);
                    ReplyView {
                        id: target.clone(),
                        author_name: original.map(|o| name_of(&o.author)).unwrap_or_default(),
                        excerpt: original
                            .map(|o| if o.deleted { "Message supprimé".to_string() } else { excerpt(&o.text) })
                            .unwrap_or_else(|| "Message introuvable".to_string()),
                    }
                });
                let attachments = m
                    .attachments
                    .iter()
                    .map(|a| {
                        let path = if m.author == me {
                            meta.local_files.get(&a.id).map(PathBuf::from).filter(|p| p.exists())
                        } else {
                            Some(chat.media_path(conversation, &a.id, &a.file_name)).filter(|p| p.exists())
                        };
                        AttachmentView {
                            id: a.id.clone(),
                            file_name: a.file_name.clone(),
                            size: a.size,
                            kind: a.kind,
                            progress: if path.is_none() { progress.get(&a.id).copied() } else { None },
                            path,
                        }
                    })
                    .collect();
                let reactions = m
                    .reactions
                    .iter()
                    .map(|(emoji, who)| ReactionView {
                        emoji: emoji.clone(),
                        count: who.len(),
                        mine: who.contains(&me),
                        names: who.iter().map(|id| name_of(id)).collect(),
                    })
                    .collect();
                MessageView {
                    id: m.id.clone(),
                    author_id: m.author.clone(),
                    author_name: name_of(&m.author),
                    mine: m.author == me,
                    system: m.system,
                    text: if m.system && m.text == PIN_NOTE {
                        let pinned = m.reply_to.as_deref().and_then(|target| state.messages.iter().find(|o| o.id == target));
                        let what = pinned.map(|o| excerpt(&o.text)).filter(|t| !t.is_empty()).unwrap_or_else(|| "un message".to_string());
                        format!("📌 {} a épinglé « {what} »", name_of(&m.author))
                    } else {
                        m.text.clone()
                    },
                    sent_ms: m.sent_ms,
                    edited: m.edited,
                    deleted: m.deleted,
                    reply,
                    attachments,
                    reactions,
                    status,
                    lamport: m.lamport,
                    forwarded: m.forwarded.clone(),
                    pinned_by: m.pinned.as_deref().map(&name_of),
                }
            })
            .collect()
    }

    /// Épingle ou désépingle un message (n'importe quel membre).
    pub fn pin_chat(&self, conversation: &str, message: &str, pinned: bool) -> Result<()> {
        let mut chat = lock(&self.inner.chat);
        let state = chat.conversations.get(conversation).ok_or_else(|| anyhow!("conversation introuvable"))?;
        if state.meta.closed {
            bail!("vous avez quitté ce groupe");
        }
        let target = state.messages.iter().find(|m| m.id == message).ok_or_else(|| anyhow!("message introuvable"))?;
        if target.system || target.deleted {
            bail!("ce message ne peut pas être épinglé");
        }
        chat.emit(conversation, ChatKind::Pin { target: message.to_string(), pinned })?;
        Ok(())
    }

    /// Messages épinglés d'une conversation, du plus récent au plus ancien.
    pub fn pinned_messages(&self, conversation: &str) -> Vec<MessageView> {
        let mut pinned: Vec<MessageView> = self.messages(conversation, usize::MAX).into_iter().filter(|m| m.pinned_by.is_some() && !m.deleted).collect();
        pinned.reverse();
        pinned
    }

    /// Transfère un message (texte et pièces jointes disponibles) vers une autre conversation.
    pub fn forward_chat(&self, from: &str, message: &str, to: &str) -> Result<()> {
        let original = self
            .messages(from, usize::MAX)
            .into_iter()
            .find(|m| m.id == message && !m.system && !m.deleted)
            .ok_or_else(|| anyhow!("message introuvable"))?;
        let files: Vec<PathBuf> = original.attachments.iter().filter_map(|a| a.path.clone()).collect();
        if original.text.trim().is_empty() && files.is_empty() {
            bail!("ce message n'a rien à transférer (pièces jointes pas encore reçues)");
        }
        // Nom public (pseudo, sinon nom de l'appareil) : jamais un surnom donné ici.
        let author = match &original.forwarded {
            Some(name) => name.clone(),
            None => self.inner.public_name(&original.author_id),
        };
        self.send_chat_with_kind(to, &original.text, None, &files, None, Some(author))
    }

    /// Nombre total de messages non lus (pour la barre latérale).
    pub fn chat_unread(&self) -> usize {
        self.conversations().iter().map(|c| c.unread).sum()
    }

    // ---------------------------------------------------------------- images, autocollants, GIF

    /// Image du presse-papiers enregistrée en PNG (pour la coller dans une discussion).
    pub fn clipboard_image_to_file(&self) -> Result<PathBuf> {
        let mut clipboard = arboard::Clipboard::new().map_err(|e| anyhow!("presse-papiers inaccessible : {e}"))?;
        let image = clipboard.get_image().map_err(|_| anyhow!("pas d'image dans le presse-papiers"))?;
        let buffer = image::RgbaImage::from_raw(image.width as u32, image.height as u32, image.bytes.into_owned())
            .ok_or_else(|| anyhow!("image illisible"))?;
        let dir = lock(&self.inner.chat).dir.join(PASTE_DIR);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("image-collee-{}.png", now_ms()));
        buffer.save(&path).context("enregistrement de l'image impossible")?;
        Ok(path)
    }

    fn stickers_dir(&self) -> PathBuf {
        lock(&self.inner.chat).dir.join(STICKERS_DIR)
    }

    /// Bibliothèque d'autocollants et de GIF (sur cet appareil, sans Internet).
    pub fn stickers(&self) -> Vec<StickerView> {
        let Ok(entries) = fs::read_dir(self.stickers_dir()) else { return Vec::new() };
        let mut list: Vec<(u64, StickerView)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let id = path.file_stem()?.to_string_lossy().to_string();
                let animated = path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("gif"));
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                valid_name(&id).then_some((modified, StickerView { id, path, animated }))
            })
            .collect();
        list.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        list.into_iter().map(|(_, sticker)| sticker).collect()
    }

    /// Crée un autocollant à partir d'une image (réduite à 320 px) ou ajoute un GIF animé tel quel.
    pub fn add_sticker(&self, source: &Path) -> Result<StickerView> {
        let dir = self.stickers_dir();
        fs::create_dir_all(&dir)?;
        let id = uuid::Uuid::new_v4().to_string();
        let is_gif = source.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("gif"));
        let path = if is_gif {
            let size = fs::metadata(source)?.len();
            if size > MAX_STICKER_BYTES {
                bail!("GIF trop lourd pour un autocollant (8 Mo au plus)");
            }
            let path = dir.join(format!("{id}.gif"));
            fs::copy(source, &path)?;
            path
        } else {
            let image = image::open(source).map_err(|_| anyhow!("ce fichier n'est pas une image lisible"))?;
            let resized = image.thumbnail(STICKER_SIZE, STICKER_SIZE);
            let path = dir.join(format!("{id}.png"));
            resized.save(&path).context("enregistrement impossible")?;
            path
        };
        lock(&self.inner.chat).bump();
        Ok(StickerView { id, path, animated: is_gif })
    }

    pub fn remove_sticker(&self, id: &str) {
        if valid_name(id) {
            for extension in ["png", "gif"] {
                let _ = fs::remove_file(self.stickers_dir().join(format!("{id}.{extension}")));
            }
            lock(&self.inner.chat).bump();
        }
    }

    pub fn send_sticker(&self, conversation: &str, sticker: &str) -> Result<()> {
        let found = self.stickers().into_iter().find(|s| s.id == sticker).ok_or_else(|| anyhow!("autocollant introuvable"))?;
        let kind = if found.animated { AttachmentKind::Gif } else { AttachmentKind::Sticker };
        self.send_chat_with_kind(conversation, "", None, &[found.path], Some(kind), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_ids_match_on_both_sides() {
        assert_eq!(direct_id("alice", "bob"), direct_id("bob", "alice"));
        assert_ne!(direct_id("alice", "bob"), direct_id("alice", "carol"));
        assert!(valid_name(&direct_id("a", "b")));
    }

    #[test]
    fn messages_are_ordered_by_logical_clock_then_author() {
        let mut messages = Vec::new();
        let post = |author: &str, seq: u64, lamport: u64, id: &str| Stored {
            author: author.into(),
            event: ChatEvent {
                seq,
                lamport,
                sent_ms: 0,
                kind: ChatKind::Post { id: id.into(), text: id.into(), reply_to: None, attachments: vec![], forwarded: None },
            },
            note: None,
        };
        // Arrivées dans le désordre : l'ordre affiché est le même pour tous.
        for stored in [post("b", 1, 2, "m2"), post("a", 2, 3, "m3"), post("a", 1, 1, "m1"), post("c", 1, 2, "m2c")] {
            apply_to_messages(&mut messages, &stored);
        }
        let order: Vec<&str> = messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(order, ["m1", "m2", "m2c", "m3"]);
    }

    #[test]
    fn only_the_author_can_edit_or_delete() {
        let mut messages = Vec::new();
        let stored = |author: &str, kind: ChatKind| Stored { author: author.into(), event: ChatEvent { seq: 1, lamport: 1, sent_ms: 0, kind }, note: None };
        apply_to_messages(&mut messages, &stored("a", ChatKind::Post { id: "m".into(), text: "original".into(), reply_to: None, attachments: vec![], forwarded: None }));
        apply_to_messages(&mut messages, &stored("b", ChatKind::Edit { target: "m".into(), text: "piraté".into() }));
        apply_to_messages(&mut messages, &stored("b", ChatKind::Delete { target: "m".into() }));
        assert_eq!(messages[0].text, "original");
        assert!(!messages[0].deleted);
        apply_to_messages(&mut messages, &stored("a", ChatKind::Edit { target: "m".into(), text: "corrigé".into() }));
        assert_eq!(messages[0].text, "corrigé");
        assert!(messages[0].edited);
    }

    #[test]
    fn attachment_kinds() {
        assert_eq!(attachment_kind(Path::new("a.GIF")), AttachmentKind::Gif);
        assert_eq!(attachment_kind(Path::new("photo.jpeg")), AttachmentKind::Image);
        assert_eq!(attachment_kind(Path::new("film.mov")), AttachmentKind::Video);
        assert_eq!(attachment_kind(Path::new("notes.pdf")), AttachmentKind::File);
    }
}
