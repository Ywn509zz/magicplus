//! Liaison Bluetooth (RFCOMM) : quand deux appareils n'ont pas de réseau commun (pas de Wi-Fi,
//! pas d'Internet, réseau d'école ou d'entreprise qui isole les appareils), MagicPlus passe par le
//! Bluetooth « classique », présent sur tous les ordinateurs, y compris les Mac sans Bluetooth
//! basse consommation.
//!
//! - Chaque MagicPlus publie un service RFCOMM (`SERVICE_UUID`).
//! - Toutes les 30 s, il sonde les **ordinateurs appairés au système** (réglages Bluetooth de macOS
//!   ou de Windows), sauf ceux déjà joignables par le réseau local : connexion, échange des annonces
//!   (`H`), fermeture. L'appareil apparaît alors « via Bluetooth », comme s'il était sur le réseau.
//! - Un envoi ouvre une session (`N`) puis le **même canal chiffré Noise** qu'en TCP : le lien
//!   Bluetooth n'est qu'un tuyau, la sécurité ne dépend pas de lui.
//! - Une seule connexion à la fois vers un même appareil (RFCOMM n'en permet qu'une par canal).

use crate::{
    engine::{lock, Inner, Source},
    link::{BoxLink, Link},
    protocol::Hello,
};
use anyhow::{anyhow, bail, Result};
use std::{
    collections::{HashMap, HashSet},
    io::{self, Read, Write},
    sync::{atomic::Ordering, Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

#[cfg(target_os = "macos")]
#[path = "mac.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod platform;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[path = "none.rs"]
mod platform;

/// Service RFCOMM de MagicPlus.
pub(crate) const SERVICE_UUID: u128 = 0x4d61_d1c2_8f3e_4b6a_9c51_2b7e_0a9d_3f11;
/// Un appareil sondé en Bluetooth reste « en ligne » ce temps-là.
pub(crate) const ONLINE_WINDOW: Duration = Duration::from_secs(80);
const PROBE_EVERY: Duration = Duration::from_secs(30);
const RETRY_MIN: Duration = Duration::from_secs(60);
const RETRY_MAX: Duration = Duration::from_secs(600);
const MAX_HELLO: usize = 8192;
const GATE_WAIT: Duration = Duration::from_secs(25);

/// Ordinateur appairé au système.
#[derive(Debug, Clone)]
pub(crate) struct PairedDevice {
    /// « aa-bb-cc-dd-ee-ff ».
    pub address: String,
    pub name: String,
}

/// Adresse au format commun (minuscules, tirets).
pub(crate) fn normalize(address: &str) -> String {
    address.trim().to_ascii_lowercase().replace(':', "-")
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn uuid_bytes() -> [u8; 16] {
    SERVICE_UUID.to_be_bytes()
}

// ------------------------------------------------------------------ une connexion à la fois

static BUSY: Mutex<Option<HashSet<String>>> = Mutex::new(None);
static FREED: Condvar = Condvar::new();

struct Gate(String);

impl Gate {
    fn acquire(address: &str) -> Result<Gate> {
        let deadline = Instant::now() + GATE_WAIT;
        let mut busy = BUSY.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            let set = busy.get_or_insert_with(HashSet::new);
            if set.insert(address.to_string()) {
                return Ok(Gate(address.to_string()));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("liaison Bluetooth occupée");
            }
            busy = FREED.wait_timeout(busy, left).unwrap_or_else(|p| p.into_inner()).0;
        }
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        if let Some(set) = BUSY.lock().unwrap_or_else(|p| p.into_inner()).as_mut() {
            set.remove(&self.0);
        }
        FREED.notify_all();
    }
}

/// Lien sortant qui libère l'appareil quand il est fermé.
struct Gated {
    link: BoxLink,
    _gate: Gate,
}

impl Read for Gated {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.link.read(buf)
    }
}

impl Write for Gated {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.link.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.link.flush()
    }
}

impl Link for Gated {
    fn half_close(&self) -> bool {
        self.link.half_close()
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.link.set_read_timeout(timeout)
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.link.set_write_timeout(timeout)
    }

    fn close(&self) {
        self.link.close()
    }
}

fn open(address: &str, kind: u8) -> Result<BoxLink> {
    let gate = Gate::acquire(address)?;
    let mut link = platform::connect(address)?;
    link.write_all(&[kind])?;
    link.flush()?;
    Ok(Box::new(Gated { link, _gate: gate }))
}

/// Session chiffrée vers un appareil joignable en Bluetooth (voir `Inner::connect`).
pub(crate) fn open_session(address: &str) -> Result<BoxLink> {
    open(address, b'N')
}

// ------------------------------------------------------------------ annonces

fn write_hello(link: &mut BoxLink, hello: &Hello) -> Result<()> {
    let bytes = serde_json::to_vec(hello)?;
    let mut framed = (bytes.len() as u16).to_be_bytes().to_vec();
    framed.extend_from_slice(&bytes);
    link.write_all(&framed)?;
    link.flush()?;
    Ok(())
}

fn read_hello(link: &mut BoxLink) -> Result<Hello> {
    let mut length = [0_u8; 2];
    link.read_exact(&mut length)?;
    let length = u16::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_HELLO {
        bail!("annonce Bluetooth invalide");
    }
    let mut bytes = vec![0_u8; length];
    link.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Sonde un ordinateur appairé : s'il a MagicPlus, on échange nos annonces.
fn probe(inner: &Inner, address: &str) -> Result<()> {
    probe_over(inner, open(address, b'H')?, address)
}

/// Échange des annonces sur un lien déjà ouvert (après l'octet `H`).
fn probe_over(inner: &Inner, mut link: BoxLink, address: &str) -> Result<()> {
    link.set_read_timeout(Some(Duration::from_secs(10))).ok();
    write_hello(&mut link, &inner.hello(false))?;
    let hello = read_hello(&mut link)?;
    link.close();
    if inner.on_hello(hello, Source::Bluetooth(address.to_string())).is_none() {
        bail!("annonce ignorée");
    }
    Ok(())
}

/// Connexion entrante : annonce (`H`) ou session chiffrée (`N`).
fn incoming(inner: &Arc<Inner>, mut link: BoxLink, address: String) {
    link.set_read_timeout(Some(Duration::from_secs(15))).ok();
    let mut kind = [0_u8; 1];
    if link.read_exact(&mut kind).is_err() {
        return;
    }
    match kind[0] {
        b'H' => {
            if let Ok(hello) = read_hello(&mut link) {
                inner.on_hello(hello, Source::Bluetooth(address));
                let _ = write_hello(&mut link, &inner.hello(true));
            }
            // L'autre côté ferme après avoir lu notre annonce.
            let mut rest = [0_u8; 16];
            let _ = link.read(&mut rest);
            link.close();
        }
        b'N' => inner.handle_connection(link),
        _ => link.close(),
    }
}

// ------------------------------------------------------------------ fil Bluetooth

impl Inner {
    fn set_bluetooth_status(&self, text: &str) {
        let mut status = lock(&self.bluetooth_status);
        if *status != text {
            *status = text.to_string();
            drop(status);
            self.touch();
        }
    }
}

/// Démarre le service et le sondage (sauf si `Config::bluetooth` est faux, comme dans les tests).
pub(crate) fn spawn(inner: Arc<Inner>) {
    if !inner.cfg.bluetooth || !platform::SUPPORTED {
        return;
    }
    thread::spawn(move || run(inner));
}

fn run(inner: Arc<Inner>) {
    let mut listening = false;
    let mut next: HashMap<String, (Instant, Duration)> = HashMap::new();
    let mut last_listen_try: Option<Instant> = None;
    while !inner.shutdown.load(Ordering::SeqCst) {
        if !platform::powered() {
            inner.set_bluetooth_status("Bluetooth éteint : allumez-le pour joindre vos appareils sans réseau commun.");
            thread::sleep(Duration::from_secs(10));
            continue;
        }
        if !listening && last_listen_try.is_none_or(|at| at.elapsed() > RETRY_MIN) {
            last_listen_try = Some(Instant::now());
            let handler_inner = inner.clone();
            match platform::listen(Box::new(move |link, address| {
                let inner = handler_inner.clone();
                thread::spawn(move || incoming(&inner, link, normalize(&address)));
            })) {
                Ok(()) => listening = true,
                Err(error) => {
                    eprintln!("[bluetooth] service indisponible : {error}");
                    inner.set_bluetooth_status(&format!("Bluetooth indisponible : {error}"));
                }
            }
        }
        if listening {
            inner.set_bluetooth_status(
                "Bluetooth actif : sans réseau commun, vos ordinateurs appairés en Bluetooth (réglages du système) \
                 et qui ont MagicPlus restent joignables.",
            );
        }

        let devices = platform::paired_computers();
        for device in devices {
            if inner.shutdown.load(Ordering::SeqCst) {
                return;
            }
            let address = normalize(&device.address);
            // Déjà joignable par le réseau local : inutile de passer par le Bluetooth.
            let on_network = lock(&inner.peers)
                .values()
                .any(|peer| peer.udp_online() && peer.bluetooth.as_ref().is_some_and(|(known, _)| *known == address));
            if on_network {
                continue;
            }
            let (when, backoff) = next.get(&address).copied().unwrap_or((Instant::now(), RETRY_MIN));
            if Instant::now() < when {
                continue;
            }
            match probe(&inner, &address) {
                Ok(()) => {
                    next.insert(address, (Instant::now() + PROBE_EVERY, RETRY_MIN));
                }
                Err(error) => {
                    eprintln!("[bluetooth] {} ({address}) : {error}", device.name);
                    next.insert(address, (Instant::now() + backoff, (backoff * 2).min(RETRY_MAX)));
                }
            }
        }
        thread::sleep(Duration::from_secs(5));
    }
}

/// Erreur de plateforme lisible.
#[allow(dead_code)]
pub(crate) fn failure(what: &str, code: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{what} (code {code})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{read_ack, Config, Engine},
        protocol::Message,
        secure,
    };
    use std::net::{TcpListener, TcpStream};

    /// Lien sans demi-fermeture, comme RFCOMM (mais sur TCP pour l'essai).
    struct NoHalfClose(TcpStream);

    impl Read for NoHalfClose {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Write for NoHalfClose {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    impl Link for NoHalfClose {
        fn half_close(&self) -> bool {
            false
        }

        fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(timeout)
        }

        fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_write_timeout(timeout)
        }

        fn close(&self) {
            let _ = self.0.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Lien client et lien serveur reliés.
    fn pair() -> (BoxLink, BoxLink) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (Box::new(NoHalfClose(client)), Box::new(NoHalfClose(server)))
    }

    fn engine(label: &str, port: u16) -> Engine {
        let dir = std::env::temp_dir().join(format!("magicplus-bt-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Engine::start(Config::new(dir, port, port + 1)).unwrap()
    }

    #[test]
    fn announces_and_encrypted_session_over_a_bluetooth_like_link() {
        let (a, b) = (engine("a", 46810), engine("b", 46812));
        a.set_device_name("Portable A").unwrap();
        b.set_device_name("Portable B").unwrap();

        // Sondage : A ouvre (« H ») et échange les annonces avec B.
        let (mut client, server) = pair();
        let b_inner = b.inner.clone();
        let responder = thread::spawn(move || incoming(&b_inner, server, "AA:AA:AA:AA:AA:02".to_string()));
        client.write_all(b"H").unwrap();
        probe_over(&a.inner, client, "bb-bb-bb-bb-bb-01").unwrap();
        responder.join().unwrap();
        let seen_by_a = a.snapshot().peers.into_iter().find(|p| p.name == "Portable B").expect("A voit B");
        assert!(seen_by_a.online && seen_by_a.address == "via Bluetooth", "{seen_by_a:?}");
        assert!(b.snapshot().peers.iter().any(|p| p.name == "Portable A" && p.online));
        let route = lock(&a.inner.peers).get(&seen_by_a.id).and_then(|peer| peer.route());
        assert_eq!(route, Some(crate::engine::Route::Bluetooth("bb-bb-bb-bb-bb-01".to_string())));

        // Session chiffrée (« N ») : fin d'envoi par la marque de longueur nulle.
        b.trust(&b.snapshot().peers.iter().find(|p| p.name == "Portable A").unwrap().id).unwrap();
        let (mut client, server) = pair();
        let b_inner = b.inner.clone();
        let receiver = thread::spawn(move || incoming(&b_inner, server, "aa-aa-aa-aa-aa-02".to_string()));
        client.write_all(b"N").unwrap();
        let b_key = crate::crypto::public_from_b64(&lock(&a.inner.peers).get(&seen_by_a.id).unwrap().device.public_key).unwrap();
        let mut channel = secure::connect(client, &a.inner.secret.to_bytes(), b_key.as_bytes(), &a.inner.own_id()).unwrap();
        let message = Message::ClipboardText { id: "t1".to_string(), text: "par Bluetooth".to_string() };
        a.inner.write_message(&mut channel, &message, &[]).unwrap();
        channel.finish().unwrap();
        let ack = read_ack(&mut channel).expect("accusé de B");
        assert!(ack.ok, "{ack:?}");
        receiver.join().unwrap();
        assert_eq!(b.snapshot().clipboard.map(|c| c.text).as_deref(), Some("par Bluetooth"));
        a.shutdown();
        b.shutdown();
    }

    #[test]
    fn gate_serialises_connections_to_one_device() {
        let first = Gate::acquire("aa-bb").unwrap();
        let other = Gate::acquire("cc-dd").unwrap();
        let waiter = thread::spawn(|| {
            let started = Instant::now();
            let _gate = Gate::acquire("aa-bb").unwrap();
            started.elapsed()
        });
        thread::sleep(Duration::from_millis(300));
        drop(first);
        assert!(waiter.join().unwrap() >= Duration::from_millis(250));
        drop(other);
        assert_eq!(normalize("AA:BB:CC:00:11:22"), "aa-bb-cc-00-11-22");
        assert_eq!(uuid_bytes()[0], 0x4d);
    }
}
