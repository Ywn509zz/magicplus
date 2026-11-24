//! Transferts de fichiers reprenables.
//!
//! **Envoi** : chaque couple fichier × destinataire devient une tâche (`OutgoingJob`) enregistrée
//! dans `envois.json`. Un fil « planificateur » lance les tâches dont le destinataire est joignable
//! et les relance après une coupure (délai doublé à chaque échec, 60 s au plus), jusqu'à
//! `Settings::resume_hours` après la dernière activité (48 h par défaut). Le fichier source est lu
//! morceau par morceau (256 Ko) : rien n'est chargé entièrement en mémoire.
//!
//! **Réception** : les morceaux sont écrits dans `partiels/<id>.part`, dans le dossier de données
//! (caché), avec `partiels/<id>.json` pour pouvoir reprendre. À la reprise, le récepteur répond à
//! l'offre par le nombre d'octets déjà reçus et l'expéditeur repart de là. Une fois complet et
//! vérifié (SHA-256), le fichier est déplacé dans le dossier de réception (Téléchargements par défaut).

use crate::{
    crypto,
    engine::{explain_refusal, lock, read_ack, refusal, Engine, Inner, ReceivedView, TransferState, TransferView},
    engine::{MAX_RECEIVED_SHOWN, MAX_TRANSFERS_SHOWN},
    protocol::*,
    store::TrustedPeer,
    util::*,
};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant, UNIX_EPOCH},
};

const OUTGOING_FILE: &str = "envois.json";
const PARTIALS_DIR: &str = "partiels";
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

/// Envoi à faire ou à reprendre.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OutgoingJob {
    pub id: String,
    pub path: PathBuf,
    pub file_name: String,
    pub size: u64,
    /// Date de modification du fichier source (ms) : s'il change, l'envoi est abandonné.
    pub modified_ms: u64,
    /// SHA-256 (base64) du fichier ; vide tant qu'il n'est pas calculé.
    pub digest: String,
    pub peer_id: String,
    pub peer_name: String,
    /// Dernière activité (création ou progression), en ms depuis 1970.
    pub updated_ms: u64,
    /// Octets envoyés au dernier essai (affichage).
    pub sent: u64,
    /// Pièce jointe d'une discussion (destinataire : membre de la conversation).
    #[serde(default)]
    pub chat: Option<ChatLink>,
}

/// Réception interrompue, enregistrée à côté du fichier partiel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct IncomingMeta {
    pub id: String,
    pub peer_id: String,
    pub peer_name: String,
    pub file_name: String,
    pub size: u64,
    pub digest: String,
    pub updated_ms: u64,
    #[serde(default)]
    pub chat: Option<ChatLink>,
}

/// Réception en cours (connexion ouverte).
pub(crate) struct Incoming {
    meta: IncomingMeta,
    file: File,
    hasher: Sha256,
    received: u64,
    next_sequence: u64,
    last_percent: u8,
    last_saved: Instant,
}

impl Incoming {
    pub(crate) fn peer_id(&self) -> &str {
        &self.meta.peer_id
    }
}

pub(crate) struct Retry {
    next: Instant,
    failures: u32,
}

enum JobError {
    /// Interruption temporaire : on retentera.
    Pause(String),
    /// Abandon définitif.
    Fail(String),
}

fn pause(reason: impl Into<String>) -> JobError {
    JobError::Pause(reason.into())
}

fn fail(reason: impl Into<String>) -> JobError {
    JobError::Fail(reason.into())
}

/// Identifiant utilisable dans un nom de fichier (il vient de l'autre appareil).
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn modified_ms(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn percent(done: u64, total: u64) -> u8 {
    (done.saturating_mul(100)).checked_div(total).unwrap_or(100).min(100) as u8
}

fn ratio(done: u64, total: u64) -> f32 {
    if total == 0 {
        1.0
    } else {
        (done as f64 / total as f64).min(1.0) as f32
    }
}

/// Lit jusqu'à remplir le tampon (ou la fin du fichier).
fn read_full(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = reader.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

/// Déplace un fichier, y compris vers un autre disque (copie puis suppression).
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to)?;
    fs::remove_file(from)
}

// ------------------------------------------------------------------ planificateur

/// Lance et relance les envois en attente ; nettoie ce qui a dépassé le délai de reprise.
pub(crate) fn spawn_scheduler(inner: Arc<Inner>) {
    thread::spawn(move || {
        let mut last_cleanup: Option<Instant> = None;
        while !inner.shutdown.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(250));
            inner.expire_outgoing();
            if last_cleanup.is_none_or(|at| at.elapsed() > Duration::from_secs(30)) {
                inner.expire_partials();
                last_cleanup = Some(Instant::now());
            }

            let jobs: Vec<OutgoingJob> = lock(&inner.outgoing).clone();
            for job in jobs {
                if lock(&inner.active_sends).contains(&job.id) {
                    continue;
                }
                if lock(&inner.retries).get(&job.id).is_some_and(|retry| retry.next > Instant::now()) {
                    continue;
                }
                if inner.job_target(&job).is_none() {
                    inner.mark_waiting(&job);
                    continue;
                }
                lock(&inner.active_sends).insert(job.id.clone());
                let worker = inner.clone();
                thread::spawn(move || worker.run_job(job.id));
            }
        }
    });
}

impl Engine {
    /// Ajoute des fichiers à envoyer à tous les appareils associés joignables. Les envois
    /// interrompus reprennent tout seuls (voir `spawn_scheduler`).
    pub fn send_files(&self, paths: Vec<PathBuf>) {
        let inner = &self.inner;
        let targets = inner.online_targets();
        if targets.is_empty() {
            inner.set_status("Aucun appareil associé n'est joignable");
            return;
        }
        let mut count = 0;
        for path in paths {
            let meta = match fs::metadata(&path) {
                Ok(meta) if meta.is_file() => meta,
                Ok(_) => {
                    inner.set_status(format!("« {} » est un dossier : choisissez des fichiers", path.display()));
                    continue;
                }
                Err(error) => {
                    inner.set_status(format!("« {} » est illisible ({error})", path.display()));
                    continue;
                }
            };
            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "fichier".to_string());
            for target in &targets {
                let job = OutgoingJob {
                    id: uuid::Uuid::new_v4().to_string(),
                    path: path.clone(),
                    file_name: file_name.clone(),
                    size: meta.len(),
                    modified_ms: modified_ms(&meta),
                    digest: String::new(),
                    peer_id: target.id.clone(),
                    peer_name: target.name.clone(),
                    updated_ms: now_ms(),
                    sent: 0,
                    chat: None,
                };
                inner.upsert_transfer(inner.view_for(&job, TransferState::Active, 0.0));
                lock(&inner.outgoing).push(job);
                count += 1;
            }
        }
        inner.save_outgoing();
        if count > 0 {
            inner.set_status(format!("{count} envoi(s) en cours"));
        }
    }

    /// Annule un envoi (en cours ou en attente) ou une réception interrompue.
    pub fn cancel_transfer(&self, id: &str) {
        let inner = &self.inner;
        let job = {
            let mut outgoing = lock(&inner.outgoing);
            let position = outgoing.iter().position(|job| job.id == id);
            position.map(|position| outgoing.remove(position))
        };
        if let Some(job) = job {
            inner.save_outgoing();
            lock(&inner.retries).remove(id);
            if lock(&inner.active_sends).contains(id) {
                // Le fil d'envoi s'arrête au prochain morceau et prévient le destinataire.
                lock(&inner.cancelled).insert(id.to_string());
            } else if let Some(target) = inner.job_target(&job) {
                // Prévient le destinataire pour qu'il supprime le fichier partiel.
                let worker = inner.clone();
                let id = id.to_string();
                thread::spawn(move || {
                    if let Ok(mut stream) = worker.connect(&target) {
                        let _ = worker.write_message(&mut stream, &Message::FileCancel { id }, &[]);
                        let _ = stream.finish();
                    }
                });
            }
            inner.upsert_transfer(inner.view_for(&job, TransferState::Failed("annulé".into()), ratio(job.sent, job.size)));
            inner.set_status(format!("Envoi de « {} » annulé", job.file_name));
            return;
        }

        // Réception : refusée désormais, fichier partiel supprimé.
        lock(&inner.cancelled).insert(id.to_string());
        if !lock(&inner.incoming).contains_key(id) {
            inner.discard_incoming(id, "annulé");
        }
        inner.set_status("Réception annulée");
    }
}

impl Inner {
    // ---------------------------------------------------------------- état et persistance

    pub(crate) fn upsert_transfer(&self, view: TransferView) {
        let mut transfers = lock(&self.transfers);
        match transfers.iter_mut().find(|item| item.id == view.id) {
            Some(existing) => *existing = view,
            None => {
                transfers.insert(0, view);
                transfers.truncate(MAX_TRANSFERS_SHOWN);
            }
        }
        drop(transfers);
        self.touch();
    }

    fn view_for(&self, job: &OutgoingJob, state: TransferState, progress: f32) -> TransferView {
        TransferView {
            id: job.id.clone(),
            file_name: job.file_name.clone(),
            size: job.size,
            peer_name: job.peer_name.clone(),
            sending: true,
            progress,
            state,
            attachment: job.chat.as_ref().map(|link| link.attachment.clone()),
        }
    }

    fn incoming_view(meta: &IncomingMeta, state: TransferState, received: u64) -> TransferView {
        TransferView {
            id: meta.id.clone(),
            file_name: meta.file_name.clone(),
            size: meta.size,
            peer_name: meta.peer_name.clone(),
            sending: false,
            progress: ratio(received, meta.size),
            state,
            attachment: meta.chat.as_ref().map(|link| link.attachment.clone()),
        }
    }

    fn save_outgoing(&self) {
        let jobs = lock(&self.outgoing).clone();
        if let Err(error) = self.store.save(OUTGOING_FILE, &jobs) {
            eprintln!("[transfert] enregistrement des envois impossible : {error}");
        }
    }

    fn update_job(&self, job: &OutgoingJob) {
        let found = {
            let mut outgoing = lock(&self.outgoing);
            match outgoing.iter_mut().find(|item| item.id == job.id) {
                Some(existing) => {
                    *existing = job.clone();
                    true
                }
                None => false,
            }
        };
        if found {
            self.save_outgoing();
        }
    }

    fn remove_job(&self, id: &str) {
        lock(&self.outgoing).retain(|job| job.id != id);
        self.save_outgoing();
    }

    /// Destinataire joignable d'une tâche : appareil associé, ou membre de la conversation.
    pub(crate) fn job_target(&self, job: &OutgoingJob) -> Option<crate::engine::Target> {
        match &job.chat {
            Some(link) => self.chat_target(&link.conversation, &job.peer_id),
            None => self.target_for(&job.peer_id),
        }
    }

    /// Ajoute l'envoi d'une pièce jointe de discussion à un membre.
    pub(crate) fn queue_chat_file(&self, path: &Path, file_name: &str, size: u64, peer_id: &str, peer_name: &str, link: ChatLink) {
        let modified = fs::metadata(path).map(|meta| modified_ms(&meta)).unwrap_or(0);
        let job = OutgoingJob {
            id: uuid::Uuid::new_v4().to_string(),
            path: path.to_path_buf(),
            file_name: file_name.to_string(),
            size,
            modified_ms: modified,
            digest: String::new(),
            peer_id: peer_id.to_string(),
            peer_name: peer_name.to_string(),
            updated_ms: now_ms(),
            sent: 0,
            chat: Some(link),
        };
        self.upsert_transfer(self.view_for(&job, TransferState::Active, 0.0));
        lock(&self.outgoing).push(job);
        self.save_outgoing();
    }

    fn resume_window_ms(&self) -> u64 {
        lock(&self.settings).resume_hours.max(1) * 3600 * 1000
    }

    fn partials_dir(&self) -> PathBuf {
        self.store.dir().join(PARTIALS_DIR)
    }

    /// Dossier où ranger les fichiers reçus : réglage, sinon Téléchargements.
    pub(crate) fn receive_folder(&self) -> PathBuf {
        lock(&self.settings)
            .save_folder
            .clone()
            .filter(|folder| !folder.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::download_dir)
            .unwrap_or_else(|| self.store.dir().join("Reçus"))
    }

    /// Au démarrage : réaffiche les envois et réceptions interrompus.
    pub(crate) fn restore_transfers(&self) {
        let jobs: Vec<OutgoingJob> = self.store.load(OUTGOING_FILE).unwrap_or_default();
        for job in &jobs {
            let reason = format!("en attente de {} — reprise automatique", job.peer_name);
            self.upsert_transfer(self.view_for(job, TransferState::Paused(reason), ratio(job.sent, job.size)));
        }
        *lock(&self.outgoing) = jobs;

        for meta in self.read_partials() {
            let received = fs::metadata(self.partials_dir().join(format!("{}.part", meta.id)))
                .map(|m| m.len())
                .unwrap_or(0);
            let reason = format!("interrompu — reprise automatique quand {} revient", meta.peer_name);
            self.upsert_transfer(Self::incoming_view(&meta, TransferState::Paused(reason), received));
        }
    }

    fn read_partials(&self) -> Vec<IncomingMeta> {
        let Ok(entries) = fs::read_dir(self.partials_dir()) else { return Vec::new() };
        entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| fs::read(entry.path()).ok())
            .filter_map(|raw| serde_json::from_slice::<IncomingMeta>(&raw).ok())
            .filter(|meta| valid_id(&meta.id))
            .collect()
    }

    fn mark_waiting(&self, job: &OutgoingJob) {
        let reason = format!("en attente de {} — reprise automatique", job.peer_name);
        let current = lock(&self.transfers).iter().find(|view| view.id == job.id).map(|view| view.state.clone());
        if !matches!(current, Some(TransferState::Paused(_))) {
            self.upsert_transfer(self.view_for(job, TransferState::Paused(reason), ratio(job.sent, job.size)));
        }
    }

    fn expire_outgoing(&self) {
        let window = self.resume_window_ms();
        let now = now_ms();
        let expired: Vec<OutgoingJob> = {
            let active = lock(&self.active_sends).clone();
            let mut outgoing = lock(&self.outgoing);
            let (gone, kept): (Vec<_>, Vec<_>) = outgoing
                .drain(..)
                .partition(|job| !active.contains(&job.id) && now.saturating_sub(job.updated_ms) > window);
            *outgoing = kept;
            gone
        };
        if expired.is_empty() {
            return;
        }
        self.save_outgoing();
        for job in expired {
            lock(&self.retries).remove(&job.id);
            let reason = format!("délai de reprise dépassé ({} h)", window / 3_600_000);
            self.upsert_transfer(self.view_for(&job, TransferState::Failed(reason), ratio(job.sent, job.size)));
        }
    }

    fn expire_partials(&self) {
        let window = self.resume_window_ms();
        let now = now_ms();
        let active: Vec<String> = lock(&self.incoming).keys().cloned().collect();
        for meta in self.read_partials() {
            if !active.contains(&meta.id) && now.saturating_sub(meta.updated_ms) > window {
                let reason = format!("délai de reprise dépassé ({} h)", window / 3_600_000);
                self.discard_incoming(&meta.id, &reason);
            }
        }
    }

    // ---------------------------------------------------------------- envoi

    fn run_job(&self, id: String) {
        let job = lock(&self.outgoing).iter().find(|job| job.id == id).cloned();
        let Some(mut job) = job else {
            lock(&self.active_sends).remove(&id);
            return;
        };
        let result = self.attempt(&mut job);
        lock(&self.active_sends).remove(&id);
        let cancelled = lock(&self.cancelled).remove(&id);
        let still_queued = lock(&self.outgoing).iter().any(|item| item.id == id);

        match result {
            Ok(()) => {
                self.remove_job(&id);
                lock(&self.retries).remove(&id);
                self.upsert_transfer(self.view_for(&job, TransferState::Complete, 1.0));
                self.set_status(format!("« {} » envoyé à {}", job.file_name, job.peer_name));
            }
            Err(JobError::Fail(reason)) => {
                self.remove_job(&id);
                lock(&self.retries).remove(&id);
                self.upsert_transfer(self.view_for(&job, TransferState::Failed(reason.clone()), ratio(job.sent, job.size)));
                self.set_status(format!("Envoi de « {} » abandonné : {reason}", job.file_name));
            }
            Err(JobError::Pause(_)) if cancelled || !still_queued => {
                self.upsert_transfer(self.view_for(&job, TransferState::Failed("annulé".into()), ratio(job.sent, job.size)));
            }
            Err(JobError::Pause(reason)) => {
                self.update_job(&job);
                let delay = {
                    let mut retries = lock(&self.retries);
                    let retry = retries.entry(id.clone()).or_insert(Retry { next: Instant::now(), failures: 0 });
                    retry.failures = retry.failures.saturating_add(1);
                    let delay = self
                        .cfg
                        .retry_delay
                        .saturating_mul(1 << (retry.failures - 1).min(4))
                        .min(MAX_RETRY_DELAY);
                    retry.next = Instant::now() + delay;
                    delay
                };
                eprintln!("[transfert] « {} » en pause ({reason}), nouvel essai dans {delay:?}", job.file_name);
                self.upsert_transfer(self.view_for(&job, TransferState::Paused(reason), ratio(job.sent, job.size)));
            }
        }
    }

    fn is_cancelled(&self, id: &str) -> bool {
        lock(&self.cancelled).contains(id) || !lock(&self.outgoing).iter().any(|job| job.id == id)
    }

    fn attempt(&self, job: &mut OutgoingJob) -> Result<(), JobError> {
        let meta = fs::metadata(&job.path).map_err(|_| fail("fichier source introuvable"))?;
        if meta.len() != job.size || modified_ms(&meta) != job.modified_ms {
            return Err(fail("le fichier a été modifié depuis le début de l'envoi"));
        }

        // Empreinte du fichier entier, calculée une seule fois (le récepteur vérifie l'intégrité).
        if job.digest.is_empty() {
            self.upsert_transfer(self.view_for(job, TransferState::Active, 0.0));
            let mut file = File::open(&job.path).map_err(|e| fail(format!("lecture impossible ({e})")))?;
            let mut hasher = Sha256::new();
            let mut buffer = vec![0_u8; CHUNK_SIZE];
            loop {
                if self.is_cancelled(&job.id) {
                    return Err(fail("annulé"));
                }
                let read = file.read(&mut buffer).map_err(|e| fail(format!("lecture impossible ({e})")))?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            job.digest = crypto::encode_key(&hasher.finalize());
            self.update_job(job);
        }

        let target = self
            .job_target(job)
            .ok_or_else(|| pause(format!("en attente de {} — reprise automatique", job.peer_name)))?;
        // Canal chiffré Noise : offre, morceaux et réponses passent tous dedans.
        let mut stream = self.connect(&target).map_err(|e| pause(e.to_string()))?;
        stream.get_ref().set_read_timeout(Some(Duration::from_secs(60))).ok();

        let offer = Message::FileOffer {
            id: job.id.clone(),
            file_name: job.file_name.clone(),
            size: job.size,
            digest: job.digest.clone(),
            chat: job.chat.clone(),
        };
        self.write_message(&mut stream, &offer, &[])
            .map_err(|e| pause(format!("connexion interrompue ({e})")))?;
        let ack = read_ack(&mut stream).ok_or_else(|| pause(format!("{} ne répond pas", target.name)))?;
        if !ack.ok {
            return Err(classify(&ack, &target.name));
        }
        let offset = ack.offset.unwrap_or(0);
        if offset > job.size || (offset % CHUNK_SIZE as u64 != 0 && offset != job.size) {
            return Err(pause("réponse de reprise invalide"));
        }
        if offset > 0 {
            eprintln!("[transfert] reprise de « {} » à partir de {offset} octets", job.file_name);
        }

        let mut file = File::open(&job.path).map_err(|e| fail(format!("lecture impossible ({e})")))?;
        file.seek(SeekFrom::Start(offset)).map_err(|e| fail(format!("lecture impossible ({e})")))?;
        job.sent = offset;
        self.upsert_transfer(self.view_for(job, TransferState::Active, ratio(offset, job.size)));

        let sent = (|| -> Result<(), JobError> {
            let mut buffer = vec![0_u8; CHUNK_SIZE];
            let mut sequence = offset / CHUNK_SIZE as u64;
            let mut chunks_this_attempt = 0_u64;
            let mut last_percent = percent(offset, job.size);
            let mut last_saved = Instant::now();
            while job.sent < job.size {
                if self.is_cancelled(&job.id) {
                    let cancel = Message::FileCancel { id: job.id.clone() };
                    let _ = self.write_message(&mut stream, &cancel, &[]);
                    return Err(fail("annulé"));
                }
                if let Some(limit) = self.cfg.fail_after_chunks {
                    if chunks_this_attempt == limit && !self.test_cut_done.swap(true, Ordering::SeqCst) {
                        stream.get_ref().close();
                        return Err(pause("connexion coupée (test)"));
                    }
                }
                let want = (job.size - job.sent).min(CHUNK_SIZE as u64) as usize;
                let read = read_full(&mut file, &mut buffer[..want]).map_err(|e| fail(format!("lecture impossible ({e})")))?;
                if read < want {
                    return Err(fail("le fichier a été modifié pendant l'envoi"));
                }
                let chunk = Message::FileChunk { id: job.id.clone(), sequence };
                self.write_message(&mut stream, &chunk, &buffer[..read])
                    .map_err(|e| pause(format!("connexion interrompue ({e})")))?;
                sequence += 1;
                chunks_this_attempt += 1;
                job.sent += read as u64;
                job.updated_ms = now_ms();

                let now_percent = percent(job.sent, job.size);
                if now_percent != last_percent {
                    last_percent = now_percent;
                    self.upsert_transfer(self.view_for(job, TransferState::Active, ratio(job.sent, job.size)));
                }
                if last_saved.elapsed() > Duration::from_secs(5) {
                    last_saved = Instant::now();
                    self.update_job(job);
                }
            }
            self.write_message(&mut stream, &Message::FileComplete { id: job.id.clone() }, &[])
                .map_err(|e| pause(format!("connexion interrompue ({e})")))?;
            let _ = stream.finish();
            Ok(())
        })();

        // Le récepteur confirme, ou explique pourquoi il a arrêté.
        if sent.is_err() {
            stream.get_ref().set_read_timeout(Some(Duration::from_secs(2))).ok();
        }
        let last = read_ack(&mut stream);
        match (sent, last) {
            (Err(JobError::Fail(reason)), _) => Err(JobError::Fail(reason)),
            (_, Some(ack)) if !ack.ok => Err(classify(&ack, &target.name)),
            (Ok(()), Some(_)) => Ok(()),
            (Ok(()), None) => Err(pause(format!("{} n'a pas confirmé la réception", target.name))),
            (Err(error), _) => Err(error),
        }
    }

    // ---------------------------------------------------------------- réception

    /// Accepte une offre de fichier. Retourne le nombre d'octets déjà reçus (reprise).
    pub(crate) fn begin_incoming(
        &self,
        peer: &TrustedPeer,
        id: &str,
        file_name: &str,
        size: u64,
        digest: &str,
        chat: Option<ChatLink>,
    ) -> Result<u64> {
        if !valid_id(id) {
            bail!("identifiant de transfert invalide");
        }
        if lock(&self.cancelled).contains(id) {
            return Err(refusal("cancelled", "transfert annulé"));
        }
        if lock(&self.incoming).contains_key(id) {
            return Err(refusal("busy", "transfert déjà en cours"));
        }

        let dir = self.partials_dir();
        fs::create_dir_all(&dir).context("dossier des fichiers partiels inaccessible")?;
        let meta_path = dir.join(format!("{id}.json"));
        let part_path = dir.join(format!("{id}.part"));
        let previous: Option<IncomingMeta> = fs::read(&meta_path).ok().and_then(|raw| serde_json::from_slice(&raw).ok());
        let resumable = previous
            .as_ref()
            .is_some_and(|meta| meta.peer_id == peer.id && meta.size == size && meta.digest == digest);

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&part_path)
            .context("écriture sur le disque impossible")?;
        let offset = if resumable {
            let length = file.metadata().map(|m| m.len()).unwrap_or(0).min(size);
            length - length % CHUNK_SIZE as u64
        } else {
            0
        };
        file.set_len(offset).context("écriture sur le disque impossible")?;

        // Reprise : on refait l'empreinte de ce qui est déjà sur le disque.
        let mut hasher = Sha256::new();
        if offset > 0 {
            file.seek(SeekFrom::Start(0))?;
            let mut buffer = vec![0_u8; CHUNK_SIZE];
            let mut left = offset;
            while left > 0 {
                let want = left.min(CHUNK_SIZE as u64) as usize;
                let read = read_full(&mut file, &mut buffer[..want])?;
                if read < want {
                    bail!("fichier partiel illisible");
                }
                hasher.update(&buffer[..read]);
                left -= read as u64;
            }
        }
        file.seek(SeekFrom::Start(offset))?;

        let meta = IncomingMeta {
            id: id.to_string(),
            peer_id: peer.id.clone(),
            peer_name: peer.name.clone(),
            file_name: sanitize_file_name(file_name),
            size,
            digest: digest.to_string(),
            chat,
            updated_ms: now_ms(),
        };
        fs::write(&meta_path, serde_json::to_vec(&meta)?).context("écriture sur le disque impossible")?;
        self.upsert_transfer(Self::incoming_view(&meta, TransferState::Active, offset));
        if offset > 0 {
            self.set_status(format!(
                "Reprise de « {} » à {} %",
                meta.file_name,
                percent(offset, size)
            ));
        }
        lock(&self.incoming).insert(
            id.to_string(),
            Incoming {
                meta,
                file,
                hasher,
                received: offset,
                next_sequence: offset / CHUNK_SIZE as u64,
                last_percent: percent(offset, size),
                last_saved: Instant::now(),
            },
        );
        Ok(offset)
    }

    pub(crate) fn append_chunk(&self, peer_id: &str, id: &str, sequence: u64, body: &[u8]) -> Result<()> {
        if lock(&self.cancelled).contains(id) {
            return Err(refusal("cancelled", "transfert annulé"));
        }
        let mut incoming = lock(&self.incoming);
        let item = incoming.get_mut(id).ok_or_else(|| anyhow!("transfert inconnu"))?;
        if item.meta.peer_id != peer_id {
            bail!("transfert inconnu");
        }
        if sequence != item.next_sequence {
            bail!("morceau hors séquence");
        }
        if body.len() > CHUNK_SIZE || item.received + body.len() as u64 > item.meta.size {
            bail!("trop de données reçues");
        }

        item.hasher.update(body);
        item.file.write_all(body).context("écriture sur le disque impossible")?;
        item.next_sequence += 1;
        item.received += body.len() as u64;

        if item.last_saved.elapsed() > Duration::from_secs(5) {
            item.last_saved = Instant::now();
            item.meta.updated_ms = now_ms();
            let path = self.partials_dir().join(format!("{id}.json"));
            let _ = fs::write(path, serde_json::to_vec(&item.meta).unwrap_or_default());
        }

        let now_percent = percent(item.received, item.meta.size);
        if now_percent != item.last_percent {
            item.last_percent = now_percent;
            let view = Self::incoming_view(&item.meta, TransferState::Active, item.received);
            drop(incoming);
            self.upsert_transfer(view);
        }
        Ok(())
    }

    pub(crate) fn finish_incoming(&self, peer_id: &str, id: &str) -> Result<()> {
        let item = {
            let mut incoming = lock(&self.incoming);
            match incoming.get(id) {
                Some(item) if item.meta.peer_id == peer_id => {}
                _ => bail!("transfert inconnu"),
            }
            incoming.remove(id).expect("présent juste au-dessus")
        };
        let Incoming { meta, mut file, hasher, received, .. } = item;

        if received != meta.size {
            bail!("fichier incomplet");
        }
        if crypto::encode_key(&hasher.finalize()) != meta.digest {
            bail!("empreinte invalide : le fichier est corrompu");
        }
        file.flush().context("écriture sur le disque impossible")?;
        drop(file);

        let part = self.partials_dir().join(format!("{id}.part"));
        if let Some(link) = &meta.chat {
            // Pièce jointe : rangée avec sa conversation, affichée dans la discussion.
            let destination = self.chat_media_path(link, &meta.file_name);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).context("enregistrement impossible")?;
            }
            let _ = fs::remove_file(&destination);
            move_file(&part, &destination).context("enregistrement impossible")?;
            let _ = fs::remove_file(self.partials_dir().join(format!("{id}.json")));
            self.upsert_transfer(Self::incoming_view(&meta, TransferState::Complete, meta.size));
            self.chat_changed();
            return Ok(());
        }

        let folder = self.receive_folder();
        fs::create_dir_all(&folder).with_context(|| format!("dossier de réception inaccessible ({})", folder.display()))?;
        let destination = unique_destination(&folder, &meta.file_name);
        move_file(&part, &destination).context("enregistrement impossible")?;
        let _ = fs::remove_file(self.partials_dir().join(format!("{id}.json")));

        {
            let mut list = lock(&self.received);
            list.insert(
                0,
                ReceivedView {
                    id: id.to_string(),
                    file_name: meta.file_name.clone(),
                    size: meta.size,
                    from: meta.peer_name.clone(),
                    path: destination,
                },
            );
            list.truncate(MAX_RECEIVED_SHOWN);
        }
        self.upsert_transfer(Self::incoming_view(&meta, TransferState::Complete, meta.size));
        self.set_status(format!("« {} » reçu de {}", meta.file_name, meta.peer_name));
        Ok(())
    }

    /// Connexion coupée : on garde le fichier partiel pour la reprise.
    pub(crate) fn suspend_incoming(&self, id: &str, reason: &str) {
        let Some(mut item) = lock(&self.incoming).remove(id) else { return };
        let _ = item.file.flush();
        item.meta.updated_ms = now_ms();
        let _ = fs::write(
            self.partials_dir().join(format!("{id}.json")),
            serde_json::to_vec(&item.meta).unwrap_or_default(),
        );
        self.upsert_transfer(Self::incoming_view(&item.meta, TransferState::Paused(reason.to_string()), item.received));
    }

    /// Abandon : supprime le fichier partiel.
    pub(crate) fn discard_incoming(&self, id: &str, reason: &str) {
        if !valid_id(id) {
            return;
        }
        let active = lock(&self.incoming).remove(id);
        let dir = self.partials_dir();
        let meta = active.as_ref().map(|item| item.meta.clone()).or_else(|| {
            fs::read(dir.join(format!("{id}.json")))
                .ok()
                .and_then(|raw| serde_json::from_slice::<IncomingMeta>(&raw).ok())
        });
        let received = active.as_ref().map(|item| item.received).unwrap_or(0);
        drop(active);
        let _ = fs::remove_file(dir.join(format!("{id}.part")));
        let _ = fs::remove_file(dir.join(format!("{id}.json")));
        if let Some(meta) = meta {
            self.upsert_transfer(Self::incoming_view(&meta, TransferState::Failed(reason.to_string()), received));
        }
    }
}

/// Refus du destinataire : définitif s'il a annulé, sinon on retentera.
fn classify(ack: &Ack, name: &str) -> JobError {
    let message = explain_refusal(ack, name);
    if ack.code == "cancelled" {
        JobError::Fail(message)
    } else {
        JobError::Pause(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_from_the_network_are_safe_file_names() {
        assert!(valid_id("3f2a-b4c1"));
        assert!(!valid_id("../../etc"));
        assert!(!valid_id(""));
        assert!(!valid_id(&"a".repeat(65)));
    }

    #[test]
    fn progress_helpers() {
        assert_eq!(percent(0, 0), 100);
        assert_eq!(percent(50, 200), 25);
        assert_eq!(ratio(1, 0), 1.0);
    }
}
