//! Moteur de MagicPlus : état partagé, appairage, presse-papiers, découverte.
//! Les transferts de fichiers (reprenables) sont dans `transfer.rs`.
//! Aucune dépendance à l'interface : l'interface lit `snapshot()` et appelle les méthodes.

use crate::{
    crypto,
    link::{BoxLink, Link},
    net,
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

/// Durée d'affichage d'un message d'état dans le bandeau du bas.
pub(crate) const STATUS_SHOWN: Duration = Duration::from_secs(7);

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
    /// Liaison Bluetooth avec les ordinateurs appairés (jamais dans les tests).
    pub bluetooth: bool,
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
            bluetooth: false,
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
        // Développement : autres ports (« 46454,46455 »), pour une copie lancée à côté de la vraie
        // sans être vue par les appareils du réseau.
        let ports = std::env::var("MAGICPLUS_PORTS").ok().and_then(|value| {
            let (discovery, transfer) = value.split_once(',')?;
            Some((discovery.trim().parse().ok()?, transfer.trim().parse().ok()?))
        });
        if let Some((discovery, transfer)) = ports {
            config.discovery_port = discovery;
            config.transfer_port = transfer;
        }
        // `MAGICPLUS_NO_BLUETOOTH=1` : pas de Bluetooth (copie de développement).
        config.bluetooth = std::env::var_os("MAGICPLUS_NO_BLUETOOTH").is_none();
        config
    }
}

pub(crate) struct Discovered {
    pub device: DeviceInfo,
    pub presence: crate::presence::Presence,
    pub fingerprint: String,
    /// Dernière adresse IP connue (non spécifiée si l'appareil n'a été vu qu'en Bluetooth).
    pub ip: IpAddr,
    pub tcp_port: u16,
    /// Dernière annonce, quel que soit le moyen.
    pub last_seen: Instant,
    /// Dernière annonce par le réseau local (UDP).
    pub udp_seen: Option<Instant>,
    /// Adresse Bluetooth et dernière annonce reçue par ce moyen.
    pub bluetooth: Option<(String, Instant)>,
    pub was_online: bool,
}

/// D'où vient une annonce.
#[derive(Debug, Clone)]
pub(crate) enum Source {
    Udp(IpAddr),
    Bluetooth(String),
}

impl Source {
    fn label(&self) -> String {
        match self {
            Source::Udp(ip) => ip.to_string(),
            Source::Bluetooth(_) => "Bluetooth".to_string(),
        }
    }
}

/// Comment joindre un appareil.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Route {
    Tcp(SocketAddr),
    Bluetooth(String),
}

impl Discovered {
    pub(crate) fn udp_online(&self) -> bool {
        self.udp_seen.is_some_and(|at| at.elapsed() < ONLINE_WINDOW)
    }

    pub(crate) fn bluetooth_online(&self) -> bool {
        self.bluetooth.as_ref().is_some_and(|(_, at)| at.elapsed() < crate::bluetooth::ONLINE_WINDOW)
    }

    pub(crate) fn online(&self) -> bool {
        self.udp_online() || self.bluetooth_online()
    }

    /// Réseau local d'abord (plus rapide), sinon Bluetooth.
    pub(crate) fn route(&self) -> Option<Route> {
        if self.udp_online() {
            Some(Route::Tcp(SocketAddr::new(self.ip, self.tcp_port)))
        } else if self.bluetooth_online() {
            self.bluetooth.as_ref().map(|(address, _)| Route::Bluetooth(address.clone()))
        } else {
            None
        }
    }

    fn address_label(&self) -> String {
        let ip = (!self.ip.is_unspecified()).then(|| format!("{}:{}", self.ip, self.tcp_port));
        match (ip, self.bluetooth_online() && !self.udp_online()) {
            (Some(ip), false) => ip,
            (Some(ip), true) => format!("{ip}  ·  via Bluetooth"),
            (None, _) => "via Bluetooth".to_string(),
        }
    }
}

/// Appareil associé et joignable en ce moment.
#[derive(Clone)]
pub(crate) struct Target {
    pub id: String,
    pub name: String,
    pub route: Route,
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
    /// Nom à afficher : notre surnom, sinon son pseudo, sinon le nom de l'appareil.
    pub name: String,
    /// Nom de l'appareil (ordinateur).
    pub device_name: String,
    /// Surnom donné ici (vide sinon).
    pub alias: String,
    /// Pseudo choisi par son utilisateur (vide sinon).
    pub pseudo: String,
    pub platform: String,
    pub address: String,
    pub fingerprint: String,
    pub trusted: bool,
    pub online: bool,
    /// Présence annoncée (`Offline` s'il n'est pas joignable ou invisible).
    pub presence: crate::presence::Presence,
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
    /// Pseudo montré à tous (vide : le nom de l'appareil).
    pub pseudo: String,
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
    /// Adresses des cartes réseau virtuelles (VMware, Hyper-V…), inutiles aux autres appareils.
    pub virtual_addresses: Vec<String>,
    /// Pare-feu de Windows : à régler si les autres appareils ne peuvent pas joindre celui-ci.
    pub firewall: crate::firewall::FirewallState,
    /// Demandes d'ami reçues et envoyées.
    pub friend_requests: Vec<crate::friends::FriendRequestView>,
    /// Liaison Bluetooth (vide : désactivée ou non prise en charge).
    pub bluetooth: String,
}

/// Notification du système à afficher (message reçu, demande d'ami…).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Notice {
    /// Conversation à ouvrir au clic (aucune : page Appareils).
    pub conversation: Option<String>,
    pub title: String,
    pub body: String,
    /// Ce qui l'a déclenchée (pour les préférences).
    pub kind: NoticeKind,
    /// Sans son (préférence).
    pub silent: bool,
    /// À montrer même si la conversation est déjà affichée au premier plan (préférence).
    pub even_if_visible: bool,
}

/// Origine d'une notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoticeKind {
    /// Message d'une discussion à deux.
    Direct,
    /// Message d'un groupe (`mention` : il nous mentionne).
    Group { mention: bool },
    /// Autre (demande d'ami, essai…).
    #[default]
    Other,
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
    /// Réponse avec données (synchronisation des canevas).
    Data(String),
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
    /// Moment du dernier message d'état : le bandeau s'efface ensuite tout seul.
    pub status_at: Mutex<Instant>,
    pub status_expired: AtomicBool,
    /// Premier lancement sur cet appareil (pas de « nouveautés » à montrer).
    pub fresh_install: bool,
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
    /// Canevas partagés (documents Yjs par conversation).
    pub canvases: Mutex<crate::canvas::CanvasStore>,
    /// Pseudos des autres appareils, appris par leurs annonces (id → pseudo).
    pub pseudos: Mutex<HashMap<String, String>>,
    /// Pare-feu (Windows) : MagicPlus peut-il être joint par les autres appareils ?
    pub firewall: Mutex<crate::firewall::FirewallState>,
    /// Demandes d'ami entre membres d'un même groupe.
    pub friends: Mutex<crate::friends::FriendStore>,
    /// Notifications pas encore affichées par l'interface.
    pub notices: Mutex<Vec<Notice>>,
    /// État de la liaison Bluetooth, pour la page Appareils (vide : pas de Bluetooth).
    pub bluetooth_status: Mutex<String>,
    /// Tests : la coupure simulée n'a lieu qu'une fois.
    pub test_cut_done: AtomicBool,
    /// Discussions (conversations, journal d'événements, synchronisation).
    pub chat: Mutex<ChatStore>,
    pub dirty: AtomicBool,
    pub shutdown: AtomicBool,
}

/// Pseudo ou surnom propre : sans caractère de contrôle, 32 caractères au plus.
pub(crate) fn clean_pseudo(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(32).collect::<String>().trim().to_string()
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

        let fresh_install = store.load::<StoredIdentity>("identity.json").is_none();
        let identity = match store.load::<StoredIdentity>("identity.json") {
            Some(identity) => identity,
            None => {
                let secret = crypto::generate_keypair();
                let identity = StoredIdentity {
                    id: uuid::Uuid::new_v4().to_string(),
                    name: default_device_name(),
                    pseudo: String::new(),
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
        let canvases = crate::canvas::CanvasStore::load(store.dir());
        let pseudos: HashMap<String, String> = store.load("pseudos.json").unwrap_or_default();
        let friends = crate::friends::FriendStore::load(&store);

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
                status_at: Mutex::new(Instant::now()),
                status_expired: AtomicBool::new(false),
                fresh_install,
                refused: Mutex::default(),
                manual_resolved: Mutex::default(),
                other_versions: Mutex::default(),
                update: Mutex::default(),
                profiles: Mutex::new(profiles),
                canvases: Mutex::new(canvases),
                pseudos: Mutex::new(pseudos),
                // Développement : `MAGICPLUS_FAKE_FIREWALL=1` montre l'avertissement du pare-feu.
                firewall: Mutex::new(if std::env::var_os("MAGICPLUS_FAKE_FIREWALL").is_some() {
                    crate::firewall::FirewallState::Blocked
                } else {
                    crate::firewall::FirewallState::Ok
                }),
                friends: Mutex::new(friends),
                notices: Mutex::default(),
                bluetooth_status: Mutex::default(),
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
        crate::canvas::spawn_canvas_scheduler(engine.inner.clone());
        engine.spawn_update_checker();
        engine.spawn_firewall_checker();
        // Développement : une notification d'essai 3 s après le démarrage.
        if std::env::var_os("MAGICPLUS_TEST_NOTICE").is_some() {
            let inner = engine.inner.clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(3));
                inner.notify(Notice {
                    title: "MagicPlus".to_string(),
                    body: "Notification d'essai : les messages reçus s'afficheront ainsi.".to_string(),
                    ..Notice::default()
                });
            });
        }
        crate::friends::spawn_friend_scheduler(engine.inner.clone());
        crate::bluetooth::spawn(engine.inner.clone());
        // Premier lancement, ou rien de nouveau : on retient la version (sinon l'interface montre
        // les nouveautés puis appelle `dismiss_whats_new`).
        if engine.whats_new().is_none() {
            engine.dismiss_whats_new();
        }
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

    /// Notification d'essai (bouton « Tester »), montrée si les notifications sont activées.
    pub fn test_notice(&self) {
        let prefs = self.snapshot().settings.notifications;
        if !prefs.enabled {
            self.inner.set_status("Les notifications sont désactivées");
            return;
        }
        self.inner.push_notice(Notice {
            title: "MagicPlus".to_string(),
            body: "Les notifications fonctionnent 🎉".to_string(),
            silent: !prefs.sound,
            even_if_visible: true,
            ..Notice::default()
        });
    }

    /// Notifications à afficher depuis le dernier appel (messages, demandes d'ami).
    pub fn take_notices(&self) -> Vec<Notice> {
        std::mem::take(&mut *lock(&self.inner.notices))
    }

    /// Lancement automatique à l'ouverture de la session (lu dans le système).
    pub fn launch_at_login(&self) -> bool {
        crate::autostart::is_enabled()
    }

    pub fn set_launch_at_login(&self, enabled: bool) -> Result<()> {
        crate::autostart::set_enabled(enabled)?;
        self.inner.set_status(if enabled {
            "MagicPlus se lancera à l'ouverture de la session, en arrière-plan"
        } else {
            "MagicPlus ne se lancera plus au démarrage"
        });
        Ok(())
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
                pseudo: identity.pseudo.clone(),
                platform: identity.platform.clone(),
                fingerprint: crypto::fingerprint(&identity.public_key),
            }
        };

        let trusted = lock(&inner.trusted).clone();
        let refused = lock(&inner.refused).clone();
        let aliases = lock(&inner.settings).aliases.clone();
        let pseudos = lock(&inner.pseudos).clone();
        let naming = |id: &str, device: &str| {
            let alias = aliases.get(id).cloned().unwrap_or_default();
            let pseudo = pseudos.get(id).cloned().unwrap_or_default();
            let name = if !alias.is_empty() { alias.clone() } else if !pseudo.is_empty() { pseudo.clone() } else { device.to_string() };
            (name, alias, pseudo)
        };
        let wants_pairing = |id: &str| refused.get(id).is_some_and(|at| at.elapsed() < PAIRING_REQUEST_WINDOW);
        let mut peers: Vec<PeerView> = Vec::new();
        {
            let discovered = lock(&inner.peers);
            for (id, peer) in discovered.iter() {
                let trusted_entry = trusted.get(id);
                let same_key = trusted_entry.is_some_and(|entry| entry.public_key == peer.device.public_key);
                let (name, alias, pseudo) = naming(id, &peer.device.name);
                peers.push(PeerView {
                    id: id.clone(),
                    name,
                    device_name: peer.device.name.clone(),
                    alias,
                    pseudo,
                    platform: peer.device.platform.clone(),
                    address: peer.address_label(),
                    fingerprint: peer.fingerprint.clone(),
                    trusted: same_key,
                    online: peer.online(),
                    presence: if peer.online() { peer.presence } else { crate::presence::Presence::Offline },
                    key_changed: trusted_entry.is_some() && !same_key,
                    wants_pairing: !same_key && wants_pairing(id),
                });
            }
            for (id, entry) in trusted.iter() {
                if !discovered.contains_key(id) {
                    let (name, alias, pseudo) = naming(id, &entry.name);
                    peers.push(PeerView {
                        id: id.clone(),
                        name,
                        device_name: entry.name.clone(),
                        alias,
                        pseudo,
                        platform: entry.platform.clone(),
                        address: String::new(),
                        fingerprint: crypto::fingerprint(&entry.public_key),
                        trusted: true,
                        online: false,
                        presence: crate::presence::Presence::Offline,
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
                            .filter(|peer| peer.udp_online())
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
        let (local_addresses, virtual_addresses) = shareable_addresses();
        Snapshot {
            identity,
            local_addresses,
            manual,
            peers,
            transfers: lock(&inner.transfers).clone(),
            received: lock(&inner.received).clone(),
            clipboard: lock(&inner.clipboard).clone(),
            // Vide une fois le délai passé : le bandeau disparaît.
            status: if lock(&inner.status_at).elapsed() < STATUS_SHOWN { self.status() } else { String::new() },
            settings,
            receive_folder,
            chat_unread,
            other_versions,
            update: self.update(),
            virtual_addresses,
            firewall: lock(&inner.firewall).clone(),
            friend_requests: self.friend_requests(),
            bluetooth: lock(&inner.bluetooth_status).clone(),
        }
    }

    // ---------------------------------------------------------------- pare-feu (Windows)

    fn spawn_firewall_checker(&self) {
        if !crate::firewall::SUPPORTED {
            return;
        }
        let inner = self.inner.clone();
        thread::spawn(move || {
            let mut next = Instant::now();
            while !inner.shutdown.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_secs(1));
                if Instant::now() < next {
                    continue;
                }
                next = Instant::now() + Duration::from_secs(15 * 60);
                if matches!(*lock(&inner.firewall), crate::firewall::FirewallState::Fixing) {
                    continue;
                }
                let state = crate::firewall::check();
                let mut current = lock(&inner.firewall);
                if *current != state && !matches!(*current, crate::firewall::FirewallState::Fixing) {
                    *current = state;
                    drop(current);
                    inner.touch();
                }
            }
        });
    }

    /// Autorise MagicPlus dans le pare-feu de Windows (une fenêtre de Windows demande l'accord).
    pub fn allow_firewall(&self) {
        use crate::firewall::FirewallState;
        {
            let mut state = lock(&self.inner.firewall);
            if *state == FirewallState::Fixing {
                return;
            }
            *state = FirewallState::Fixing;
        }
        self.inner.touch();
        self.inner.set_status("Autorisation de MagicPlus dans le pare-feu : acceptez la fenêtre de Windows…");
        let inner = self.inner.clone();
        thread::spawn(move || {
            let result = crate::firewall::allow(inner.cfg.discovery_port, inner.cfg.transfer_port);
            let state = match &result {
                Ok(()) => crate::firewall::check(),
                Err(error) => FirewallState::Failed(error.to_string()),
            };
            let message = match (&result, &state) {
                (Ok(()), FirewallState::Ok) => "MagicPlus est autorisé dans le pare-feu : les autres appareils peuvent maintenant joindre ce PC".to_string(),
                (Ok(()), _) => "Règles ajoutées, mais Windows bloque encore MagicPlus (pare-feu géré par l'entreprise ou un antivirus ?)".to_string(),
                (Err(error), _) => format!("Pare-feu non modifié : {error}"),
            };
            *lock(&inner.firewall) = state;
            inner.set_status(message);
            inner.touch();
        });
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

    /// Pseudo montré à tous les appareils (dès la détection, avant même l'association).
    pub fn set_pseudo(&self, pseudo: &str) -> Result<()> {
        let pseudo = clean_pseudo(pseudo);
        let mut identity = self.inner.identity.write().unwrap_or_else(|p| p.into_inner());
        identity.pseudo = pseudo.clone();
        self.inner.store.save("identity.json", &*identity)?;
        drop(identity);
        self.inner.chat_changed();
        self.inner.set_status(if pseudo.is_empty() {
            "Pseudo retiré : les autres voient le nom de l'appareil".to_string()
        } else {
            format!("Pseudo « {pseudo} » enregistré : les autres appareils le voient maintenant")
        });
        Ok(())
    }

    /// Surnom d'un autre appareil, visible seulement ici (vide : on l'enlève).
    pub fn set_alias(&self, peer_id: &str, alias: &str) -> Result<()> {
        let alias = clean_pseudo(alias);
        self.update_settings(|settings| {
            if alias.is_empty() {
                settings.aliases.remove(peer_id);
            } else {
                settings.aliases.insert(peer_id.to_string(), alias.clone());
            }
        })?;
        self.inner.chat_changed();
        self.inner.touch();
        Ok(())
    }

    /// Nom affiché d'un appareil : surnom, sinon pseudo, sinon `None`.
    pub fn display_name(&self, id: &str) -> Option<String> {
        self.inner.display_name(id)
    }

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
                // Bandeau d'information : effacé après quelques secondes.
                if lock(&inner.status_at).elapsed() >= STATUS_SHOWN && !inner.status_expired.swap(true, Ordering::SeqCst) {
                    inner.touch();
                }
                let trusted = lock(&inner.trusted).clone();
                let mut changed = false;
                let mut peers = lock(&inner.peers);
                peers.retain(|id, peer| {
                    let keep = peer.last_seen.elapsed() < FORGET_AFTER || trusted.contains_key(id);
                    changed |= !keep;
                    keep
                });
                for peer in peers.values_mut() {
                    let online = peer.online();
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
    /// Surnom donné ici, sinon pseudo de l'appareil (le nôtre compris).
    pub(crate) fn display_name(&self, id: &str) -> Option<String> {
        if let Some(alias) = lock(&self.settings).aliases.get(id).filter(|a| !a.is_empty()) {
            return Some(alias.clone());
        }
        if id == self.own_id() {
            let pseudo = self.identity.read().unwrap_or_else(|p| p.into_inner()).pseudo.clone();
            return (!pseudo.is_empty()).then_some(pseudo);
        }
        lock(&self.pseudos).get(id).filter(|p| !p.is_empty()).cloned()
    }

    /// Nom public d'un appareil : son pseudo, sinon le nom de l'appareil (jamais nos surnoms).
    pub(crate) fn public_name(&self, id: &str) -> String {
        let identity = self.identity.read().unwrap_or_else(|p| p.into_inner());
        if id == identity.id {
            return if identity.pseudo.is_empty() { identity.name.clone() } else { identity.pseudo.clone() };
        }
        drop(identity);
        if let Some(pseudo) = lock(&self.pseudos).get(id).filter(|p| !p.is_empty()) {
            return pseudo.clone();
        }
        lock(&self.trusted)
            .get(id)
            .map(|p| p.name.clone())
            .or_else(|| lock(&self.chat).member_name(id))
            .or_else(|| lock(&self.peers).get(id).map(|p| p.device.name.clone()))
            .unwrap_or_else(|| "Quelqu'un".to_string())
    }

    /// Tous les noms affichés connus (id → surnom ou pseudo).
    pub(crate) fn display_names(&self) -> HashMap<String, String> {
        let mut names: HashMap<String, String> = lock(&self.pseudos).clone();
        let own = self.identity.read().unwrap_or_else(|p| p.into_inner());
        if !own.pseudo.is_empty() {
            names.insert(own.id.clone(), own.pseudo.clone());
        }
        drop(own);
        for (id, alias) in lock(&self.settings).aliases.iter().filter(|(_, a)| !a.is_empty()) {
            names.insert(id.clone(), alias.clone());
        }
        names
    }

    pub(crate) fn touch(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    pub(crate) fn set_status(&self, text: impl Into<String>) {
        let text = text.into();
        eprintln!("[magicplus] {text}");
        *lock(&self.status) = text;
        *lock(&self.status_at) = Instant::now();
        self.status_expired.store(false, Ordering::SeqCst);
        self.touch();
    }

    /// Notification pour l'interface, selon les préférences et la présence (rien en « ne pas
    /// déranger »).
    pub(crate) fn notify(&self, mut notice: Notice) {
        let (prefs, presence) = {
            let settings = lock(&self.settings);
            (settings.notifications.clone(), crate::presence::effective(&settings, crate::presence::system_idle()))
        };
        if !crate::presence::should_notify(notice.kind, &prefs, presence) {
            return;
        }
        if !prefs.preview && notice.kind != NoticeKind::Other {
            notice.body = "Nouveau message".to_string();
        }
        notice.silent = !prefs.sound;
        notice.even_if_visible = prefs.when_focused;
        self.push_notice(notice);
    }

    fn push_notice(&self, notice: Notice) {
        let mut notices = lock(&self.notices);
        if notices.len() < 20 {
            notices.push(notice);
        }
        drop(notices);
        self.touch();
    }

    pub(crate) fn save_trusted(&self) -> Result<()> {
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
                pseudo: identity.pseudo.clone(),
            },
            tcp_port: self.cfg.transfer_port,
            discovery_port: self.cfg.discovery_port,
            reply,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            presence: crate::presence::effective(&lock(&self.settings), crate::presence::system_idle()).announced(),
        }
    }

    /// Annonce d'un appareil MagicPlus d'une autre version : on ne peut pas se parler, mais on le
    /// dit (page Appareils et barre d'état) au lieu de l'ignorer en silence.
    fn note_other_version(&self, hello: &Hello, source: &str) {
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
    pub(crate) fn on_hello(&self, hello: Hello, source: Source) -> Option<bool> {
        if hello.device.id == self.own_id() {
            return None;
        }
        if hello.protocol != PROTOCOL {
            self.note_other_version(&hello, &source.label());
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
        let presence = hello.presence;
        let mut device = hello.device;
        device.name = device.name.chars().filter(|c| !c.is_control()).take(60).collect();
        let name = device.name.clone();
        let id = device.id.clone();
        let hello_id = id.clone();

        // Pseudo annoncé : retenu (et gardé sur le disque pour quand l'appareil est hors ligne).
        let pseudo = clean_pseudo(&device.pseudo);
        let changed = {
            let mut pseudos = lock(&self.pseudos);
            let known = pseudos.get(&id).cloned().unwrap_or_default();
            if known != pseudo {
                if pseudo.is_empty() {
                    pseudos.remove(&id);
                } else {
                    pseudos.insert(id.clone(), pseudo.clone());
                }
                let _ = self.store.save("pseudos.json", &*pseudos);
                true
            } else {
                false
            }
        };
        if changed {
            self.chat_changed();
            self.touch();
        }

        let mut peers = lock(&self.peers);
        let announce = peers.get(&id).is_none_or(|known| !known.online());
        if peers.get(&id).is_some_and(|known| known.presence != presence) {
            self.touch();
        }
        let now = Instant::now();
        let entry = peers.entry(id).or_insert_with(|| Discovered {
            device: device.clone(),
            presence,
            fingerprint: fingerprint.clone(),
            ip: IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            tcp_port: 0,
            last_seen: now,
            udp_seen: None,
            bluetooth: None,
            was_online: true,
        });
        // Autre clé qu'avant : on oublie les moyens de le joindre appris avec l'ancienne.
        if entry.device.public_key != device.public_key {
            entry.udp_seen = None;
            entry.bluetooth = None;
        }
        entry.device = device;
        entry.presence = presence;
        entry.fingerprint = fingerprint;
        entry.last_seen = now;
        entry.was_online = true;
        match source {
            Source::Udp(ip) => {
                entry.ip = ip;
                entry.tcp_port = hello.tcp_port;
                entry.udp_seen = Some(now);
            }
            Source::Bluetooth(address) => entry.bluetooth = Some((address, now)),
        }
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
        if entry.public_key != peer.device.public_key {
            return None;
        }
        Some(Target {
            id: peer_id.to_string(),
            name: entry.name.clone(),
            route: peer.route()?,
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
        let link: BoxLink = match &target.route {
            Route::Tcp(address) => {
                let stream = TcpStream::connect_timeout(address, Duration::from_secs(5)).map_err(|error| {
                    anyhow!(
                        "{} injoignable ({error}) : son pare-feu bloque sans doute MagicPlus. Sur cet appareil, ouvrez \
                         Appareils et cliquez sur « Autoriser MagicPlus » (Windows)",
                        target.name
                    )
                })?;
                stream.set_nodelay(true).ok();
                Box::new(stream)
            }
            Route::Bluetooth(address) => crate::bluetooth::open_session(address)
                .map_err(|error| anyhow!("{} injoignable en Bluetooth : {error}", target.name))?,
        };
        link.set_write_timeout(Some(Duration::from_secs(30))).ok();
        link.set_read_timeout(Some(Duration::from_secs(30))).ok();
        secure::connect(link, &self.secret.to_bytes(), target.public_key.as_bytes(), &self.own_id())
    }

    fn send_text(&self, target: &Target, text: &str) -> Result<()> {
        let mut channel = self.connect(target)?;
        channel.get_ref().set_read_timeout(Some(Duration::from_secs(10))).ok();
        let message = Message::ClipboardText {
            id: uuid::Uuid::new_v4().to_string(),
            text: text.to_string(),
        };
        let sent = self.write_message(&mut channel, &message, &[]);
        channel.finish().ok();
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
    pub(crate) fn handle_connection(&self, stream: BoxLink) {
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
        } else if let Some(sender) = self.friend_request_sender(&remote_id, &key_b64) {
            // Appareil à qui nous avons demandé d'être ami : seulement sa réponse.
            sender
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
            channel.get_ref().close_after_reply();
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
                Ok(Reply::Data(data)) => write_ack(&mut channel, &Ack { ok: true, data, ..Ack::default() }),
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
        channel.get_ref().close_after_reply();
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
            Message::ChatEvents { conversation, events, group } => {
                let applied = self.receive_chat_events(sender, &conversation, events, group)?;
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
            Message::EmojiFetch { id } => Ok(Reply::Data(self.serve_emoji(sender, &id)?)),
            Message::CanvasSync { conversation, canvas, update, state } => {
                let reply = self.receive_canvas_sync(sender, &conversation, canvas, &update, &state)?;
                Ok(Reply::Data(reply))
            }
            Message::FriendRequest { conversation } => {
                let reply = self.receive_friend_request(sender, &conversation)?;
                Ok(Reply::Data(reply))
            }
            Message::FriendAnswer { accepted } => {
                self.receive_friend_answer(sender, accepted)?;
                Ok(Reply::None)
            }
        }
    }
}

// ------------------------------------------------------------------ petites aides

/// Canal chiffré sur une connexion TCP.
pub(crate) type Channel = SecureStream<BoxLink>;

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

/// Adresses IPv4 de cette machine, hors boucle locale, avec « carte virtuelle ? ».
fn local_ipv4_interfaces() -> Vec<(String, bool)> {
    let mut addresses: Vec<(String, bool)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|interface| !interface.is_loopback())
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(address) if !address.ip.is_link_local() => {
                Some((address.ip.to_string(), crate::firewall::is_virtual_adapter(&interface.name)))
            }
            _ => None,
        })
        .collect();
    addresses.sort();
    addresses.dedup_by(|a, b| a.0 == b.0);
    addresses
}

/// Toutes les adresses IPv4 de cette machine, hors boucle locale (cartes virtuelles comprises).
pub fn local_ipv4_addresses() -> Vec<String> {
    local_ipv4_interfaces().into_iter().map(|(ip, _)| ip).collect()
}

/// Adresses à donner aux autres appareils, puis celles des cartes virtuelles (mises à part,
/// sauf s'il n'y a qu'elles).
pub fn shareable_addresses() -> (Vec<String>, Vec<String>) {
    let (virtual_ones, real): (Vec<_>, Vec<_>) = local_ipv4_interfaces().into_iter().partition(|(_, virtual_one)| *virtual_one);
    let real: Vec<String> = real.into_iter().map(|(ip, _)| ip).collect();
    let virtual_ones: Vec<String> = virtual_ones.into_iter().map(|(ip, _)| ip).collect();
    if real.is_empty() {
        (virtual_ones, Vec::new())
    } else {
        (real, virtual_ones)
    }
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
