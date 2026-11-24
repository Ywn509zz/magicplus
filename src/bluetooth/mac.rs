//! macOS : RFCOMM par IOBluetooth. Les ordinateurs s'appairent dans Réglages Système → Bluetooth.
//!
//! IOBluetooth rappelle ses délégués sur la boucle d'exécution du fil qui l'utilise : tout se
//! passe donc sur un fil dédié (« bluetooth ») qui fait tourner sa boucle et exécute les
//! commandes des autres fils (ouvrir, écrire, fermer…). Les données reçues repartent par des
//! canaux vers les liens (`MacLink`) lus par le moteur.

use super::{uuid_bytes, PairedDevice};
use crate::link::{BoxLink, Link};
use anyhow::{anyhow, bail, Result};
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject},
    sel, AllocAnyThread, ClassType, Message,
};
use objc2_foundation::{NSArray, NSData, NSDate, NSDefaultRunLoopMode, NSDictionary, NSNumber, NSRunLoop, NSString};
use objc2_io_bluetooth::{
    kBluetoothDeviceClassMajorComputer, BluetoothHCIPowerState, IOBluetoothDevice, IOBluetoothHostController,
    IOBluetoothRFCOMMChannel, IOBluetoothSDPServiceRecord, IOBluetoothSDPUUID, IOBluetoothUserNotification,
    IOBluetoothUserNotificationChannelDirection,
};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    ffi::c_void,
    io::{self, Read, Write},
    sync::{
        mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

pub(super) const SUPPORTED: bool = true;

type OnLink = Arc<dyn Fn(BoxLink, String) + Send + Sync>;

enum Command {
    Listen { on_link: OnLink, reply: SyncSender<Result<()>> },
    Connect { address: String, reply: SyncSender<Result<BoxLink>> },
    Write { id: u64, data: Vec<u8>, reply: SyncSender<io::Result<()>> },
    Close { id: u64 },
    Paired { reply: SyncSender<Vec<PairedDevice>> },
    Powered { reply: SyncSender<bool> },
}

/// Commandes vers le fil Bluetooth (créé au premier usage).
fn commands() -> Sender<Command> {
    static COMMANDS: OnceLock<Mutex<Sender<Command>>> = OnceLock::new();
    COMMANDS
        .get_or_init(|| {
            let (sender, receiver) = channel();
            let _ = thread::Builder::new().name("bluetooth".into()).spawn(move || bluetooth_thread(receiver));
            Mutex::new(sender)
        })
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

fn ask<T>(make: impl FnOnce(SyncSender<T>) -> Command, timeout: Duration) -> Option<T> {
    let (reply, answer) = sync_channel(1);
    commands().send(make(reply)).ok()?;
    answer.recv_timeout(timeout).ok()
}

pub(super) fn powered() -> bool {
    ask(|reply| Command::Powered { reply }, Duration::from_secs(10)).unwrap_or(false)
}

pub(super) fn paired_computers() -> Vec<PairedDevice> {
    ask(|reply| Command::Paired { reply }, Duration::from_secs(10)).unwrap_or_default()
}

pub(super) fn listen(on_link: Box<dyn Fn(BoxLink, String) + Send + Sync>) -> Result<()> {
    let on_link: OnLink = Arc::from(on_link);
    ask(|reply| Command::Listen { on_link, reply }, Duration::from_secs(20)).unwrap_or_else(|| Err(anyhow!("pas de réponse")))
}

pub(super) fn connect(address: &str) -> Result<BoxLink> {
    let address = address.to_string();
    ask(|reply| Command::Connect { address, reply }, Duration::from_secs(60))
        .unwrap_or_else(|| Err(anyhow!("délai dépassé")))
}

// ------------------------------------------------------------------ lien vu par le moteur

struct MacLink {
    id: u64,
    data: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    read_timeout: Cell<Option<Duration>>,
    write_timeout: Cell<Option<Duration>>,
}

impl Read for MacLink {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_empty() {
            let received = match self.read_timeout.get() {
                Some(timeout) => self.data.recv_timeout(timeout),
                None => self.data.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(bytes) => self.pending = bytes,
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
                Err(RecvTimeoutError::Timeout) => return Err(io::Error::new(io::ErrorKind::TimedOut, "Bluetooth : délai dépassé")),
            }
        }
        let count = buf.len().min(self.pending.len());
        buf[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        Ok(count)
    }
}

impl Write for MacLink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (reply, answer) = sync_channel(1);
        commands()
            .send(Command::Write { id: self.id, data: buf.to_vec(), reply })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Bluetooth arrêté"))?;
        let timeout = self.write_timeout.get().unwrap_or(Duration::from_secs(120));
        match answer.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(buf.len()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "Bluetooth : envoi trop long")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Link for MacLink {
    fn half_close(&self) -> bool {
        false
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout.set(timeout);
        Ok(())
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout.set(timeout);
        Ok(())
    }

    fn close(&self) {
        let _ = commands().send(Command::Close { id: self.id });
    }
}

impl Drop for MacLink {
    fn drop(&mut self) {
        self.close();
    }
}

// ------------------------------------------------------------------ fil Bluetooth

#[derive(Default)]
struct State {
    /// Canaux ouverts, par identifiant de lien.
    channels: HashMap<u64, Retained<IOBluetoothRFCOMMChannel>>,
    /// Pointeur du canal → (identifiant, destination des données reçues).
    inbox: HashMap<usize, (u64, Sender<Vec<u8>>)>,
    next_id: u64,
    on_link: Option<OnLink>,
    _service: Option<Retained<IOBluetoothSDPServiceRecord>>,
    _notification: Option<Retained<IOBluetoothUserNotification>>,
    /// Requête SDP terminée (rappel du délégué).
    sdp_done: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
    static DELEGATE: RefCell<Option<Retained<BluetoothDelegate>>> = const { RefCell::new(None) };
}

fn key(channel: &IOBluetoothRFCOMMChannel) -> usize {
    channel as *const IOBluetoothRFCOMMChannel as usize
}

/// Enregistre un canal ouvert et renvoie le lien qui le lit.
fn register(channel: Retained<IOBluetoothRFCOMMChannel>) -> MacLink {
    let (sender, receiver) = channel_pair();
    let id = STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.next_id += 1;
        let id = state.next_id;
        state.inbox.insert(key(&channel), (id, sender));
        state.channels.insert(id, channel);
        id
    });
    MacLink { id, data: receiver, pending: Vec::new(), read_timeout: Cell::new(None), write_timeout: Cell::new(None) }
}

fn channel_pair() -> (Sender<Vec<u8>>, Receiver<Vec<u8>>) {
    channel()
}

fn delegate() -> Retained<BluetoothDelegate> {
    DELEGATE.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(|| {
                // SAFETY : initialisation d'un objet sans variables d'instance.
                unsafe { msg_send![BluetoothDelegate::alloc(), init] }
            })
            .clone()
    })
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MagicPlusBluetoothDelegate"]
    struct BluetoothDelegate;

    impl BluetoothDelegate {
        #[unsafe(method(rfcommChannelData:data:length:))]
        fn channel_data(&self, channel: &IOBluetoothRFCOMMChannel, data: *mut c_void, length: usize) {
            if data.is_null() || length == 0 {
                return;
            }
            // SAFETY : IOBluetooth fournit `length` octets valables pendant le rappel.
            let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, length) }.to_vec();
            STATE.with(|state| {
                if let Some((_, sender)) = state.borrow().inbox.get(&key(channel)) {
                    let _ = sender.send(bytes);
                }
            });
        }

        #[unsafe(method(rfcommChannelClosed:))]
        fn channel_closed(&self, channel: &IOBluetoothRFCOMMChannel) {
            STATE.with(|state| {
                let mut state = state.borrow_mut();
                if let Some((id, _)) = state.inbox.remove(&key(channel)) {
                    state.channels.remove(&id);
                }
            });
        }

        #[unsafe(method(rfcommChannelOpenComplete:status:))]
        fn channel_open_complete(&self, _channel: &IOBluetoothRFCOMMChannel, _status: i32) {}

        /// Un autre appareil ouvre notre service.
        #[unsafe(method(newRFCOMMChannelOpened:channel:))]
        fn new_channel(&self, _notification: &IOBluetoothUserNotification, channel: &IOBluetoothRFCOMMChannel) {
            // SAFETY : délégué valable (cet objet) pour un canal qui vient de s'ouvrir.
            unsafe { channel.setDelegate(Some(as_any(self))) };
            let address = unsafe { channel.getDevice() }
                .and_then(|device| unsafe { device.addressString() })
                .map(|address| address.to_string())
                .unwrap_or_default();
            let retained: Retained<IOBluetoothRFCOMMChannel> = channel.retain();
            let link = register(retained);
            let on_link = STATE.with(|state| state.borrow().on_link.clone());
            if let Some(on_link) = on_link {
                on_link(Box::new(link), address);
            }
        }

        #[unsafe(method(sdpQueryComplete:status:))]
        fn sdp_complete(&self, _device: &IOBluetoothDevice, _status: i32) {
            STATE.with(|state| state.borrow_mut().sdp_done = true);
        }
    }
);

/// Un tour de boucle d'exécution (délégués IOBluetooth).
fn run_loop_step(seconds: f64) {
    let until = NSDate::dateWithTimeIntervalSinceNow(seconds);
    // SAFETY : mode par défaut, sur la boucle de ce fil.
    let ran = NSRunLoop::currentRunLoop().runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &until);
    if !ran {
        thread::sleep(Duration::from_secs_f64(seconds));
    }
}

fn bluetooth_thread(receiver: Receiver<Command>) {
    let mut deferred: VecDeque<Command> = VecDeque::new();
    loop {
        while let Some(command) = deferred.pop_front() {
            handle(command, &receiver, &mut deferred);
        }
        loop {
            match receiver.try_recv() {
                Ok(command) => handle(command, &receiver, &mut deferred),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            }
        }
        run_loop_step(0.02);
    }
}

/// Pendant une attente (requête SDP) : écritures et fermetures tout de suite, le reste après.
fn pump_while_waiting(receiver: &Receiver<Command>, deferred: &mut VecDeque<Command>) {
    while let Ok(command) = receiver.try_recv() {
        match command {
            Command::Write { .. } | Command::Close { .. } | Command::Powered { .. } => handle(command, receiver, deferred),
            other => deferred.push_back(other),
        }
    }
}

fn handle(command: Command, receiver: &Receiver<Command>, deferred: &mut VecDeque<Command>) {
    match command {
        Command::Powered { reply } => {
            // SAFETY : contrôleur Bluetooth de la machine.
            let on = unsafe { IOBluetoothHostController::defaultController() }
                .is_some_and(|controller| unsafe { controller.powerState() } == BluetoothHCIPowerState::ON);
            let _ = reply.send(on);
        }
        Command::Paired { reply } => {
            let _ = reply.send(paired());
        }
        Command::Listen { on_link, reply } => {
            let _ = reply.send(publish(on_link));
        }
        Command::Connect { address, reply } => {
            let _ = reply.send(open(&address, receiver, deferred).map(|link| Box::new(link) as BoxLink));
        }
        Command::Write { id, data, reply } => {
            let channel = STATE.with(|state| state.borrow().channels.get(&id).cloned());
            let result = match channel {
                Some(channel) => write_all(&channel, &data),
                None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "liaison Bluetooth fermée")),
            };
            let _ = reply.send(result);
        }
        Command::Close { id } => {
            let channel = STATE.with(|state| {
                let mut state = state.borrow_mut();
                let channel = state.channels.remove(&id);
                if let Some(channel) = &channel {
                    state.inbox.remove(&key(channel));
                }
                channel
            });
            if let Some(channel) = channel {
                // SAFETY : canal ouvert par nous ou reçu, fermé une seule fois.
                unsafe { channel.closeChannel() };
            }
        }
    }
}

fn write_all(channel: &IOBluetoothRFCOMMChannel, data: &[u8]) -> io::Result<()> {
    // SAFETY : lecture de la taille maximale d'un envoi.
    let mtu = (unsafe { channel.getMTU() } as usize).clamp(16, u16::MAX as usize);
    for piece in data.chunks(mtu) {
        // SAFETY : `piece` est valable pendant l'appel ; IOBluetooth ne le modifie pas.
        let status = unsafe { channel.writeSync_length(piece.as_ptr() as *mut c_void, piece.len() as u16) };
        if status != 0 {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, format!("envoi Bluetooth refusé (code {status:#x})")));
        }
    }
    Ok(())
}

fn paired() -> Vec<PairedDevice> {
    // SAFETY : liste des appareils appairés au système.
    let Some(devices) = (unsafe { IOBluetoothDevice::pairedDevices() }) else { return Vec::new() };
    let mut list = Vec::new();
    for device in devices.iter() {
        let Ok(device) = device.downcast::<IOBluetoothDevice>() else { continue };
        // SAFETY : lecture des propriétés d'un appareil connu.
        unsafe {
            if device.deviceClassMajor() != kBluetoothDeviceClassMajorComputer {
                continue;
            }
            let Some(address) = device.addressString() else { continue };
            let name = device.nameOrAddress().map(|n| n.to_string()).unwrap_or_default();
            list.push(PairedDevice { address: address.to_string(), name });
        }
    }
    list
}

fn service_uuid() -> Option<Retained<IOBluetoothSDPUUID>> {
    let bytes = uuid_bytes();
    // SAFETY : 16 octets valables pendant l'appel (copiés).
    unsafe { IOBluetoothSDPUUID::uuidWithBytes_length(bytes.as_ptr() as *const c_void, 16) }
}

/// N'importe quel objet vu comme `AnyObject` (toujours valable).
fn any<T: Message>(object: Retained<T>) -> Retained<AnyObject> {
    // SAFETY : tout objet Objective-C est un `AnyObject`.
    unsafe { Retained::cast_unchecked(object) }
}

fn as_any<T: Message>(object: &T) -> &AnyObject {
    // SAFETY : tout objet Objective-C est un `AnyObject`.
    unsafe { &*(object as *const T as *const AnyObject) }
}

/// Description du service publiée dans l'annuaire SDP (format des fichiers `.plist` d'Apple).
fn service_dictionary() -> Retained<NSDictionary> {
    let data = |bytes: &[u8]| any(NSData::with_bytes(bytes));
    let list = |items: Vec<Retained<AnyObject>>| any(NSArray::from_retained_slice(&items));
    let key = |text: &str| NSString::from_str(text);
    let channel_keys = [key("DataElementSize"), key("DataElementType"), key("DataElementValue")];
    let channel_element = NSDictionary::<NSString, AnyObject>::from_retained_objects(
        &channel_keys.iter().map(|k| &**k).collect::<Vec<_>>(),
        // Canal souhaité ; macOS en attribue un autre s'il est pris.
        &[any(NSNumber::new_u32(1)), any(NSNumber::new_u32(1)), any(NSNumber::new_u32(17))],
    );
    let keys = [
        key("0001 - ServiceClassIDList"),
        key("0004 - ProtocolDescriptorList"),
        key("0005 - BrowseGroupList*"),
        key("0100 - ServiceName*"),
    ];
    let dictionary = NSDictionary::<NSString, AnyObject>::from_retained_objects(
        &keys.iter().map(|k| &**k).collect::<Vec<_>>(),
        &[
            list(vec![data(&uuid_bytes())]),
            list(vec![list(vec![data(&[0x01, 0x00])]), list(vec![data(&[0x00, 0x03]), any(channel_element)])]),
            list(vec![data(&[0x10, 0x02])]),
            any(NSString::from_str("MagicPlus")),
        ],
    );
    // SAFETY : un dictionnaire aux clés `NSString` est un `NSDictionary` quelconque.
    unsafe { Retained::cast_unchecked(dictionary) }
}

fn publish(on_link: OnLink) -> Result<()> {
    let dictionary = service_dictionary();
    // SAFETY : dictionnaire de description du service ; `NSDictionary` de type quelconque.
    let record = unsafe { IOBluetoothSDPServiceRecord::publishedServiceRecordWithDictionary(Some(&dictionary)) }
    .ok_or_else(|| anyhow!("service Bluetooth non publié (Bluetooth autorisé pour MagicPlus ?)"))?;
    let mut channel_id: u8 = 0;
    // SAFETY : lecture du canal RFCOMM attribué.
    if unsafe { record.getRFCOMMChannelID(&mut channel_id) } != 0 {
        bail!("canal Bluetooth introuvable");
    }
    let delegate = delegate();
    // SAFETY : notification des canaux entrants sur ce canal, délivrée à notre délégué.
    let notification = unsafe {
        IOBluetoothRFCOMMChannel::registerForChannelOpenNotifications_selector_withChannelID_direction(
            Some(as_any(&*delegate)),
            Some(sel!(newRFCOMMChannelOpened:channel:)),
            channel_id,
            IOBluetoothUserNotificationChannelDirection::Incoming,
        )
    }
    .ok_or_else(|| anyhow!("écoute Bluetooth impossible"))?;
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.on_link = Some(on_link);
        state._service = Some(record);
        state._notification = Some(notification);
    });
    eprintln!("[bluetooth] service publié sur le canal RFCOMM {channel_id}");
    Ok(())
}

/// Canal RFCOMM de notre service chez `device` (après requête SDP si besoin).
fn remote_channel(device: &IOBluetoothDevice, fresh: bool, receiver: &Receiver<Command>, deferred: &mut VecDeque<Command>) -> Option<u8> {
    let uuid = service_uuid()?;
    if fresh {
        STATE.with(|state| state.borrow_mut().sdp_done = false);
        let uuids: Retained<NSArray> = NSArray::from_retained_slice(&[any(uuid.clone())]);
        let delegate = delegate();
        // SAFETY : requête asynchrone, réponse au délégué (`sdpQueryComplete:status:`).
        let status = unsafe { device.performSDPQuery_uuids(Some(as_any(&*delegate)), Some(&uuids)) };
        if status != 0 {
            return None;
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while !STATE.with(|state| state.borrow().sdp_done) && Instant::now() < deadline {
            pump_while_waiting(receiver, deferred);
            run_loop_step(0.05);
        }
    }
    // SAFETY : lecture de l'annuaire du périphérique (dernière requête SDP).
    let record = unsafe { device.getServiceRecordForUUID(Some(&uuid)) }?;
    let mut channel_id: u8 = 0;
    // SAFETY : lecture du canal RFCOMM de l'enregistrement.
    (unsafe { record.getRFCOMMChannelID(&mut channel_id) } == 0).then_some(channel_id)
}

fn open(address: &str, receiver: &Receiver<Command>, deferred: &mut VecDeque<Command>) -> Result<MacLink> {
    // SAFETY : appareil désigné par son adresse.
    let device = unsafe { IOBluetoothDevice::deviceWithAddressString(Some(&NSString::from_str(address))) }
        .ok_or_else(|| anyhow!("adresse Bluetooth invalide"))?;
    let delegate = delegate();
    // Canal connu (requête SDP précédente), sinon nouvelle requête ; puis un second essai frais.
    for fresh in [false, true] {
        let Some(channel_id) = remote_channel(&device, fresh, receiver, deferred) else { continue };
        let mut channel: Option<Retained<IOBluetoothRFCOMMChannel>> = None;
        // SAFETY : ouverture synchrone ; le délégué reçoit ensuite les données.
        let status = unsafe {
            device.openRFCOMMChannelSync_withChannelID_delegate(Some(&mut channel), channel_id, Some(as_any(&*delegate)))
        };
        if status == 0 {
            if let Some(channel) = channel {
                return Ok(register(channel));
            }
        }
    }
    bail!("MagicPlus introuvable sur cet appareil (éteint, hors de portée, ou MagicPlus fermé)")
}

#[allow(dead_code)]
fn class_name() -> &'static str {
    BluetoothDelegate::class().name().to_str().unwrap_or_default()
}
