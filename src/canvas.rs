//! Canevas : des pages partagées dans une conversation (surtout les groupes), que tous les membres
//! écrivent ensemble en Markdown (titres, listes, tableaux, couleurs…), **en temps réel**, avec un
//! historique de versions.
//!
//! **Fusion sans conflit.** Chaque conversation a un document Yjs (bibliothèque `yrs`) : la liste
//! des canevas (`canevas`, titre et auteur en JSON), le texte de chacun (`texte/<id>`) et son
//! historique (`versions/<id>`). Deux membres qui tapent en même temps, même hors ligne pendant des
//! jours, retrouvent exactement le même texte une fois réunis : Yjs (un CRDT) fusionne les frappes
//! sans perte et dans le même ordre partout.
//!
//! **Synchronisation** (message `canvas-sync`, un aller-retour) : l'expéditeur envoie ce que le
//! destinataire n'a pas encore (d'après le dernier état connu de celui-ci) et son propre état ; le
//! destinataire applique, puis répond dans l'accusé (`Ack.data`) ce qui manque à l'expéditeur et son
//! état. Chaque frappe part tout de suite vers les membres joignables (temps réel) ; un échange
//! complet a lieu au moins toutes les 30 s avec chacun, et dès qu'un membre revient.
//!
//! **Historique intelligent.** Les modifications d'une personne sont regroupées en une version
//! quand elle fait une pause (45 s) ou au bout de 10 min : auteur, heure, caractères ajoutés et
//! retirés, résumé (début de la partie modifiée) et texte complet. Les versions sont partagées par
//! tous ; on peut en afficher une, la nommer ou la restaurer (l'état courant est d'abord gardé).
//!
//! Stockage : `canevas/<conversation>.yjs` (état complet, encodage Yjs v1).

use crate::{
    chat::{valid_name, SenderInfo},
    engine::{explain_refusal, lock, read_ack, Engine, Inner},
    protocol::Message,
    util::now_ms,
};
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};
use yrs::{
    updates::{decoder::Decode, encoder::Encode},
    Any, Array, ArrayRef, Doc, GetString, Map, MapRef, Out, ReadTxn, StateVector, Text, TextRef, Transact, Update,
};

const DIR: &str = "canevas";
/// Pause qui clôt une version.
const VERSION_IDLE: Duration = Duration::from_secs(45);
/// Durée maximale d'une version pendant une longue séance d'écriture.
const VERSION_MAX_SESSION: Duration = Duration::from_secs(600);
const MAX_VERSIONS: u32 = 150;
/// « X modifie… » reste affiché ce temps après la dernière frappe reçue.
const EDITING_SHOWN: Duration = Duration::from_secs(5);
/// Échange complet avec chaque membre au moins à ce rythme.
const FULL_SYNC_EVERY: Duration = Duration::from_secs(30);
const RETRY_AFTER: Duration = Duration::from_secs(10);
pub const MAX_CANVAS_BYTES: usize = 300_000;
pub const MAX_TITLE: usize = 80;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(text: &str) -> Result<Vec<u8>> {
    Ok(base64::engine::general_purpose::STANDARD.decode(text)?)
}

// ------------------------------------------------------------------ vues

#[derive(Debug, Clone, PartialEq)]
pub struct CanvasView {
    pub id: String,
    pub title: String,
    pub author: String,
    pub created_ms: u64,
    pub text: String,
    /// Membres en train de modifier ce canevas en ce moment.
    pub editing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CanvasVersionView {
    pub id: String,
    pub at_ms: u64,
    pub authors: Vec<String>,
    pub added: usize,
    pub removed: usize,
    /// Nom donné par un membre (vide sinon).
    pub label: String,
    /// Début de la partie modifiée.
    pub summary: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct CanvasMeta {
    title: String,
    author: String,
    created: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct VersionEntry {
    id: String,
    at: u64,
    authors: Vec<String>,
    added: usize,
    removed: usize,
    label: String,
    summary: String,
    text: String,
}

/// Réponse d'un membre à une synchronisation (dans `Ack.data`).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SyncReply {
    update: String,
    state: String,
}

// ------------------------------------------------------------------ document

/// Séance d'écriture en cours (pour fabriquer une version).
struct Session {
    started: Instant,
    last: Instant,
    added: usize,
    removed: usize,
    base: String,
}

struct Space {
    doc: Doc,
    dirty: bool,
    /// Dernier état connu de chaque membre (encodé).
    known: HashMap<String, Vec<u8>>,
    /// Notre état (encodé) au dernier échange réussi avec chaque membre.
    synced: HashMap<String, Vec<u8>>,
    last_full: HashMap<String, Instant>,
    sessions: HashMap<String, Session>,
    /// Canevas que nous venons de modifier (annoncé aux autres pour « X modifie… »).
    typing: Option<(String, Instant)>,
}

impl Space {
    fn new() -> Self {
        Self {
            doc: Doc::new(),
            dirty: false,
            known: HashMap::new(),
            synced: HashMap::new(),
            last_full: HashMap::new(),
            sessions: HashMap::new(),
            typing: None,
        }
    }

    fn list(&self) -> MapRef {
        self.doc.get_or_insert_map("canevas")
    }

    fn text(&self, id: &str) -> TextRef {
        self.doc.get_or_insert_text(format!("texte/{id}"))
    }

    fn versions(&self, id: &str) -> ArrayRef {
        self.doc.get_or_insert_array(format!("versions/{id}"))
    }

    fn state(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    fn metas(&self) -> Vec<(String, CanvasMeta)> {
        let list = self.list();
        let txn = self.doc.transact();
        let mut metas: Vec<(String, CanvasMeta)> = list
            .iter(&txn)
            .filter_map(|(id, value)| match value {
                Out::Any(Any::String(json)) => serde_json::from_str(&json).ok().map(|meta| (id.to_string(), meta)),
                _ => None,
            })
            .collect();
        metas.sort_by(|a, b| a.1.created.cmp(&b.1.created).then(a.0.cmp(&b.0)));
        metas
    }

    fn exists(&self, id: &str) -> bool {
        let list = self.list();
        let txn = self.doc.transact();
        list.get(&txn, id).is_some()
    }

    fn content(&self, id: &str) -> String {
        let text = self.text(id);
        let txn = self.doc.transact();
        text.get_string(&txn)
    }

    fn version_entries(&self, id: &str) -> Vec<VersionEntry> {
        let versions = self.versions(id);
        let txn = self.doc.transact();
        versions
            .iter(&txn)
            .filter_map(|value| match value {
                Out::Any(Any::String(json)) => serde_json::from_str(&json).ok(),
                _ => None,
            })
            .collect()
    }

    /// Remplace le texte par `new` en ne touchant que la partie modifiée (le reste garde ses
    /// identifiants Yjs : les frappes simultanées des autres s'y insèrent correctement).
    fn replace_text(&mut self, id: &str, new: &str) -> (usize, usize) {
        let old = self.content(id);
        let (prefix, old_mid, new_mid) = diff(&old, new);
        if old_mid.is_empty() && new_mid.is_empty() {
            return (0, 0);
        }
        let text = self.text(id);
        let mut txn = self.doc.transact_mut();
        if !old_mid.is_empty() {
            text.remove_range(&mut txn, prefix as u32, old_mid.len() as u32);
        }
        if !new_mid.is_empty() {
            text.insert(&mut txn, prefix as u32, new_mid);
        }
        drop(txn);
        self.dirty = true;
        (new_mid.chars().count(), old_mid.chars().count())
    }

    fn push_version(&mut self, id: &str, entry: VersionEntry) {
        let versions = self.versions(id);
        let json = serde_json::to_string(&entry).unwrap_or_default();
        let mut txn = self.doc.transact_mut();
        versions.push_back(&mut txn, json);
        // Trop de versions : on retire les plus anciennes sans nom.
        let mut length = versions.len(&txn);
        let mut index = 0;
        while length > MAX_VERSIONS && index < length {
            let named = matches!(versions.get(&txn, index), Some(Out::Any(Any::String(json)))
                if serde_json::from_str::<VersionEntry>(&json).is_ok_and(|v| !v.label.is_empty()));
            if named {
                index += 1;
            } else {
                versions.remove(&mut txn, index);
                length -= 1;
            }
        }
        drop(txn);
        self.dirty = true;
    }

    /// Clôt la séance d'écriture de ce canevas en une version (s'il y a eu des changements).
    fn close_session(&mut self, id: &str, author: &str) {
        let Some(session) = self.sessions.remove(id) else { return };
        let text = self.content(id);
        if text == session.base {
            return;
        }
        let summary = summarize(&session.base, &text);
        self.push_version(
            id,
            VersionEntry {
                id: uuid::Uuid::new_v4().to_string(),
                at: now_ms(),
                authors: vec![author.to_string()],
                added: session.added,
                removed: session.removed,
                label: String::new(),
                summary,
                text,
            },
        );
    }
}

/// Préfixe commun (en octets, sur une frontière de caractère), partie retirée, partie ajoutée.
fn diff<'a>(old: &'a str, new: &'a str) -> (usize, &'a str, &'a str) {
    let mut prefix = 0;
    for ((i, a), b) in old.char_indices().zip(new.chars()) {
        if a != b {
            break;
        }
        prefix = i + a.len_utf8();
    }
    let old_rest = &old[prefix..];
    let new_rest = &new[prefix..];
    let mut suffix = 0;
    for (a, b) in old_rest.chars().rev().zip(new_rest.chars().rev()) {
        if a != b || suffix + a.len_utf8() > old_rest.len().min(new_rest.len()) {
            break;
        }
        suffix += a.len_utf8();
    }
    (prefix, &old_rest[..old_rest.len() - suffix], &new_rest[..new_rest.len() - suffix])
}

/// Nouvelle position d'un curseur (octets) quand le texte passe de `old` à `new`.
pub fn transform_offset(old: &str, new: &str, offset: usize) -> usize {
    let (prefix, removed, added) = diff(old, new);
    let offset = offset.min(old.len());
    let result = if offset <= prefix {
        offset
    } else if offset >= prefix + removed.len() {
        offset - removed.len() + added.len()
    } else {
        prefix + added.len()
    };
    let mut result = result.min(new.len());
    while !new.is_char_boundary(result) {
        result -= 1;
    }
    result
}

/// Boutons de mise en forme de l'éditeur (identifiants communs au Mac et à Windows).
pub const FORMATS: [(&str, &str, &str); 16] = [
    ("bold", "G", "Gras"),
    ("italic", "I", "Italique"),
    ("underline", "S", "Souligné"),
    ("strike", "B", "Barré"),
    ("h1", "T1", "Titre"),
    ("h2", "T2", "Sous-titre"),
    ("h3", "T3", "Petit titre"),
    ("bullet", "•", "Liste à puces"),
    ("number", "1.", "Liste numérotée"),
    ("task", "☐", "Cases à cocher"),
    ("quote", "❝", "Citation"),
    ("code", "</>", "Code"),
    ("codeblock", "{ }", "Bloc de code"),
    ("table", "▦", "Tableau"),
    ("rule", "—", "Séparateur"),
    ("link", "🔗", "Lien"),
];

/// Couleurs du bouton « Couleur » (nom affiché, mot écrit dans le texte).
pub const COLORS: [(&str, &str); 8] = [
    ("Rouge", "rouge"),
    ("Orange", "orange"),
    ("Jaune", "jaune"),
    ("Vert", "vert"),
    ("Bleu", "bleu"),
    ("Violet", "violet"),
    ("Rose", "rose"),
    ("Gris", "gris"),
];

/// Applique un bouton de mise en forme à `text` avec la sélection `start..end` (octets).
/// Retourne le nouveau texte et la nouvelle sélection. `kind` : un identifiant de [`FORMATS`]
/// ou `color:<mot>`.
pub fn apply_format(text: &str, start: usize, end: usize, kind: &str) -> (String, usize, usize) {
    let fix = |mut i: usize| {
        i = i.min(text.len());
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let (start, end) = (fix(start.min(end)), fix(start.max(end)));
    let selected = &text[start..end];
    let wrap = |open: &str, close: &str, placeholder: &str| {
        let inner = if selected.is_empty() { placeholder } else { selected };
        let new = format!("{}{open}{inner}{close}{}", &text[..start], &text[end..]);
        let inner_start = start + open.len();
        (new, inner_start, inner_start + inner.len())
    };
    // Lignes entières touchées par la sélection.
    let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = text[end..].find('\n').map(|i| end + i).unwrap_or(text.len());
    let prefix_lines = |make: &dyn Fn(usize) -> String, strip: &dyn Fn(&str) -> Option<usize>| {
        let block = &text[line_start..line_end];
        let lines: Vec<&str> = block.split('\n').collect();
        let all_have = lines.iter().filter(|l| !l.trim().is_empty()).all(|l| strip(l).is_some());
        let mut number = 0;
        let changed: Vec<String> = lines
            .iter()
            .map(|line| {
                let bare = strip(line).map(|n| &line[n..]).unwrap_or(line);
                if all_have && !block.trim().is_empty() {
                    bare.to_string()
                } else if line.trim().is_empty() && lines.len() > 1 {
                    line.to_string()
                } else {
                    number += 1;
                    format!("{}{bare}", make(number))
                }
            })
            .collect();
        let replaced = changed.join("\n");
        let new = format!("{}{replaced}{}", &text[..line_start], &text[line_end..]);
        (new, line_start + replaced.len(), line_start + replaced.len())
    };
    // Titre : remplace un titre d'un autre niveau, retire un titre du même niveau.
    let heading = |level: usize| {
        let strip = |line: &str| -> (usize, usize) {
            let hashes = line.chars().take_while(|c| *c == '#').count();
            if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') { (hashes, hashes + 1) } else { (0, 0) }
        };
        let block = &text[line_start..line_end];
        let lines: Vec<&str> = block.split('\n').collect();
        let all_same = lines.iter().filter(|l| !l.trim().is_empty()).all(|l| strip(l).0 == level);
        let changed: Vec<String> = lines
            .iter()
            .map(|line| {
                let bare = &line[strip(line).1..];
                if all_same || (line.trim().is_empty() && lines.len() > 1) { bare.to_string() } else { format!("{} {bare}", "#".repeat(level)) }
            })
            .collect();
        let replaced = changed.join("\n");
        let new = format!("{}{replaced}{}", &text[..line_start], &text[line_end..]);
        (new, line_start + replaced.len(), line_start + replaced.len())
    };
    let simple = |mark: &'static str| prefix_lines(&move |_| mark.to_string(), &move |line: &str| line.starts_with(mark).then_some(mark.len()));
    let insert_block = |block: &str, select: Option<(usize, usize)>| {
        let before = if start == 0 || text[..start].ends_with("\n\n") { "" } else if text[..start].ends_with('\n') { "\n" } else { "\n\n" };
        let new = format!("{}{before}{block}\n{}", &text[..start], &text[end..]);
        let base = start + before.len();
        let (a, b) = select.unwrap_or((block.len(), block.len()));
        (new, base + a, base + b)
    };
    match kind {
        "bold" => wrap("**", "**", "texte en gras"),
        "italic" => wrap("*", "*", "texte en italique"),
        "underline" => wrap("__", "__", "texte souligné"),
        "strike" => wrap("~~", "~~", "texte barré"),
        "code" => wrap("`", "`", "code"),
        "h1" => heading(1),
        "h2" => heading(2),
        "h3" => heading(3),
        "bullet" => simple("- "),
        "task" => simple("- [ ] "),
        "quote" => simple("> "),
        "number" => prefix_lines(&|n| format!("{n}. "), &|line: &str| {
            let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
            (digits > 0 && line[digits..].starts_with(". ")).then_some(digits + 2)
        }),
        "codeblock" => {
            if selected.is_empty() {
                insert_block("```\ncode\n```", Some((4, 8)))
            } else {
                let new = format!("{}```\n{selected}\n```{}", &text[..start], &text[end..]);
                (new, start + 4, start + 4 + selected.len())
            }
        }
        "table" => insert_block("| Colonne 1 | Colonne 2 | Colonne 3 |\n|---|---|---|\n|  |  |  |\n|  |  |  |", Some((2, 11))),
        "rule" => insert_block("---", None),
        "link" => {
            let label = if selected.is_empty() { "texte du lien" } else { selected };
            let new = format!("{}[{label}](https://){}", &text[..start], &text[end..]);
            let url = start + label.len() + 3;
            (new, url, url + "https://".len())
        }
        color if color.starts_with("color:") => {
            let name = &color[6..];
            wrap(&format!("<font color=\"{name}\">"), "</font>", "texte en couleur")
        }
        _ => (text.to_string(), start, end),
    }
}

/// Début de la partie modifiée entre deux textes (pour nommer une version).
fn summarize(old: &str, new: &str) -> String {
    let (prefix, removed, added) = diff(old, new);
    let line_start = new[..prefix].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let region = if added.trim().is_empty() { &new[line_start..] } else { &new[line_start..prefix + added.len()] };
    let line = region.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let plain: String = crate::markdown::plain(line).chars().take(60).collect();
    if plain.is_empty() {
        if removed.trim().is_empty() { "mise en forme".to_string() } else { "suppression".to_string() }
    } else {
        plain
    }
}

pub(crate) struct CanvasStore {
    dir: PathBuf,
    spaces: HashMap<String, Space>,
    revision: u64,
    editing: HashMap<(String, String), HashMap<String, Instant>>,
    syncing: HashSet<(String, String)>,
    retry: HashMap<(String, String), Instant>,
}

impl CanvasStore {
    pub(crate) fn load(data_dir: &std::path::Path) -> Self {
        let dir = data_dir.join(DIR);
        let mut spaces = HashMap::new();
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(conversation) = path.file_stem().map(|s| s.to_string_lossy().to_string()) else { continue };
                if path.extension().is_none_or(|ext| ext != "yjs") || !valid_name(&conversation) {
                    continue;
                }
                let space = Space::new();
                if let Some(update) = fs::read(&path).ok().and_then(|bytes| Update::decode_v1(&bytes).ok()) {
                    let applied = space.doc.transact_mut().apply_update(update);
                    if applied.is_err() {
                        eprintln!("[canevas] {conversation} : fichier abîmé, ignoré");
                        continue;
                    }
                }
                spaces.insert(conversation, space);
            }
        }
        Self { dir, spaces, revision: 1, editing: HashMap::new(), syncing: HashSet::new(), retry: HashMap::new() }
    }

    fn space(&mut self, conversation: &str) -> &mut Space {
        self.spaces.entry(conversation.to_string()).or_insert_with(Space::new)
    }

    fn save_dirty(&mut self) {
        let _ = fs::create_dir_all(&self.dir);
        for (conversation, space) in self.spaces.iter_mut().filter(|(_, s)| s.dirty) {
            let bytes = space.doc.transact().encode_state_as_update_v1(&StateVector::default());
            let path = self.dir.join(format!("{conversation}.yjs"));
            let temporary = path.with_extension("tmp");
            if fs::write(&temporary, bytes).and_then(|_| fs::rename(&temporary, &path)).is_ok() {
                space.dirty = false;
            }
        }
    }

    fn bump(&mut self) {
        self.revision += 1;
    }
}

// ------------------------------------------------------------------ moteur

impl Inner {
    fn my_name(&self) -> String {
        let identity = self.identity.read().unwrap_or_else(|p| p.into_inner());
        if identity.pseudo.is_empty() { identity.name.clone() } else { identity.pseudo.clone() }
    }

    /// Membres (hors nous) d'une conversation où l'on peut écrire.
    fn canvas_members(&self, conversation: &str) -> Result<Vec<String>> {
        lock(&self.chat).writable_members(conversation).ok_or_else(|| anyhow!("conversation introuvable ou quittée"))
    }

    /// Synchronise le document de `conversation` avec `member` (un aller-retour).
    fn sync_canvas(&self, conversation: &str, member: &str) -> Result<()> {
        let target = self.chat_target(conversation, member).ok_or_else(|| anyhow!("hors ligne"))?;
        let (update, state, typing) = {
            let mut canvases = lock(&self.canvases);
            let space = canvases.space(conversation);
            let known = space.known.get(member).and_then(|sv| StateVector::decode_v1(sv).ok()).unwrap_or_default();
            let update = space.doc.transact().encode_diff_v1(&known);
            let typing = space.typing.as_ref().filter(|(_, at)| at.elapsed() < Duration::from_secs(3)).map(|(id, _)| id.clone());
            (update, space.state(), typing)
        };
        let mut channel = self.connect(&target)?;
        let message = Message::CanvasSync { conversation: conversation.to_string(), canvas: typing, update: b64(&update), state: b64(&state) };
        self.write_message(&mut channel, &message, &[])?;
        let _ = channel.finish();
        let ack = read_ack(&mut channel).ok_or_else(|| anyhow!("{} ne répond pas", target.name))?;
        if !ack.ok {
            bail!("{}", explain_refusal(&ack, &target.name));
        }
        let reply: SyncReply = serde_json::from_str(&ack.data).unwrap_or_default();
        let mut canvases = lock(&self.canvases);
        let space = canvases.space(conversation);
        // Modifié ici pendant l'aller-retour : ces changements ne sont pas encore partis.
        let local_unchanged = space.state() == state;
        let mut changed = false;
        if !reply.update.is_empty() {
            let update = Update::decode_v1(&unb64(&reply.update)?).context("mise à jour illisible")?;
            let before = space.state();
            space.doc.transact_mut().apply_update(update).map_err(|e| anyhow!("mise à jour refusée : {e}"))?;
            changed = space.state() != before;
            if changed {
                space.dirty = true;
            }
        }
        if let Ok(state) = unb64(&reply.state) {
            space.known.insert(member.to_string(), state);
        }
        if local_unchanged {
            let ours = space.state();
            space.synced.insert(member.to_string(), ours);
        } else {
            // Sinon on les croirait livrés et ils n'arriveraient qu'à la synchronisation complète
            // suivante (30 s plus tard).
            space.synced.remove(member);
        }
        space.last_full.insert(member.to_string(), Instant::now());
        if changed {
            canvases.bump();
        }
        drop(canvases);
        if changed {
            self.touch();
        }
        Ok(())
    }

    /// Synchronisation reçue d'un membre : on applique, puis on répond ce qui lui manque.
    pub(crate) fn receive_canvas_sync(
        &self,
        sender: &SenderInfo,
        conversation: &str,
        canvas: Option<String>,
        update: &str,
        state: &str,
    ) -> Result<String> {
        if !valid_name(conversation) || !lock(&self.chat).is_member(conversation, sender) {
            return Err(crate::engine::refusal("not-member", "vous n'êtes pas membre de cette conversation"));
        }
        if lock(&self.chat).writable_members(conversation).is_none() {
            return Err(crate::engine::refusal("not-member", "conversation quittée"));
        }
        let update = unb64(update)?;
        let their_state = unb64(state)?;
        let mut canvases = lock(&self.canvases);
        let space = canvases.space(conversation);
        let mut changed = false;
        if !update.is_empty() {
            let before = space.state();
            let decoded = Update::decode_v1(&update).context("mise à jour illisible")?;
            space.doc.transact_mut().apply_update(decoded).map_err(|e| anyhow!("mise à jour refusée : {e}"))?;
            changed = space.state() != before;
            if changed {
                space.dirty = true;
            }
        }
        let theirs = StateVector::decode_v1(&their_state).unwrap_or_default();
        let missing = space.doc.transact().encode_diff_v1(&theirs);
        let ours = space.state();
        space.known.insert(sender.id.clone(), their_state);
        space.last_full.insert(sender.id.clone(), Instant::now());
        if let Some(canvas) = canvas.filter(|id| valid_name(id) && changed) {
            let name = self.display_name(&sender.id).unwrap_or_else(|| sender.name.clone());
            canvases.editing.entry((conversation.to_string(), canvas)).or_default().insert(name, Instant::now());
            changed = true;
        }
        if changed {
            canvases.bump();
        }
        drop(canvases);
        if changed {
            self.touch();
        }
        Ok(serde_json::to_string(&SyncReply { update: b64(&missing), state: b64(&ours) })?)
    }
}

pub(crate) fn spawn_canvas_scheduler(inner: Arc<Inner>) {
    thread::spawn(move || {
        let mut last_save = Instant::now();
        while !inner.shutdown.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(150));
            let name = inner.my_name();
            let mut work = Vec::new();
            {
                let mut canvases = lock(&inner.canvases);
                // Versions : séances terminées.
                let mut changed = false;
                for space in canvases.spaces.values_mut() {
                    let done: Vec<String> = space
                        .sessions
                        .iter()
                        .filter(|(_, s)| s.last.elapsed() >= VERSION_IDLE || s.started.elapsed() >= VERSION_MAX_SESSION)
                        .map(|(id, _)| id.clone())
                        .collect();
                    for id in done {
                        space.close_session(&id, &name);
                        changed = true;
                    }
                }
                // « X modifie… » expiré.
                let before = canvases.editing.values().map(|m| m.len()).sum::<usize>();
                canvases.editing.retain(|_, who| {
                    who.retain(|_, at| at.elapsed() < EDITING_SHOWN);
                    !who.is_empty()
                });
                if before != canvases.editing.values().map(|m| m.len()).sum::<usize>() {
                    changed = true;
                }
                if changed {
                    canvases.bump();
                }
                if last_save.elapsed() > Duration::from_secs(2) {
                    canvases.save_dirty();
                    last_save = Instant::now();
                }
                let conversations: Vec<String> = canvases.spaces.keys().cloned().collect();
                for conversation in conversations {
                    let Ok(members) = inner.canvas_members(&conversation) else { continue };
                    let space = canvases.space(&conversation);
                    let ours = space.state();
                    for member in members {
                        let behind = space.synced.get(&member) != Some(&ours);
                        let stale = space.last_full.get(&member).is_none_or(|at| at.elapsed() > FULL_SYNC_EVERY);
                        if behind || stale {
                            work.push((conversation.clone(), member));
                        }
                    }
                }
                let now = Instant::now();
                work.retain(|key| !canvases.syncing.contains(key) && canvases.retry.get(key).is_none_or(|at| *at <= now));
            }
            for (conversation, member) in work {
                if inner.chat_target(&conversation, &member).is_none() {
                    continue;
                }
                lock(&inner.canvases).syncing.insert((conversation.clone(), member.clone()));
                let worker = inner.clone();
                thread::spawn(move || {
                    let result = worker.sync_canvas(&conversation, &member);
                    let mut canvases = lock(&worker.canvases);
                    let key = (conversation, member);
                    canvases.syncing.remove(&key);
                    match result {
                        Ok(()) => {
                            canvases.retry.remove(&key);
                        }
                        Err(_) => {
                            canvases.retry.insert(key, Instant::now() + RETRY_AFTER);
                        }
                    }
                });
            }
        }
        lock(&inner.canvases).save_dirty();
    });
}

impl Engine {
    /// Change à chaque modification d'un canevas (le nôtre ou reçu).
    pub fn canvas_revision(&self) -> u64 {
        lock(&self.inner.canvases).revision
    }

    pub fn canvases(&self, conversation: &str) -> Vec<CanvasView> {
        let mut canvases = lock(&self.inner.canvases);
        let Some(space) = canvases.spaces.get(conversation) else { return Vec::new() };
        let list: Vec<CanvasView> = space
            .metas()
            .into_iter()
            .map(|(id, meta)| CanvasView {
                text: space.content(&id),
                editing: Vec::new(),
                id,
                title: meta.title,
                author: meta.author,
                created_ms: meta.created,
            })
            .collect();
        let editing = &mut canvases.editing;
        list.into_iter()
            .map(|mut view| {
                if let Some(who) = editing.get(&(conversation.to_string(), view.id.clone())) {
                    view.editing = who.keys().cloned().collect();
                    view.editing.sort();
                }
                view
            })
            .collect()
    }

    fn change_canvas<T>(&self, conversation: &str, change: impl FnOnce(&mut Space, &str) -> Result<T>) -> Result<T> {
        self.inner.canvas_members(conversation)?;
        let name = self.inner.my_name();
        let mut canvases = lock(&self.inner.canvases);
        let result = change(canvases.space(conversation), &name)?;
        canvases.bump();
        drop(canvases);
        self.inner.touch();
        Ok(result)
    }

    /// Nouveau canevas ; son identifiant.
    pub fn create_canvas(&self, conversation: &str, title: &str) -> Result<String> {
        let title: String = title.trim().chars().take(MAX_TITLE).collect();
        let title = if title.is_empty() { "Sans titre".to_string() } else { title };
        self.change_canvas(conversation, |space, name| {
            let id = uuid::Uuid::new_v4().to_string();
            let meta = CanvasMeta { title, author: name.to_string(), created: now_ms() };
            let list = space.list();
            let mut txn = space.doc.transact_mut();
            list.insert(&mut txn, id.as_str(), serde_json::to_string(&meta)?);
            drop(txn);
            space.dirty = true;
            Ok(id)
        })
    }

    pub fn rename_canvas(&self, conversation: &str, id: &str, title: &str) -> Result<()> {
        let title: String = title.trim().chars().take(MAX_TITLE).collect();
        if title.is_empty() {
            bail!("donnez un titre au canevas");
        }
        self.change_canvas(conversation, |space, _| {
            let mut meta = space.metas().into_iter().find(|(key, _)| key == id).map(|(_, m)| m).ok_or_else(|| anyhow!("canevas introuvable"))?;
            meta.title = title;
            let list = space.list();
            let mut txn = space.doc.transact_mut();
            list.insert(&mut txn, id, serde_json::to_string(&meta)?);
            drop(txn);
            space.dirty = true;
            Ok(())
        })
    }

    pub fn delete_canvas(&self, conversation: &str, id: &str) -> Result<()> {
        self.change_canvas(conversation, |space, _| {
            let list = space.list();
            let text = space.text(id);
            let versions = space.versions(id);
            let mut txn = space.doc.transact_mut();
            list.remove(&mut txn, id);
            let length = text.len(&txn);
            text.remove_range(&mut txn, 0, length);
            let count = versions.len(&txn);
            versions.remove_range(&mut txn, 0, count);
            drop(txn);
            space.sessions.remove(id);
            space.dirty = true;
            Ok(())
        })
    }

    /// Nouveau texte d'un canevas (Markdown), tapé par l'utilisateur : seule la différence part.
    pub fn set_canvas_text(&self, conversation: &str, id: &str, text: &str) -> Result<()> {
        if text.len() > MAX_CANVAS_BYTES {
            bail!("canevas trop long ({} Ko au plus)", MAX_CANVAS_BYTES / 1000);
        }
        self.change_canvas(conversation, |space, _| {
            if !space.exists(id) {
                bail!("canevas introuvable");
            }
            let base = space.content(id);
            let (added, removed) = space.replace_text(id, text);
            if added + removed > 0 {
                let now = Instant::now();
                let session = space.sessions.entry(id.to_string()).or_insert(Session { started: now, last: now, added: 0, removed: 0, base });
                session.last = now;
                session.added += added;
                session.removed += removed;
                space.typing = Some((id.to_string(), now));
            }
            Ok(())
        })
    }

    /// Historique d'un canevas, du plus récent au plus ancien.
    pub fn canvas_versions(&self, conversation: &str, id: &str) -> Vec<CanvasVersionView> {
        let canvases = lock(&self.inner.canvases);
        let Some(space) = canvases.spaces.get(conversation) else { return Vec::new() };
        let mut versions: Vec<CanvasVersionView> = space
            .version_entries(id)
            .into_iter()
            .map(|v| CanvasVersionView {
                id: v.id,
                at_ms: v.at,
                authors: v.authors,
                added: v.added,
                removed: v.removed,
                label: v.label,
                summary: v.summary,
                text: v.text,
            })
            .collect();
        versions.sort_by_key(|v| std::cmp::Reverse(v.at_ms));
        versions
    }

    /// Revient à une version (l'état courant est d'abord gardé comme version).
    pub fn restore_canvas_version(&self, conversation: &str, id: &str, version: &str) -> Result<()> {
        self.change_canvas(conversation, |space, name| {
            let target = space.version_entries(id).into_iter().find(|v| v.id == version).ok_or_else(|| anyhow!("version introuvable"))?;
            space.close_session(id, name);
            let before = space.content(id);
            let (added, removed) = space.replace_text(id, &target.text);
            if added + removed > 0 {
                let when = crate::util::now_ms();
                let label = if target.label.is_empty() { String::new() } else { format!(" « {} »", target.label) };
                space.push_version(
                    id,
                    VersionEntry {
                        id: uuid::Uuid::new_v4().to_string(),
                        at: when,
                        authors: vec![name.to_string()],
                        added,
                        removed,
                        label: String::new(),
                        summary: format!("Restauration de la version{label}"),
                        text: target.text.clone(),
                    },
                );
                let _ = before;
            }
            Ok(())
        })
    }

    /// Donne un nom à une version (une version nommée n'est jamais supprimée).
    pub fn name_canvas_version(&self, conversation: &str, id: &str, version: &str, label: &str) -> Result<()> {
        let label: String = label.trim().chars().take(60).collect();
        self.change_canvas(conversation, |space, _| {
            let versions = space.versions(id);
            let mut txn = space.doc.transact_mut();
            let length = versions.len(&txn);
            for index in 0..length {
                if let Some(Out::Any(Any::String(json))) = versions.get(&txn, index) {
                    if let Ok(mut entry) = serde_json::from_str::<VersionEntry>(&json) {
                        if entry.id == version {
                            entry.label = label.clone();
                            versions.remove(&mut txn, index);
                            versions.insert(&mut txn, index, serde_json::to_string(&entry)?);
                            drop(txn);
                            space.dirty = true;
                            return Ok(());
                        }
                    }
                }
            }
            bail!("version introuvable")
        })
    }

    /// Clôt tout de suite la séance d'écriture (crée la version sans attendre la pause).
    pub fn save_canvas_version(&self, conversation: &str, id: &str) -> Result<()> {
        self.change_canvas(conversation, |space, name| {
            space.close_session(id, name);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_and_cursor() {
        assert_eq!(diff("bonjour", "bonsoir"), (3, "jou", "soi"));
        assert_eq!(diff("aaa", "aaaa"), (3, "", "a"));
        assert_eq!(diff("é😀x", "é😀yx"), ("é😀".len(), "", "y"));
        // Un texte inséré avant le curseur le décale ; après, il ne bouge pas.
        assert_eq!(transform_offset("abc def", "XXabc def", 4), 6);
        assert_eq!(transform_offset("abc def", "abc defYY", 4), 4);
        assert_eq!(transform_offset("abc def", "abc", 6), 3);
    }

    #[test]
    fn concurrent_edits_merge_without_conflict() {
        let mut a = Space::new();
        let mut b = Space::new();
        {
            let list = a.list();
            let mut txn = a.doc.transact_mut();
            list.insert(&mut txn, "c1", "{}");
        }
        a.replace_text("c1", "Liste :\n- pain\n");
        // B reçoit l'état de A, puis chacun tape de son côté, hors ligne.
        let full = a.doc.transact().encode_diff_v1(&StateVector::default());
        b.doc.transact_mut().apply_update(Update::decode_v1(&full).unwrap()).unwrap();
        a.replace_text("c1", "Liste :\n- pain\n- lait\n");
        b.replace_text("c1", "Liste des courses :\n- pain\n");
        // Échange dans les deux sens.
        let to_b = a.doc.transact().encode_diff_v1(&StateVector::decode_v1(&b.state()).unwrap());
        let to_a = b.doc.transact().encode_diff_v1(&StateVector::decode_v1(&a.state()).unwrap());
        b.doc.transact_mut().apply_update(Update::decode_v1(&to_b).unwrap()).unwrap();
        a.doc.transact_mut().apply_update(Update::decode_v1(&to_a).unwrap()).unwrap();
        assert_eq!(a.content("c1"), b.content("c1"));
        assert_eq!(a.content("c1"), "Liste des courses :\n- pain\n- lait\n");
    }

    #[test]
    fn formatting_buttons() {
        assert_eq!(apply_format("un mot ici", 3, 6, "bold"), ("un **mot** ici".to_string(), 5, 8));
        assert_eq!(apply_format("", 0, 0, "italic").0, "*texte en italique*");
        assert_eq!(apply_format("a\nb\nc", 0, 3, "bullet").0, "- a\n- b\nc");
        assert_eq!(apply_format("- a\n- b", 0, 7, "bullet").0, "a\nb");
        assert_eq!(apply_format("x\ny", 0, 3, "number").0, "1. x\n2. y");
        assert_eq!(apply_format("## Titre", 3, 3, "h1").0, "# Titre");
        assert_eq!(apply_format("# Titre", 2, 2, "h1").0, "Titre");
        assert!(apply_format("avant", 5, 5, "table").0.starts_with("avant\n\n| Colonne 1"));
        assert_eq!(apply_format("ok", 0, 2, "color:rouge").0, "<font color=\"rouge\">ok</font>");
        let (text, a, b) = apply_format("voir", 0, 4, "link");
        assert_eq!(&text[a..b], "https://");
    }

    #[test]
    fn versions_are_summarized() {
        assert_eq!(summarize("# Plan\n\nA\n", "# Plan\n\nA\n- **acheter** du pain\n"), "• acheter du pain");
        assert_eq!(summarize("abc\ndef", "abc\n"), "suppression");
    }
}
