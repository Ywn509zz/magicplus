//! Moteur de MagicPlus : état partagé, appairage, presse-papiers, découverte.
//! Les transferts de fichiers (reprenables) sont dans `transfer.rs`.
//! Aucune dépendance à l'interface : l'interface lit `snapshot()` et appelle les méthodes.

use crate::{
    crypto, net,
    protocol::*,
    secure::{self, SecureStream},
    store::{default_data_dir, Settings, Store, StoredIdentity, TrustedPeer},
    chat::{ChatStore, SenderInfo},
    transfer::{Incoming, OutgoingJob, Retry},
    util::*,
};
use anyhow::{anyhow, bail, Context, Result};
use crypto_box::{PublicKey, SecretKey};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    net::{IpAddr, SocketAddr, TcpStream},
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard, RwLock,
    },
    thread,
    time::{Duration, Instant},
};

/// Un appareil qui n'a rien annoncé depuis ce délai est « hors ligne ».
pub(crate) const ONLINE_WINDOW: Duration = Duration::from_secs(8);
/// Un appareil non associé silencieux depuis ce délai disparaît de la liste.
const FORGET_AFTER: Duration = Duration::from_secs(60);
/// Un avertissement « autre version » disparaît après ce silence (annonces toutes les 2 s).
const OTHER_VERSION_WINDOW: Duration = Duration::from_secs(20);
/// Pendant ce délai, un appareil refusé faute d'association est signalé « demande d'association ».
const PAIRING_REQUEST_WINDOW: Duration = Duration::from_secs(10 * 60);
pub(crate) const MAX_TRANSFERS_SHOWN: usize = 50;
pub(crate) const MAX_RECEIVED_SHOWN: usize = 30;

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub discovery_port: u16,
    pub transfer_port: u16,
    /// Adresses supplémentaires où envoyer les annonces (tests, ou machines sur un autre sous-réseau).
    pub extra_targets: Vec<SocketAddr>,
    /// Délai avant de retenter un transfert interrompu (doublé à chaque échec, 60 s au plus).
    pub retry_delay: Duration,
    /// Tests uniquement : coupe la connexion du premier envoi après ce nombre de morceaux.
    pub fail_after_chunks: Option<u64>,
    /// Dépôt GitHub (« compte/dépôt ») où chercher les mises à jour ; `None` : jamais.
    pub update_repository: Option<String>,
}

impl Config {
    pub fn new(data_dir: PathBuf, discovery_port: u16, transfer_port: u16) -> Self {
        Self {
            data_dir,
            discovery_port,
            transfer_port,
            extra_targets: Vec::new(),
            retry_delay: Duration::from_secs(5),
            fail_after_chunks: None,
            update_repository: None,
        }
    }

    pub fn for_app() -> Self {
        let mut config = Self::new(
            std::env::var_os("MAGICPLUS_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(default_data_dir),
            DEFAULT_DISCOVERY_PORT,
            DEFAULT_TRANSFER_PORT,
        );
        config.update_repository = Some(
            std::env::var("MAGICPLUS_UPDATE_REPO").unwrap_or_else(|_| crate::update::DEFAULT_REPOSITORY.to_string()),
        );
        config
    }
}

pub(crate) struct Discovered {
    pub device: DeviceInfo,
    pub fingerprint: String,
    pub ip: IpAddr,
    pub tcp_port: u16,
    pub last_seen: Instant,
    pub was_online: bool,
}

/// Appareil associé et joignable en ce moment.
#[derive(Clone)]
pub(crate) struct Target {
    pub id: String,
    pub name: String,
    pub address: SocketAddr,
    pub public_key: PublicKey,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransferState {
    Active,
    /// Interrompu : reprendra tout seul (la raison est affichée).
    Paused(String),
    Complete,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct TransferView {
    pub id: String,
    pub file_name: String,
    pub size: u64,
    pub peer_name: String,
    pub sending: bool,
    pub progress: f32,
    pub state: TransferState,
    /// Pièce jointe de discussion transportée par ce transfert, s'il y en a une.
    pub attachment: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PeerView {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub address: String,
    pub fingerprint: String,
    pub trusted: bool,
    pub online: bool,
    /// Un appareil associé annonce maintenant une autre clé : possible usurpation.
    pub key_changed: bool,
    /// Cet appareil a essayé de nous envoyer quelque chose mais nous ne l'avons pas associé.
    pub wants_pairing: bool,
}

#[derive(Debug, Clone)]
pub struct ReceivedView {
    pub id: String,
    pub file_name: String,
    pub size: u64,
    pub from: String,
    /// Emplacement du fichier enregistré.
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ClipboardView {
    pub text: String,
    pub from: String,
}

#[derive(Debug, Clone)]
pub struct IdentityView {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub identity: IdentityView,
    /// Adresses IPv4 de cette machine (à saisir sur l'autre appareil si besoin).
    pub local_addresses: Vec<String>,
    /// Adresses saisies à la main, avec l'appareil qui y répond.
    pub manual: Vec<ManualView>,
    pub peers: Vec<PeerView>,
    pub transfers: Vec<TransferView>,
    pub received: Vec<ReceivedView>,
    pub clipboard: Option<ClipboardView>,
    pub status: String,
    pub settings: Settings,
    /// Dossier où arrivent les fichiers reçus (réglage ou Téléchargements).
    pub receive_folder: PathBuf,
    /// Messages de discussion non lus (toutes conversations).
    pub chat_unread: usize,
    /// Appareils qui s'annoncent avec une autre version de MagicPlus (ignorés tant que ça dure).
    pub other_versions: Vec<OtherVersionView>,
    /// Mises à jour de MagicPlus.
    pub update: crate::update::UpdateView,
}

/// Appareil MagicPlus d'une autre version : il s'annonce, mais les deux appareils s'ignorent.
#[derive(Debug, Clone, PartialEq)]
pub struct OtherVersionView {
    pub id: String,
    pub name: String,
    pub address: String,
    /// Sa version (« 0.3 », « 0.5.0 »…), lue dans l'annonce ou déduite du protocole.
    pub version: String,
    /// Vrai s'il est plus récent : c'est alors cet appareil-ci qu'il faut mettre à jour.
    pub newer: bool,
    /// Explication prête à afficher.
    pub message: String,
}

pub(crate) struct OtherVersion {
    view: OtherVersionView,
    last_seen: Instant,
}

/// Numéro d'un protocole « magicplus/N ».
fn protocol_number(protocol: &str) -> Option<u32> {
    protocol.strip_prefix("magicplus/")?.parse().ok()
}

#[derive(Debug, Clone)]
pub struct ManualView {
    pub address: String,
    /// Nom de l'appareil MagicPlus qui répond à cette adresse, s'il y en a un.
    pub peer_name: Option<String>,
    /// L'adresse n'a pas pu être résolue (nom inconnu).
    pub unresolved: bool,
}

/// Refus envoyé à l'expéditeur, avec un code qu'il sait interpréter.
#[derive(Debug)]
pub(crate) struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

pub(crate) fn refusal(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Refusal { code, message: message.into() })
}

/// Ce que le traitement d'une trame demande de répondre tout de suite.
pub(crate) enum Reply {
    None,
    /// Offre de fichier acceptée : nombre d'octets déjà reçus (reprise).
    Offset(u64),
}

pub(crate) struct Inner {
    pub cfg: Config,
    pub store: Store,
    pub identity: RwLock<StoredIdentity>,
    pub secret: SecretKey,
    pub peers: Mutex<HashMap<String, Discovered>>,
    pub trusted: Mutex<HashMap<String, TrustedPeer>>,
    pub settings: Mutex<Settings>,
    /// Réceptions en cours (connexion ouverte). Les réceptions interrompues sont sur le disque.
    pub incoming: Mutex<HashMap<String, Incoming>>,
    /// Envois à faire ou à reprendre (enregistrés dans `envois.json`).
    pub outgoing: Mutex<Vec<OutgoingJob>>,
    /// Envois en cours d'exécution (un fil chacun).
    pub active_sends: Mutex<HashSet<String>>,
    /// Envois annulés par l'utilisateur pendant leur exécution.
    pub cancelled: Mutex<HashSet<String>>,
    pub retries: Mutex<HashMap<String, Retry>>,
    pub transfers: Mutex<Vec<TransferView>>,
    pub received: Mutex<Vec<ReceivedView>>,
    pub clipboard: Mutex<Option<ClipboardView>>,
    pub status: Mutex<String>,
    /// Appareils refusés faute d'association (pour afficher « demande d'association »).
    pub refused: Mutex<HashMap<String, Instant>>,
    /// Résolution des adresses saisies à la main, mise à jour par le fil d'annonces
    /// (jamais sur le fil de l'interface : une résolution de nom peut prendre du temps).
    pub manual_resolved: Mutex<HashMap<String, Vec<SocketAddr>>>,
    /// Appareils entendus avec un autre protocole (autre version de MagicPlus).
    pub other_versions: Mutex<HashMap<String, OtherVersion>>,
    /// Mises à jour publiées sur GitHub (recherche, téléchargement).
    pub update: Mutex<crate::update::UpdateStatus>,
    /// Profils : le nôtre et ceux des contacts.
    pub profiles: Mutex<crate::profile::ProfileStore>,
    /// Tests : la coupure simulée n'a lieu qu'une fois.
    pub test_cut_done: AtomicBool,
    /// Discussions (conversations, journal d'événements, synchronisation).
    pub chat: Mutex<ChatStore>,
    pub dirty: AtomicBool,
    pub shutdown: AtomicBool,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<Inner>,
}

impl Engine {
    pub fn start(cfg: Config) -> Result<Engine> {
        let store = Store::new(cfg.data_dir.clone()).context("dossier de données inaccessible")?;

        let identity = match store.load::<StoredIdentity>("identity.json") {
            Some(identity) => identity,
            None => {
                let secret = crypto::generate_keypair();
                let identity = StoredIdentity {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: default_device_name(),
                    platform: platform_name().to_string(),
                    public_key: crypto::public_to_b64(&secret.public_key()),
                    secret_key: crypto::encode_key(&secret.to_bytes()),
                };
                store.save("identity.json", &identity)?;
                identity
            }
        };
        let secret = crypto::secret_from_b64(&identity.secret_key)?;
        let chat = ChatStore::load(store.dir().join("discussions"), &identity.id);
        let profiles = crate::profile::ProfileStore::load(store.dir());

        let trusted: HashMap<String, TrustedPeer> = store
            .load::<Vec<TrustedPeer>>("trusted.json")
            .unwrap_or_default()
            .into_iter()
            .map(|peer| (peer.id.clone(), peer))
            .collect();
        let settings = store.load::<Settings>("settings.json").unwrap_or_default();

        let engine = Engine {
            inner: Arc::new(Inner {
                cfg,
                store,
                identity: RwLock::new(identity),
                secret,
                peers: Mutex::default(),
                trusted: Mutex::new(trusted),
                settings: Mutex::new(settings),
                incoming: Mutex::default(),
                outgoing: Mutex::default(),
                active_sends: Mutex::default(),
                cancelled: Mutex::default(),
                retries: Mutex::default(),
                transfers: Mutex::default(),
                received: Mutex::default(),
                clipboard: Mutex::default(),
                status: Mutex::new("Recherche des appareils du réseau local…".to_string()),
                refused: Mutex::default(),
                manual_resolved: Mutex::default(),
                other_versions: Mutex::default(),
                update: Mutex::default(),
                profiles: Mutex::new(profiles),
                test_cut_done: AtomicBool::new(false),
                chat: Mutex::new(chat),
                dirty: AtomicBool::new(true),
                shutdown: AtomicBool::new(false),
            }),
        };

        engine.inner.restore_transfers();
        net::spawn(&engine);
        engine.spawn_housekeeping();
        crate::transfer::spawn_scheduler(engine.inner.clone());
        crate::chat::spawn_chat_scheduler(engine.inner.clone());
        crate::profile::spawn_profile_scheduler(engine.inner.clone());
        engine.spawn_update_checker();
        Ok(engine)
    }

    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::SeqCst);
    }

    /// Vrai une seule fois après chaque changement : l'interface se redessine alors.
    pub fn take_dirty(&self) -> bool {
        self.inner.dirty.swap(false, Ordering::SeqCst)
    }

    pub fn status(&self) -> String {
        lock(&self.inner.status).clone()
    }

    pub fn set_status(&self, text: impl Into<String>) {
        self.inner.set_status(text);
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = &self.inner;
        let identity = {
            let identity = inner.identity.read().unwrap_or_else(|p| p.into_inner());
            IdentityView {
                id: identity.id.clone(),
                name: identity.name.clone(),
                platform: identity.platform.clone(),
                fingerprint: crypto::fingerprint(&identity.public_key),
            }
        };

        let trusted = lock(&inner.trusted).clone();
        let refused = lock(&inner.refused).clone();
        let wants_pairing = |id: &str| refused.get(id).is_some_and(|at| at.elapsed() < PAIRING_REQUEST_WINDOW);
        let mut peers: Vec<PeerView> = Vec::new();
        {
            let discovered = lock(&inner.peers);
            for (id, peer) in discovered.iter() {
                let trusted_entry = trusted.get(id);
                let same_key = trusted_entry.is_some_and(|entry| entry.public_key == peer.device.public_key);
                peers.push(PeerView {
                    id: id.clone(),
                    name: peer.device.name.clone(),
                    platform: peer.device.platform.clone(),
                    address: format!("{}:{}", peer.ip, peer.tcp_port),
                    fingerprint: peer.fingerprint.clone(),
                    trusted: same_key,
                    online: peer.last_seen.elapsed() < ONLINE_WINDOW,
                    key_changed: trusted_entry.is_some() && !same_key,
                    wants_pairing: !same_key && wants_pairing(id),
                });
            }
            for (id, entry) in trusted.iter() {
                if !discovered.contains_key(id) {
                    peers.push(PeerView {
                        id: id.clone(),
                        name: entry.name.clone(),
                        platform: entry.platform.clone(),
                        address: String::new(),
                        fingerprint: crypto::fingerprint(&entry.public_key),
                        trusted: true,
                        online: false,
                        key_changed: false,
                        wants_pairing: false,
                    });
                }
            }
        }
        peers.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then(b.trusted.cmp(&a.trusted))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        let manual = {
            let settings = lock(&inner.settings);
            let resolved = lock(&inner.manual_resolved);
            let discovered = lock(&inner.peers);
            settings
                .manual_addresses
                .iter()
                .map(|address| {
                    let targets = resolved.get(address);
                    let peer_name = targets.and_then(|targets| {
                        discovered
                            .values()
                            .filter(|peer| peer.last_seen.elapsed() < ONLINE_WINDOW)
                            .find(|peer| targets.iter().any(|target| target.ip() == peer.ip))
                            .map(|peer| peer.device.name.clone())
                    });
                    ManualView {
                        address: address.clone(),
                        peer_name,
                        unresolved: targets.is_some_and(|targets| targets.is_empty()),
                    }
                })
                .collect()
        };

        // Calculé à part : `receive_folder` verrouille lui-même les réglages.
        let receive_folder = inner.receive_folder();
        let chat_unread = self.chat_unread();
        let settings = lock(&inner.settings).clone();
        let mut other_versions: Vec<OtherVersionView> = lock(&inner.other_versions)
            .values()
            .filter(|other| other.last_seen.elapsed() < OTHER_VERSION_WINDOW)
            .map(|other| other.view.clone())
            .collect();
        other_versions.sort_by(|a, b| a.name.cmp(&b.name));
        Snapshot {
            identity,
            local_addresses: local_ipv4_addresses(),
            manual,
            peers,
            transfers: lock(&inner.transfers).clone(),
            received: lock(&inner.received).clone(),
            clipboard: lock(&inner.clipboard).clone(),
            status: self.status(),
            settings,
            receive_folder,
            chat_unread,
            other_versions,
            update: self.update(),
        }
    }

    // ---------------------------------------------------------------- appairage

    /// Associe un appareil visible. L'appelant a montré l'empreinte à l'utilisateur.
    pub fn trust(&self, peer_id: &str) -> Result<()> {
        let entry = {
            let discovered = lock(&self.inner.peers);
            let peer = discovered
                .get(peer_id)
                .ok_or_else(|| anyhow!("appareil introuvable : il n'est plus visible sur le réseau"))?;
            TrustedPeer {
                id: peer_id.to_string(),
                name: peer.device.name.clone(),
                platform: peer.device.platform.clone(),
                public_key: peer.device.public_key.clone(),
            }
        };
        let name = entry.name.clone();
        lock(&self.inner.trusted).insert(peer_id.to_string(), entry);
        lock(&self.inner.refused).remove(peer_id);
        self.inner.save_trusted()?;
        self.inner.set_status(format!("{name} est maintenant associé"));
        Ok(())
    }

    pub fn forget(&self, peer_id: &str) -> Result<()> {
        lock(&self.inner.trusted).remove(peer_id);
        self.inner.save_trusted()?;
        self.inner.set_status("Appareil oublié");
        Ok(())
    }

    // ---------------------------------------------------------------- connexion par adresse

    /// Ajoute l'adresse d'un appareil à contacter directement (quand la découverte automatique
    /// est bloquée par le réseau). Accepte `192.168.1.20`, `192.168.1.20:45454` ou un nom.
    pub fn add_manual_address(&self, input: &str) -> Result<String> {
        let address = input.trim().to_string();
        if address.is_empty() {
            bail!("saisissez l'adresse IP de l'autre appareil, par exemple 192.168.1.20");
        }
        if address.len() > 253 || address.chars().any(|c| c.is_whitespace() || c.is_control()) {
            bail!("adresse invalide : « {address} »");
        }
        let targets = resolve_address(&address, self.inner.cfg.discovery_port);
        if targets.is_empty() {
            bail!("adresse introuvable : « {address} »");
        }
        if targets.iter().any(|target| local_ipv4_addresses().contains(&target.ip().to_string())) {
            bail!("« {address} » est l'adresse de cet appareil : saisissez celle de l'autre machine");
        }
        self.update_settings(|settings| {
            if !settings.manual_addresses.contains(&address) {
                settings.manual_addresses.push(address.clone());
            }
        })?;
        lock(&self.inner.manual_resolved).insert(address.clone(), targets.clone());
        net::send_hello(&self.inner, &targets);
        self.inner.set_status(format!("Appel de MagicPlus à l'adresse {address}…"));
        Ok(address)
    }

    pub fn remove_manual_address(&self, address: &str) -> Result<()> {
        self.update_settings(|settings| settings.manual_addresses.retain(|item| item != address))?;
        lock(&self.inner.manual_resolved).remove(address);
        self.inner.set_status(format!("Adresse {address} retirée"));
        Ok(())
    }

    // ---------------------------------------------------------------- réglages

    pub fn set_device_name(&self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            bail!("le nom de l'appareil ne peut pas être vide");
        }
        let mut identity = self.inner.identity.write().unwrap_or_else(|p| p.into_inner());
        identity.name = name.chars().take(60).collect();
        self.inner.store.save("identity.json", &*identity)?;
        drop(identity);
        self.inner.set_status("Nom de l'appareil enregistré");
        Ok(())
    }

    pub fn update_settings(&self, change: impl FnOnce(&mut Settings)) -> Result<()> {
        let mut settings = lock(&self.inner.settings);
        change(&mut settings);
        settings.resume_hours = settings.resume_hours.clamp(1, 24 * 30);
        self.inner.store.save("settings.json", &*settings)?;
        drop(settings);
        self.inner.touch();
        Ok(())
    }

    // ---------------------------------------------------------------- presse-papiers

    pub fn read_local_clipboard(&self) -> String {
        read_clipboard()
    }

    pub fn write_local_clipboard(&self, text: &str) -> Result<()> {
        write_clipboard(text)
    }

    /// Envoie le texte du presse-papiers à tous les appareils associés joignables.
    /// Chaque destinataire confirme ; un refus (appareil qui ne nous a pas associé…) est affiché.
    pub fn send_clipboard(&self, copy_selection: bool) {
        if copy_selection {
            if let Err(error) = copy_selection_to_clipboard() {
                self.inner.set_status(format!("Copie de la sélection impossible : {error}"));
            }
        }
        let text = self.read_local_clipboard();
        if text.trim().is_empty() {
            self.inner.set_status("Le presse-papiers est vide");
            return;
        }
        if text.len() > MAX_CLIPBOARD_BYTES {
            self.inner.set_status("Texte trop volumineux pour le presse-papiers partagé (2 Mo maximum)");
            return;
        }
        let targets = self.inner.online_targets();
        if targets.is_empty() {
            self.inner.set_status("Aucun appareil associé n'est joignable");
            return;
        }

        let count = targets.len();
        let mut handles = Vec::new();
        for target in targets {
            let inner = self.inner.clone();
            let text = text.clone();
            handles.push(thread::spawn(move || inner.send_text(&target, &text).map_err(|e| (target.name, e))));
        }
        let mut failures = Vec::new();
        for handle in handles {
            if let Ok(Err((name, error))) = handle.join() {
                failures.push(format!("{name} : {error}"));
            }
        }
        if failures.is_empty() {
            self.inner.set_status(format!("Presse-papiers envoyé à {count} appareil(s)"));
        } else {
            self.inner.set_status(format!("Envoi refusé ou incomplet — {}", failures.join(" ; ")));
        }
    }

    /// Remet le dernier texte reçu dans le presse-papiers.
    pub fn paste_last_received(&self) {
        let text = lock(&self.inner.clipboard).as_ref().map(|item| item.text.clone());
        match text {
            None => self.inner.set_status("Aucun texte reçu pour l'instant"),
            Some(text) => match self.write_local_clipboard(&text) {
                Ok(()) => self.inner.set_status("Dernier texte reçu copié dans le presse-papiers"),
                Err(error) => self.inner.set_status(error.to_string()),
            },
        }
    }

    pub fn remove_received(&self, id: &str) {
        lock(&self.inner.received).retain(|file| file.id != id);
        self.inner.touch();
    }

    fn spawn_housekeeping(&self) {
        let inner = self.inner.clone();
        thread::spawn(move || {
            while !inner.shutdown.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_secs(1));
                let trusted = lock(&inner.trusted).clone();
                let mut changed = false;
                let mut peers = lock(&inner.peers);
                peers.retain(|id, peer| {
                    let keep = peer.last_seen.elapsed() < FORGET_AFTER || trusted.contains_key(id);
                    changed |= !keep;
                    keep
                });
                for peer in peers.values_mut() {
                    let online = peer.last_seen.elapsed() < ONLINE_WINDOW;
                    if online != peer.was_online {
                        peer.was_online = online;
                        changed = true;
                    }
                }
                drop(peers);
                lock(&inner.refused).retain(|_, at| at.elapsed() < PAIRING_REQUEST_WINDOW);
                lock(&inner.other_versions).retain(|_, other| {
                    let keep = other.last_seen.elapsed() < OTHER_VERSION_WINDOW;
                    changed |= !keep;
                    keep
                });
                if changed {
                    inner.touch();
                }
            }
        });
    }
}

impl Inner {
    pub(crate) fn touch(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub(crate) fn set_status(&self, text: impl Into<String>) {
        let text = text.into();
        eprintln!("[magicplus] {text}");
        *lock(&self.status) = text;
        self.touch();
    }

    fn save_trusted(&self) -> Result<()> {
        let list: Vec<TrustedPeer> = lock(&self.trusted).values().cloned().collect();
        self.store.save("trusted.json", &list)?;
        self.touch();
        Ok(())
    }

    pub(crate) fn own_id(&self) -> String {
        self.identity.read().unwrap_or_else(|p| p.into_inner()).id.clone()
    }

    pub(crate) fn hello(&self, reply: bool) -> Hello {
        let identity = self.identity.read().unwrap_or_else(|p| p.into_inner());
        Hello {
            protocol: PROTOCOL.to_string(),
            device: DeviceInfo {
                id: identity.id.clone(),
                name: identity.name.clone(),
                platform: identity.platform.clone(),
                public_key: identity.public_key.clone(),
            },
            tcp_port: self.cfg.transfer_port,
            discovery_port: self.cfg.discovery_port,
            reply,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Annonce d'un appareil MagicPlus d'une autre version : on ne peut pas se parler, mais on le
    /// dit (page Appareils et barre d'état) au lieu de l'ignorer en silence.
    fn note_other_version(&self, hello: &Hello, source: IpAddr) {
        let (Some(theirs), Some(ours)) = (protocol_number(&hello.protocol), protocol_number(PROTOCOL)) else { return };
        let id = &hello.device.id;
        if id.is_empty() || id.len() > 64 {
            return;
        }
        let name: String = hello.device.name.chars().filter(|c| !c.is_control()).take(60).collect();
        let version: String = if hello.app_version.is_empty() {
            // Avant la 0.4, l'annonce ne donnait pas la version : protocole N = version 0.N.
            format!("0.{theirs}")
        } else {
            hello.app_version.chars().filter(|c| !c.is_control()).take(20).collect()
        };
        let mine = env!("CARGO_PKG_VERSION");
        let newer = theirs > ours;
        let message = if newer {
            format!(
                "« {name} » ({source}) a une version plus récente de MagicPlus ({version}). Mettez à jour cet appareil \
                 (version {mine}) : tant que les versions diffèrent, les deux appareils s'ignorent."
            )
        } else {
            format!(
                "« {name} » ({source}) a une ancienne version de MagicPlus ({version}). Installez-y la version {mine} : \
                 tant que les versions diffèrent, les deux appareils s'ignorent."
            )
        };
        let view = OtherVersionView { id: id.clone(), name, address: source.to_string(), version, newer, message };
        let mut others = lock(&self.other_versions);
        let fresh = others
            .get(id)
            .is_none_or(|known| known.view != view || known.last_seen.elapsed() > OTHER_VERSION_WINDOW);
        let status = fresh.then(|| view.message.clone());
        others.insert(id.clone(), OtherVersion { view, last_seen: Instant::now() });
        drop(others);
        if let Some(message) = status {
            self.set_status(message);
        }
    }

    /// Enregistre une annonce reçue. `None` si elle est ignorée (la nôtre, invalide, autre
    /// version) ; sinon `Some(vrai)` si l'appareil est nouveau ou revient après un silence.
    pub(crate) fn on_hello(&self, hello: Hello, source: IpAddr) -> Option<bool> {
        if hello.device.id == self.own_id() {
            return None;
        }
        if hello.protocol != PROTOCOL {
            self.note_other_version(&hello, source);
            return None;
        }
        // Jamais de clé ni d'identifiant démesurés ou illisibles.
        if hello.device.id.is_empty()
            || hello.device.id.len() > 64
            || crypto::public_from_b64(&hello.device.public_key).is_err()
        {
            return None;
        }

        let fingerprint = crypto::fingerprint(&hello.device.public_key);
        let mut device = hello.device;
        device.name = device.name.chars().filter(|c| !c.is_control()).take(60).collect();
        let name = device.name.clone();
        let id = device.id.clone();
        let hello_id = id.clone();

        let mut peers = lock(&self.peers);
        let announce = peers.get(&id).is_none_or(|known| known.last_seen.elapsed() > ONLINE_WINDOW);
        peers.insert(
            id,
            Discovered {
                device,
                fingerprint,
                ip: source,
                tcp_port: hello.tcp_port,
                last_seen: Instant::now(),
                was_online: true,
            },
        );
        drop(peers);
        // Il a été mis à jour : plus d'avertissement de version.
        if lock(&self.other_versions).remove(&hello_id).is_some() {
            self.touch();
        }
        if announce {
            self.set_status(format!("{name} détecté sur le réseau local"));
        }
        Some(announce)
    }

    /// Résout les adresses saisies à la main (appelé par le fil d'annonces, pas par l'interface).
    pub(crate) fn manual_targets(&self) -> Vec<SocketAddr> {
        let addresses = lock(&self.settings).manual_addresses.clone();
        let mut all = Vec::new();
        let mut resolved = HashMap::new();
        for address in addresses {
            let targets = resolve_address(&address, self.cfg.discovery_port);
            all.extend(targets.iter().copied());
            resolved.insert(address, targets);
        }
        let mut cache = lock(&self.manual_resolved);
        if *cache != resolved {
            *cache = resolved;
            drop(cache);
            self.touch();
        }
        all
    }

    /// Appareils associés (avec la clé mémorisée) qui s'annoncent en ce moment.
    pub(crate) fn online_targets(&self) -> Vec<Target> {
        let ids: Vec<String> = lock(&self.peers).keys().cloned().collect();
        ids.iter().filter_map(|id| self.target_for(id)).collect()
    }

    /// L'appareil associé `peer_id`, s'il est joignable en ce moment avec la clé mémorisée.
    pub(crate) fn target_for(&self, peer_id: &str) -> Option<Target> {
        let entry = lock(&self.trusted).get(peer_id).cloned()?;
        let peers = lock(&self.peers);
        let peer = peers.get(peer_id)?;
        // Si la clé annoncée a changé, on n'envoie rien (usurpation possible).
        if peer.last_seen.elapsed() >= ONLINE_WINDOW || entry.public_key != peer.device.public_key {
            return None;
        }
        Some(Target {
            id: peer_id.to_string(),
            name: entry.name.clone(),
            address: SocketAddr::new(peer.ip, peer.tcp_port),
            public_key: crypto::public_from_b64(&entry.public_key).ok()?,
        })
    }

    // ---------------------------------------------------------------- envoi

    /// Message applicatif : `u32 taille de l'en-tête` + en-tête JSON + octets bruts.
    pub(crate) fn write_message(&self, channel: &mut Channel, message: &Message, body: &[u8]) -> Result<()> {
        let header = serde_json::to_vec(message)?;
        let mut plain = Vec::with_capacity(4 + header.len() + body.len());
        plain.extend_from_slice(&(header.len() as u32).to_be_bytes());
        plain.extend_from_slice(&header);
        plain.extend_from_slice(body);
        channel.send(&plain)
    }

    /// Ouvre un canal chiffré (Noise) vers un appareil associé.
    pub(crate) fn connect(&self, target: &Target) -> Result<Channel> {
        let stream = TcpStream::connect_timeout(&target.address, Duration::from_secs(5))
            .map_err(|error| anyhow!("appareil inaccessible sur le réseau local ({error})"))?;
        stream.set_nodelay(true).ok();
        stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        secure::connect(stream, &self.secret.to_bytes(), target.public_key.as_bytes(), &self.own_id())
    }

    fn send_text(&self, target: &Target, text: &str) -> Result<()> {
        let mut channel = self.connect(target)?;
        channel.get_ref().set_read_timeout(Some(Duration::from_secs(10))).ok();
        let message = Message::ClipboardText {
            id: uuid::Uuid::new_v4().to_string(),
            text: text.to_string(),
        };
        let sent = self.write_message(&mut channel, &message, &[]);
        channel.get_ref().shutdown(std::net::Shutdown::Write).ok();
        match read_ack(&mut channel) {
            Some(ack) if ack.ok => Ok(()),
            Some(ack) => Err(anyhow!(explain_refusal(&ack, &target.name))),
            None => {
                sent?;
                bail!("{} n'a pas confirmé la réception", target.name)
            }
        }
    }

    // ---------------------------------------------------------------- réception

    /// L'appareil associé dont la clé publique a été prouvée par la poignée de main.
    fn trusted_by_key(&self, key: &[u8; 32]) -> Option<TrustedPeer> {
        lock(&self.trusted)
            .values()
            .find(|peer| crypto::public_from_b64(&peer.public_key).is_ok_and(|public| public.as_bytes() == key))
            .cloned()
    }

    /// Traite une connexion entrante : négociation Noise, puis messages chiffrés jusqu'à la fin.
    /// Les réponses (`Ack`) passent dans le même canal chiffré.
    pub(crate) fn handle_connection(&self, stream: TcpStream) {
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(30))).ok();
        let (mut channel, remote_id) = match secure::accept(stream, &self.secret.to_bytes()) {
            Ok(accepted) => accepted,
            Err(error) => {
                eprintln!("[magicplus] connexion refusée : {error}");
                return;
            }
        };

        let key_b64 = crypto::encode_key(channel.remote_static());
        let sender = if let Some(peer) = self.trusted_by_key(channel.remote_static()) {
            SenderInfo { id: peer.id, name: peer.name, platform: peer.platform, public_key: peer.public_key, trusted: true }
        } else if let Some(member) = self.chat_member_by_key(&remote_id, &key_b64) {
            // Membre d'un groupe, présenté par son créateur : discussions de ce groupe seulement.
            SenderInfo { id: member.id, name: member.name, platform: member.platform, public_key: member.public_key, trusted: false }
        } else {
            // Le nom n'est repris que si l'appareil annoncé a bien cette clé (pas d'usurpation).
            let known = lock(&self.peers).get(&remote_id).and_then(|peer| {
                let same_key = crypto::public_from_b64(&peer.device.public_key)
                    .is_ok_and(|public| public.as_bytes() == channel.remote_static());
                same_key.then(|| peer.device.name.clone())
            });
            if let Some(name) = known {
                lock(&self.refused).insert(remote_id, Instant::now());
                self.set_status(format!("{name} veut vous envoyer quelque chose : associez-le dans la page Appareils"));
            }
            write_ack(&mut channel, &Ack::refused("cet appareil ne vous a pas encore associé", "not-paired"));
            return;
        };

        let mut started: Vec<String> = Vec::new();
        let mut failure: Option<Ack> = None;
        loop {
            let plain = match channel.recv() {
                Ok(Some(plain)) => plain,
                Ok(None) => break,
                Err(error) => {
                    if !started.is_empty() {
                        eprintln!("[magicplus] réception interrompue : {error}");
                    }
                    failure = Some(Ack::refused(error.to_string(), "io"));
                    break;
                }
            };
            match self.handle_message(&sender, &plain, &mut started) {
                Ok(Reply::None) => {}
                Ok(Reply::Offset(offset)) => write_ack(&mut channel, &Ack::offset(offset)),
                Err(error) => {
                    let code = error.downcast_ref::<Refusal>().map(|refusal| refusal.code).unwrap_or("error");
                    if !matches!(code, "unknown-conversation" | "not-member") {
                        self.set_status(format!("Réception refusée : {error}"));
                    }
                    failure = Some(Ack::refused(error.to_string(), code));
                    break;
                }
            }
        }

        // Transfert coupé en route : le fichier partiel est gardé pour la reprise.
        for id in started {
            self.suspend_incoming(&id, "interrompu — reprise automatique quand l'expéditeur revient");
        }
        write_ack(&mut channel, &failure.unwrap_or_else(Ack::ok));
        let _ = channel.get_ref().shutdown(std::net::Shutdown::Both);
    }

    fn handle_message(&self, sender: &SenderInfo, plain: &[u8], started: &mut Vec<String>) -> Result<Reply> {
        if plain.len() < 4 {
            bail!("message invalide");
        }
        let header_len = u32::from_be_bytes([plain[0], plain[1], plain[2], plain[3]]) as usize;
        if plain.len() < 4 + header_len {
            bail!("message invalide");
        }
        let message: Message = serde_json::from_slice(&plain[4..4 + header_len]).context("en-tête illisible")?;
        let body = &plain[4 + header_len..];
        let not_paired = || refusal("not-paired", "cet appareil ne vous a pas encore associé");
        let as_peer = || TrustedPeer {
            id: sender.id.clone(),
            name: sender.name.clone(),
            platform: sender.platform.clone(),
            public_key: sender.public_key.clone(),
        };

        match message {
            Message::ClipboardText { text, .. } => {
                if !sender.trusted {
                    return Err(not_paired());
                }
                if text.len() > MAX_CLIPBOARD_BYTES {
                    bail!("texte trop volumineux");
                }
                let auto_copy = lock(&self.settings).auto_copy_received;
                let copied = auto_copy && write_clipboard(&text).is_ok();
                *lock(&self.clipboard) = Some(ClipboardView { text, from: sender.name.clone() });
                self.set_status(if copied {
                    format!("Texte reçu de {} (copié dans le presse-papiers)", sender.name)
                } else {
                    format!("Texte reçu de {}", sender.name)
                });
                Ok(Reply::None)
            }
            Message::FileOffer { id, file_name, size, digest, chat } => {
                // Fichier « libre » : appareil associé seulement. Pièce jointe : membre de la conversation.
                match &chat {
                    None if !sender.trusted => return Err(not_paired()),
                    Some(link) if !self.chat_link_allowed(sender, link) => {
                        return Err(refusal("not-member", "vous n'êtes pas membre de cette conversation"));
                    }
                    _ => {}
                }
                let offset = self.begin_incoming(&as_peer(), &id, &file_name, size, &digest, chat)?;
                started.push(id);
                Ok(Reply::Offset(offset))
            }
            Message::FileChunk { id, sequence } => {
                if let Err(error) = self.append_chunk(&sender.id, &id, sequence, body) {
                    started.retain(|item| item != &id);
                    self.discard_incoming(&id, &error.to_string());
                    return Err(error);
                }
                Ok(Reply::None)
            }
            Message::FileComplete { id } => {
                started.retain(|item| item != &id);
                if let Err(error) = self.finish_incoming(&sender.id, &id) {
                    self.discard_incoming(&id, &error.to_string());
                    return Err(error);
                }
                Ok(Reply::None)
            }
            Message::FileCancel { id } => {
                started.retain(|item| item != &id);
                let owned = lock(&self.incoming).get(&id).is_none_or(|item| item.peer_id() == sender.id);
                if owned {
                    self.discard_incoming(&id, &format!("annulé par {}", sender.name));
                }
                Ok(Reply::None)
            }
            Message::ChatEvents { conversation, events } => {
                let applied = self.receive_chat_events(sender, &conversation, events)?;
                Ok(Reply::Offset(applied))
            }
            Message::ChatTyping { conversation } => {
                self.receive_chat_typing(sender, &conversation)?;
                Ok(Reply::None)
            }
            Message::ChatRead { conversation, lamport } => {
                self.receive_chat_read(sender, &conversation, lamport)?;
                Ok(Reply::None)
            }
            Message::Profile { profile } => {
                self.receive_profile(sender, profile, body)?;
                Ok(Reply::None)
            }
        }
    }
}

// ------------------------------------------------------------------ petites aides

/// Canal chiffré sur une connexion TCP.
pub(crate) type Channel = SecureStream<TcpStream>;

pub(crate) fn write_ack(channel: &mut Channel, ack: &Ack) {
    if let Ok(json) = serde_json::to_vec(ack) {
        let _ = channel.send(&json);
    }
}

pub(crate) fn read_ack(channel: &mut Channel) -> Option<Ack> {
    channel.recv().ok().flatten().and_then(|json| serde_json::from_slice(&json).ok())
}

/// Message lisible pour un refus reçu de `name`.
pub(crate) fn explain_refusal(ack: &Ack, name: &str) -> String {
    match ack.code.as_str() {
        "not-paired" => format!("{name} ne vous a pas encore associé : sur {name}, ouvrez Appareils et cliquez sur Associer"),
        "cancelled" => format!("annulé par {name}"),
        _ if ack.error.is_empty() => format!("refusé par {name}"),
        _ => ack.error.clone(),
    }
}

fn read_clipboard() -> String {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.get_text())
        .unwrap_or_default()
}

fn write_clipboard(text: &str) -> Result<()> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text.to_string()))
        .map_err(|error| anyhow!("presse-papiers inaccessible : {error}"))
}

/// Simule « copier » dans l'application au premier plan (raccourci « copier et envoyer »).
fn copy_selection_to_clipboard() -> Result<()> {
    // Laisse le temps de relâcher les touches du raccourci, sinon elles se mélangent au Ctrl/Cmd+C.
    thread::sleep(Duration::from_millis(350));
    let status = if cfg!(target_os = "macos") {
        Command::new("osascript")
            .args(["-e", r#"tell application "System Events" to keystroke "c" using command down"#])
            .status()
    } else if cfg!(target_os = "windows") {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('^c')",
            ])
            .status()
    } else {
        Command::new("sh")
            .args([
                "-c",
                "if command -v xdotool >/dev/null 2>&1; then xdotool key ctrl+c; else exit 2; fi",
            ])
            .status()
    }
    .map_err(|error| anyhow!("{error}"))?;

    if !status.success() {
        bail!("autorisation d'accessibilité manquante ?");
    }
    thread::sleep(Duration::from_millis(220));
    Ok(())
}

/// Adresses IPv4 de cette machine, hors boucle locale.
pub fn local_ipv4_addresses() -> Vec<String> {
    let mut addresses: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|interface| !interface.is_loopback())
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(address) if !address.ip.is_link_local() => Some(address.ip.to_string()),
            _ => None,
        })
        .collect();
    addresses.sort();
    addresses.dedup();
    addresses
}

/// `ip`, `ip:port`, `nom` ou `nom:port` vers des adresses IPv4 (port de découverte par défaut).
pub fn resolve_address(address: &str, default_port: u16) -> Vec<SocketAddr> {
    use std::net::ToSocketAddrs;
    if let Ok(socket) = address.parse::<SocketAddr>() {
        return vec![socket];
    }
    if let Ok(ip) = address.parse::<IpAddr>() {
        return vec![SocketAddr::new(ip, default_port)];
    }
    let resolved = match address.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => {
            (host, port.parse::<u16>().unwrap_or(default_port)).to_socket_addrs()
        }
        _ => (address, default_port).to_socket_addrs(),
    };
    let mut targets: Vec<SocketAddr> = resolved
        .map(|items| items.filter(|item| item.is_ipv4()).collect())
        .unwrap_or_default();
    targets.dedup();
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_typed_addresses() {
        assert_eq!(resolve_address("192.168.1.20", 45454), vec!["192.168.1.20:45454".parse().unwrap()]);
        assert_eq!(resolve_address("10.0.0.5:5000", 45454), vec!["10.0.0.5:5000".parse().unwrap()]);
        assert_eq!(resolve_address("localhost:7000", 45454), vec!["127.0.0.1:7000".parse().unwrap()]);
        assert!(resolve_address("nom-qui-n-existe-pas.invalid", 45454).is_empty());
    }

    #[test]
    fn refusals_are_explained() {
        let ack = Ack::refused("cet appareil ne vous a pas encore associé", "not-paired");
        assert!(explain_refusal(&ack, "PC").contains("cliquez sur Associer"));
        assert_eq!(explain_refusal(&Ack::refused("", "cancelled"), "PC"), "annulé par PC");
    }
}
