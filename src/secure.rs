//! Canal chiffré entre deux appareils : protocole Noise (motif IK), comme WireGuard.
//!
//! `Noise_IK_25519_ChaChaPoly_BLAKE2s` :
//! - **poignée de main** en un aller-retour : l'initiateur connaît déjà la clé publique du
//!   destinataire (appareil associé) ; chacun prouve qu'il détient sa clé secrète, et des clés
//!   **éphémères** sont échangées. Les clés de session en dérivent (BLAKE2s) : voler plus tard la clé
//!   secrète d'un appareil ne permet pas de déchiffrer les conversations enregistrées (confidentialité
//!   persistante).
//! - **transport** : ChaCha20-Poly1305 avec un compteur implicite par sens. Un message modifié,
//!   rejoué, supprimé ou réordonné fait échouer le déchiffrement : la connexion est coupée.
//! - Rejouer une ancienne poignée de main ne sert à rien : le destinataire répond avec une nouvelle
//!   clé éphémère, et l'attaquant ne peut pas calculer les clés de session.
//!
//! Sur le fil : messages Noise préfixés par leur longueur (`u16`, 65 535 octets au plus). Au-dessus,
//! un flux d'octets déchiffrés où chaque message applicatif est précédé de sa longueur (`u32`).

use crate::protocol::{MAX_FRAME_BYTES, PROTOCOL};
use anyhow::{anyhow, bail, Context, Result};
use std::io::{self, Read, Write};

const NOISE_PARAMS: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";
const NOISE_MAX: usize = 65_535;
const TAG: usize = 16;
/// Clair maximal par message Noise.
const PIECE: usize = NOISE_MAX - TAG;
/// L'identifiant envoyé dans la poignée de main est court.
const MAX_ID: usize = 64;

/// Flux chiffré et authentifié au-dessus d'un flux d'octets (TCP, et demain Bluetooth).
pub(crate) struct SecureStream<S: Read + Write> {
    stream: S,
    transport: snow::TransportState,
    /// Octets déjà déchiffrés mais pas encore lus.
    inbox: Vec<u8>,
    remote_static: [u8; 32],
}

fn params() -> snow::params::NoiseParams {
    NOISE_PARAMS.parse().expect("paramètres Noise valides")
}

fn prologue() -> Vec<u8> {
    format!("{PROTOCOL}/noise").into_bytes()
}

fn write_noise(stream: &mut impl Write, message: &[u8]) -> io::Result<()> {
    let mut framed = Vec::with_capacity(2 + message.len());
    framed.extend_from_slice(&(message.len() as u16).to_be_bytes());
    framed.extend_from_slice(message);
    stream.write_all(&framed)
}

/// Lit un message Noise. `Ok(None)` si le flux est fermé proprement avant le message.
fn read_noise(stream: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut length = [0_u8; 2];
    match stream.read(&mut length[..1])? {
        0 => return Ok(None),
        _ => stream.read_exact(&mut length[1..])?,
    }
    let length = u16::from_be_bytes(length) as usize;
    let mut message = vec![0_u8; length];
    stream.read_exact(&mut message)?;
    Ok(Some(message))
}

/// Ouvre un canal vers un appareil dont on connaît la clé publique (initiateur).
pub(crate) fn connect<S: Read + Write>(
    mut stream: S,
    local_secret: &[u8; 32],
    remote_public: &[u8; 32],
    local_id: &str,
) -> Result<SecureStream<S>> {
    let prologue = prologue();
    let mut handshake = snow::Builder::new(params())
        .local_private_key(local_secret)
        .remote_public_key(remote_public)
        .prologue(&prologue)
        .build_initiator()
        .map_err(|error| anyhow!("chiffrement : {error}"))?;

    let mut buffer = vec![0_u8; NOISE_MAX];
    let length = handshake
        .write_message(local_id.as_bytes(), &mut buffer)
        .map_err(|error| anyhow!("chiffrement : {error}"))?;
    write_noise(&mut stream, &buffer[..length]).context("connexion interrompue")?;

    let reply = read_noise(&mut stream)
        .context("connexion interrompue")?
        .ok_or_else(|| anyhow!("l'appareil a fermé la connexion pendant la négociation"))?;
    handshake
        .read_message(&reply, &mut buffer)
        .map_err(|_| anyhow!("négociation refusée : ce n'est pas l'appareil associé (clé différente)"))?;

    let transport = handshake.into_transport_mode().map_err(|error| anyhow!("chiffrement : {error}"))?;
    Ok(SecureStream { stream, transport, inbox: Vec::new(), remote_static: *remote_public })
}

/// Accepte un canal entrant (répondeur). Retourne aussi l'identifiant annoncé par l'initiateur ;
/// sa clé publique, elle, est prouvée par la poignée de main (`remote_static`).
pub(crate) fn accept<S: Read + Write>(mut stream: S, local_secret: &[u8; 32]) -> Result<(SecureStream<S>, String)> {
    let prologue = prologue();
    let mut handshake = snow::Builder::new(params())
        .local_private_key(local_secret)
        .prologue(&prologue)
        .build_responder()
        .map_err(|error| anyhow!("chiffrement : {error}"))?;

    let first = read_noise(&mut stream)
        .context("connexion interrompue")?
        .ok_or_else(|| anyhow!("connexion fermée avant la négociation"))?;
    let mut buffer = vec![0_u8; NOISE_MAX];
    let length = handshake
        .read_message(&first, &mut buffer)
        .map_err(|_| anyhow!("négociation invalide (autre version de MagicPlus ?)"))?;
    if length > MAX_ID {
        bail!("négociation invalide");
    }
    let remote_id = String::from_utf8(buffer[..length].to_vec()).map_err(|_| anyhow!("négociation invalide"))?;
    let remote_static: [u8; 32] = handshake
        .get_remote_static()
        .and_then(|key| key.try_into().ok())
        .ok_or_else(|| anyhow!("négociation invalide"))?;

    let length = handshake.write_message(&[], &mut buffer).map_err(|error| anyhow!("chiffrement : {error}"))?;
    write_noise(&mut stream, &buffer[..length]).context("connexion interrompue")?;
    let transport = handshake.into_transport_mode().map_err(|error| anyhow!("chiffrement : {error}"))?;
    Ok((SecureStream { stream, transport, inbox: Vec::new(), remote_static }, remote_id))
}

impl<S: Read + Write> SecureStream<S> {
    /// Clé publique de l'autre appareil, prouvée par la poignée de main.
    pub(crate) fn remote_static(&self) -> &[u8; 32] {
        &self.remote_static
    }

    pub(crate) fn get_ref(&self) -> &S {
        &self.stream
    }

    /// Envoie un message (chiffré, découpé en messages Noise de 64 Ko au plus).
    pub(crate) fn send(&mut self, plain: &[u8]) -> Result<()> {
        if plain.len() > MAX_FRAME_BYTES {
            bail!("message trop volumineux");
        }
        let mut data = Vec::with_capacity(4 + plain.len());
        data.extend_from_slice(&(plain.len() as u32).to_be_bytes());
        data.extend_from_slice(plain);

        let mut output = Vec::with_capacity(data.len() + data.len() / PIECE * (TAG + 2) + TAG + 2);
        let mut sealed = vec![0_u8; NOISE_MAX];
        for piece in data.chunks(PIECE) {
            let length = self
                .transport
                .write_message(piece, &mut sealed)
                .map_err(|error| anyhow!("chiffrement : {error}"))?;
            output.extend_from_slice(&(length as u16).to_be_bytes());
            output.extend_from_slice(&sealed[..length]);
        }
        self.stream.write_all(&output)?;
        Ok(())
    }

    /// Lit et déchiffre au moins `wanted` octets dans la boîte de réception.
    /// `Ok(false)` si le flux est fermé proprement alors que la boîte est vide.
    fn fill(&mut self, wanted: usize) -> Result<bool> {
        let mut plain = vec![0_u8; NOISE_MAX];
        while self.inbox.len() < wanted {
            let Some(message) = read_noise(&mut self.stream)? else {
                if self.inbox.is_empty() {
                    return Ok(false);
                }
                bail!("connexion coupée au milieu d'un message");
            };
            let length = self
                .transport
                .read_message(&message, &mut plain)
                .map_err(|_| anyhow!("message altéré, rejoué ou mal chiffré : connexion coupée"))?;
            self.inbox.extend_from_slice(&plain[..length]);
        }
        Ok(true)
    }

    /// Reçoit un message. `Ok(None)` si l'autre côté a fermé proprement entre deux messages.
    pub(crate) fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        if !self.fill(4)? {
            return Ok(None);
        }
        let length = u32::from_be_bytes([self.inbox[0], self.inbox[1], self.inbox[2], self.inbox[3]]) as usize;
        if length > MAX_FRAME_BYTES {
            bail!("message trop volumineux");
        }
        if !self.fill(4 + length)? {
            bail!("connexion coupée au milieu d'un message");
        }
        let message = self.inbox[4..4 + length].to_vec();
        self.inbox.drain(..4 + length);
        Ok(Some(message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto;
    use std::{
        net::{TcpListener, TcpStream},
        thread,
    };

    fn keys() -> ([u8; 32], [u8; 32]) {
        let secret = crypto::generate_keypair();
        (secret.to_bytes(), *secret.public_key().as_bytes())
    }

    /// Un octet-flux en mémoire : ce que l'un écrit, l'autre le lit.
    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn handshake_and_messages_both_ways() {
        let (alice_secret, _) = keys();
        let (bob_secret, bob_public) = keys();
        let alice_public = *crypto::secret_from_b64(&crypto::encode_key(&alice_secret)).unwrap().public_key().as_bytes();
        let (client, server) = tcp_pair();

        let bob = thread::spawn(move || {
            let (mut channel, id) = accept(server, &bob_secret).unwrap();
            assert_eq!(id, "alice");
            let big = channel.recv().unwrap().unwrap();
            channel.send(b"bien recu").unwrap();
            (big, *channel.remote_static())
        });
        let mut channel = connect(client, &alice_secret, &bob_public, "alice").unwrap();
        let payload: Vec<u8> = (0..300_000u32).map(|n| (n % 253) as u8).collect();
        channel.send(&payload).unwrap();
        assert_eq!(channel.recv().unwrap().unwrap(), b"bien recu");
        let (received, seen_key) = bob.join().unwrap();
        assert_eq!(received, payload);
        // La clé prouvée par Noise est bien celle que MagicPlus mémorise à l'association.
        assert_eq!(seen_key, alice_public);
    }

    #[test]
    fn wrong_recipient_key_is_detected() {
        let (alice_secret, _) = keys();
        let (bob_secret, _) = keys();
        let (_, mallory_public) = keys();
        let (client, server) = tcp_pair();
        let bob = thread::spawn(move || accept(server, &bob_secret).is_err());
        // Alice croit parler à « Mallory » : Bob ne peut pas lire la poignée de main.
        assert!(connect(client, &alice_secret, &mallory_public, "alice").is_err());
        assert!(bob.join().unwrap());
    }

    /// Enregistre ce qu'un côté écrit, pour le rejouer ou l'altérer.
    #[derive(Default)]
    struct Tape {
        written: Vec<u8>,
        to_read: io::Cursor<Vec<u8>>,
    }
    impl Read for Tape {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.to_read.read(buffer)
        }
    }
    impl Write for Tape {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Fait une poignée de main en mémoire et renvoie (canal d'Alice, canal de Bob).
    fn paired_channels() -> (SecureStream<Tape>, SecureStream<Tape>) {
        let (alice_secret, _) = keys();
        let (bob_secret, bob_public) = keys();
        // 1) Alice écrit le premier message.
        let mut alice_hs = snow::Builder::new(params())
            .local_private_key(&alice_secret)
            .remote_public_key(&bob_public)
            .prologue(&prologue())
            .build_initiator()
            .unwrap();
        let mut buffer = vec![0_u8; NOISE_MAX];
        let mut first = Tape::default();
        let length = alice_hs.write_message(b"alice", &mut buffer).unwrap();
        write_noise(&mut first, &buffer[..length]).unwrap();
        // 2) Bob répond.
        let tape = Tape { written: Vec::new(), to_read: io::Cursor::new(first.written) };
        let (bob, _) = accept(tape, &bob_secret).unwrap();
        let reply = bob.stream.written.clone();
        // 3) Alice lit la réponse.
        let mut cursor = io::Cursor::new(reply);
        let message = read_noise(&mut cursor).unwrap().unwrap();
        alice_hs.read_message(&message, &mut buffer).unwrap();
        let alice = SecureStream {
            stream: Tape::default(),
            transport: alice_hs.into_transport_mode().unwrap(),
            inbox: Vec::new(),
            remote_static: bob_public,
        };
        let mut bob = bob;
        bob.stream.written.clear();
        (alice, bob)
    }

    fn deliver(from: &mut SecureStream<Tape>, to: &mut SecureStream<Tape>, bytes: Vec<u8>) -> Result<Option<Vec<u8>>> {
        from.stream.written.clear();
        to.stream.to_read = io::Cursor::new(bytes);
        to.recv()
    }

    #[test]
    fn tampering_is_detected() {
        let (mut alice, mut bob) = paired_channels();
        alice.send(b"virement de 10 euros").unwrap();
        let mut wire = alice.stream.written.clone();
        let last = wire.len() - 1;
        wire[last] ^= 1;
        assert!(deliver(&mut alice, &mut bob, wire).is_err());
    }

    #[test]
    fn replay_is_detected() {
        let (mut alice, mut bob) = paired_channels();
        alice.send(b"premier").unwrap();
        let wire = alice.stream.written.clone();
        assert_eq!(deliver(&mut alice, &mut bob, wire.clone()).unwrap().unwrap(), b"premier");
        // Le même message renvoyé une seconde fois est refusé.
        assert!(deliver(&mut alice, &mut bob, wire).is_err());
    }

    #[test]
    fn each_connection_uses_fresh_keys() {
        // Mêmes appareils, même texte : deux connexions ne produisent jamais le même chiffré,
        // car chaque poignée de main tire de nouvelles clés éphémères (confidentialité persistante).
        let (mut first, _) = paired_channels();
        let (mut second, _) = paired_channels();
        first.send(b"meme message").unwrap();
        second.send(b"meme message").unwrap();
        assert_ne!(first.stream.written, second.stream.written);
    }

    /// Débit du chiffrement seul (chiffrer + déchiffrer), en version optimisée :
    /// `cargo test --release --lib debit_du_chiffrement -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn debit_du_chiffrement() {
        let (mut alice, mut bob) = paired_channels();
        let block = vec![7_u8; 256 * 1024];
        let total: usize = 256 * 1024 * 1024;
        let start = std::time::Instant::now();
        let mut done = 0;
        while done < total {
            alice.send(&block).unwrap();
            let wire = std::mem::take(&mut alice.stream.written);
            bob.stream.to_read = io::Cursor::new(wire);
            assert_eq!(bob.recv().unwrap().unwrap().len(), block.len());
            done += block.len();
        }
        let seconds = start.elapsed().as_secs_f64();
        println!(
            "chiffrement + déchiffrement : {} Mo en {:.2} s = {:.0} Mo/s",
            total / 1024 / 1024,
            seconds,
            total as f64 / 1024.0 / 1024.0 / seconds
        );
    }

    #[test]
    fn content_is_not_readable_on_the_wire() {
        let (mut alice, _bob) = paired_channels();
        alice.send(b"mot de passe tres secret").unwrap();
        let wire = &alice.stream.written;
        assert!(!wire.windows(6).any(|window| window == b"secret"));
    }
}
