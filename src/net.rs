//! Réseau local : annonces UDP, écoute TCP et trames binaires.

use crate::{
    engine::{Engine, Inner},
    protocol::*,
};
use std::{
    collections::HashMap,
    io,
    net::{Ipv4Addr, TcpListener, UdpSocket},
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};

const BROADCAST_INTERVAL: Duration = Duration::from_secs(2);
const BIND_RETRY_DELAY: Duration = Duration::from_secs(3);

pub(crate) fn spawn(engine: &Engine) {
    let inner = engine.inner.clone();
    spawn_udp_listener(inner.clone());
    spawn_udp_broadcaster(inner.clone());
    spawn_tcp_listener(inner);
}

// ------------------------------------------------------------------ découverte

/// Envoie tout de suite notre annonce à ces adresses (sans attendre le prochain tour).
pub(crate) fn send_hello(inner: &Inner, targets: &[std::net::SocketAddr]) {
    let Ok(encoded) = serde_json::to_vec(&inner.hello(false)) else { return };
    if let Ok(socket) = UdpSocket::bind(("0.0.0.0", 0)) {
        for target in targets {
            let _ = socket.send_to(&encoded, target);
        }
    }
}

fn spawn_udp_listener(inner: Arc<Inner>) {
    thread::spawn(move || {
        // Le port peut rester occupé un instant (instance précédente en train de se fermer) :
        // on réessaie au lieu d'abandonner pour toute la session.
        let socket = loop {
            if inner.shutdown.load(Ordering::SeqCst) {
                return;
            }
            match UdpSocket::bind(("0.0.0.0", inner.cfg.discovery_port)) {
                Ok(socket) => break socket,
                Err(error) => {
                    inner.set_status(format!(
                        "Découverte : le port UDP {} est occupé ({error}), nouvelle tentative…",
                        inner.cfg.discovery_port
                    ));
                    thread::sleep(BIND_RETRY_DELAY);
                }
            }
        };
        socket.set_read_timeout(Some(Duration::from_millis(500))).ok();

        let mut buffer = [0_u8; 4096];
        let mut last_reply: HashMap<String, Instant> = HashMap::new();
        while !inner.shutdown.load(Ordering::SeqCst) {
            let (size, source) = match socket.recv_from(&mut buffer) {
                Ok(result) => result,
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
                Err(error) => {
                    eprintln!("[réseau] lecture UDP impossible : {error}");
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            let Ok(hello) = serde_json::from_slice::<Hello>(&buffer[..size]) else {
                continue;
            };
            let reply_port = hello.discovery_port;
            let is_reply = hello.reply;
            let peer_id = hello.device.id.clone();
            let name = hello.device.name.clone();
            let Some(announce) = inner.on_hello(hello, crate::engine::Source::Udp(source.ip())) else {
                continue;
            };
            if announce {
                eprintln!("[découverte] {name} détecté à {}", source.ip());
            }
            // Réponse directe à chaque annonce (au plus une par seconde et par appareil), sauf aux
            // réponses elles-mêmes. Ainsi un appareil qui ne nous joint qu'en direct (adresse saisie
            // à la main, diffusion bloquée par le réseau) continue de nous voir en ligne.
            if !is_reply && last_reply.get(&peer_id).is_none_or(|at| at.elapsed() >= Duration::from_secs(1)) {
                last_reply.insert(peer_id, Instant::now());
                if let Ok(bytes) = serde_json::to_vec(&inner.hello(true)) {
                    if let Ok(reply) = UdpSocket::bind(("0.0.0.0", 0)) {
                        let _ = reply.send_to(&bytes, (source.ip(), reply_port));
                    }
                }
            }
            if last_reply.len() > 256 {
                last_reply.retain(|_, at| at.elapsed() < Duration::from_secs(60));
            }
        }
    });
}

fn spawn_udp_broadcaster(inner: Arc<Inner>) {
    thread::spawn(move || {
        let mut warned = false;
        while !inner.shutdown.load(Ordering::SeqCst) {
            let port = inner.cfg.discovery_port;
            let mut sent = 0;

            // Tout ce qui est lent (liste des interfaces, sockets, résolution des adresses
            // saisies) est préparé AVANT de lire l'annonce. Sinon, un renommage pendant ce temps
            // (plusieurs millisecondes sous Windows, avec ses interfaces Hyper-V, WSL ou Docker)
            // ferait partir l'ancien nom après une réponse portant le nouveau : les autres
            // appareils réafficheraient l'ancien nom jusqu'au tour suivant.

            // Un socket par interface, lié à son adresse : l'annonce part par cette interface
            // (un socket unique n'utilise que la route par défaut, souvent le mauvais réseau
            // sous Windows avec VPN, Hyper-V ou WSL). La liste est relue pour suivre le Wi-Fi.
            let mut per_interface = Vec::new();
            if let Ok(interfaces) = if_addrs::get_if_addrs() {
                for interface in interfaces {
                    if interface.is_loopback() {
                        continue;
                    }
                    let if_addrs::IfAddr::V4(address) = interface.addr else {
                        continue;
                    };
                    if address.ip.is_unspecified() {
                        continue;
                    }
                    let subnet = address
                        .broadcast
                        .unwrap_or_else(|| Ipv4Addr::from(u32::from(address.ip) | !u32::from(address.netmask)));
                    let Ok(socket) = UdpSocket::bind((address.ip, 0)) else {
                        continue;
                    };
                    if socket.set_broadcast(true).is_err() {
                        continue;
                    }
                    per_interface.push((socket, address.ip, subnet));
                }
            }

            // Adresses saisies par l'utilisateur (réseau qui bloque la diffusion, autre
            // sous-réseau) et adresses de test : annonce envoyée directement à chacune.
            let mut direct = inner.cfg.extra_targets.clone();
            direct.extend(inner.manual_targets());
            let direct_socket = if direct.is_empty() { None } else { UdpSocket::bind(("0.0.0.0", 0)).ok() };

            // Relu à chaque tour, juste avant d'envoyer : le nom de l'appareil peut changer.
            let Ok(encoded) = serde_json::to_vec(&inner.hello(false)) else {
                return;
            };

            for (socket, ip, subnet) in &per_interface {
                let mut delivered = false;
                for target in [*subnet, Ipv4Addr::BROADCAST] {
                    if target != *ip && socket.send_to(&encoded, (target, port)).is_ok() {
                        delivered = true;
                    }
                }
                if delivered {
                    sent += 1;
                }
            }

            if sent == 0 {
                if let Ok(socket) = UdpSocket::bind(("0.0.0.0", 0)) {
                    if socket.set_broadcast(true).is_ok()
                        && socket.send_to(&encoded, (Ipv4Addr::BROADCAST, port)).is_ok()
                    {
                        sent += 1;
                    }
                }
            }

            if let Some(socket) = &direct_socket {
                for target in &direct {
                    if socket.send_to(&encoded, target).is_ok() {
                        sent += 1;
                    }
                }
            }

            if sent == 0 && !warned {
                warned = true;
                inner.set_status("Aucune interface réseau n'a pu émettre : le Wi-Fi ou l'Ethernet est-il connecté ?");
            } else if sent > 0 {
                warned = false;
            }
            thread::sleep(BROADCAST_INTERVAL);
        }
    });
}

// ------------------------------------------------------------------ transfert

fn spawn_tcp_listener(inner: Arc<Inner>) {
    thread::spawn(move || {
        let listener = loop {
            if inner.shutdown.load(Ordering::SeqCst) {
                return;
            }
            match TcpListener::bind(("0.0.0.0", inner.cfg.transfer_port)) {
                Ok(listener) => break listener,
                Err(error) => {
                    inner.set_status(format!(
                        "Transfert : le port TCP {} est occupé ({error}), nouvelle tentative…",
                        inner.cfg.transfer_port
                    ));
                    thread::sleep(BIND_RETRY_DELAY);
                }
            }
        };
        listener.set_nonblocking(true).ok();

        while !inner.shutdown.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).ok();
                    let inner = inner.clone();
                    thread::spawn(move || inner.handle_connection(Box::new(stream)));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(50)),
                Err(error) => {
                    eprintln!("[réseau] connexion refusée : {error}");
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }
    });
}
