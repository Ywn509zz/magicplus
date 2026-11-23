//! Deux moteurs complets dans le même processus, sur le réseau local de cette machine.

use magicplus_core::{Config, Engine, Snapshot, TransferState};
use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU16, Ordering},
    thread,
    time::{Duration, Instant},
};

static NEXT_PORT: AtomicU16 = AtomicU16::new(47100);
const CHUNK: usize = 256 * 1024;

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("magicplus-e2e-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn wait_for(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("délai dépassé : {what}");
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn config(dir: &Path, discovery: u16, transfer: u16, extra: Vec<SocketAddr>) -> Config {
    let mut config = Config::new(dir.join("data"), discovery, transfer);
    config.extra_targets = extra;
    config.retry_delay = Duration::from_millis(500);
    config
}

struct Pair {
    a: Engine,
    b: Engine,
    a_dir: PathBuf,
    b_dir: PathBuf,
    b_config: Config,
}

/// Deux moteurs qui se voient par l'adresse de boucle locale. `tweak` ajuste les configurations.
fn start_pair(label: &str, tweak: impl Fn(&mut Config, &mut Config)) -> Pair {
    let base = NEXT_PORT.fetch_add(4, Ordering::SeqCst);
    let (a_disc, a_tcp, b_disc, b_tcp) = (base, base + 1, base + 2, base + 3);
    let a_dir = temp_dir(&format!("{label}-a"));
    let b_dir = temp_dir(&format!("{label}-b"));
    let mut a_config = config(&a_dir, a_disc, a_tcp, vec![loopback(b_disc)]);
    let mut b_config = config(&b_dir, b_disc, b_tcp, vec![loopback(a_disc)]);
    tweak(&mut a_config, &mut b_config);

    let a = Engine::start(a_config).unwrap();
    let b = Engine::start(b_config.clone()).unwrap();
    a.set_device_name("Machine A").unwrap();
    b.set_device_name("Machine B").unwrap();
    // Jamais le vrai dossier Téléchargements pendant les tests.
    for (engine, dir) in [(&a, &a_dir), (&b, &b_dir)] {
        let inbox = dir.join("inbox").to_string_lossy().to_string();
        engine.update_settings(|s| s.save_folder = Some(inbox)).unwrap();
    }
    Pair { a, b, a_dir, b_dir, b_config }
}

fn peer_id_of(engine: &Engine, name: &str) -> Option<String> {
    engine.snapshot().peers.into_iter().find(|peer| peer.name == name && peer.online).map(|peer| peer.id)
}

fn see_each_other(pair: &Pair) {
    wait_for("A voit B", Duration::from_secs(10), || peer_id_of(&pair.a, "Machine B").is_some());
    wait_for("B voit A", Duration::from_secs(10), || peer_id_of(&pair.b, "Machine A").is_some());
}

fn pair_up(pair: &Pair) {
    see_each_other(pair);
    pair.a.trust(&peer_id_of(&pair.a, "Machine B").unwrap()).unwrap();
    pair.b.trust(&peer_id_of(&pair.b, "Machine A").unwrap()).unwrap();
}

fn transfer_state(snapshot: &Snapshot, name: &str, sending: bool) -> Option<TransferState> {
    snapshot
        .transfers
        .iter()
        .find(|t| t.file_name == name && t.sending == sending)
        .map(|t| t.state.clone())
}

fn sample(size: usize) -> Vec<u8> {
    (0..size as u32).map(|n| (n % 251) as u8).collect()
}

/// Taille du fichier partiel en cours de réception (0 s'il n'y en a pas).
fn partial_size(data_dir: &Path) -> u64 {
    fs::read_dir(data_dir.join("partiels"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "part"))
                .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

#[test]
fn discovery_pairing_and_files_on_disk() {
    let pair = start_pair("main", |_, _| {});
    see_each_other(&pair);
    let seen = pair.a.snapshot().peers.into_iter().find(|p| p.name == "Machine B").unwrap();
    assert_eq!(seen.fingerprint.len(), 19);
    assert!(!seen.trusted);

    pair_up(&pair);
    assert!(pair.a.snapshot().peers.iter().any(|p| p.name == "Machine B" && p.trusted));

    // ~3 morceaux, écrits directement sur le disque, dans le dossier de réception.
    let source = pair.a_dir.join("bonjour é.txt");
    let content = sample(700_000);
    fs::write(&source, &content).unwrap();
    pair.a.send_files(vec![source.clone()]);
    wait_for("réception", Duration::from_secs(20), || {
        transfer_state(&pair.b.snapshot(), "bonjour é.txt", false) == Some(TransferState::Complete)
    });
    wait_for("envoi confirmé", Duration::from_secs(20), || {
        transfer_state(&pair.a.snapshot(), "bonjour é.txt", true) == Some(TransferState::Complete)
    });
    let received = pair.b.snapshot().received;
    assert_eq!(received.len(), 1);
    let inbox = pair.b_dir.join("inbox");
    assert!(received[0].path.starts_with(&inbox));
    assert_eq!(received[0].path.file_name().unwrap(), "bonjour é.txt");
    assert_eq!(fs::read(&received[0].path).unwrap(), content);
    assert_eq!(partial_size(&pair.b_dir.join("data")), 0, "plus de fichier partiel");

    // Deuxième envoi du même nom : pas d'écrasement.
    pair.a.send_files(vec![source]);
    wait_for("second fichier", Duration::from_secs(20), || fs::read_dir(&inbox).unwrap().count() == 2);

    pair.a.shutdown();
    pair.b.shutdown();
}

#[test]
fn one_way_link_stays_online() {
    // A ne joint B qu'en direct (comme une adresse saisie à la main) ; B ne connaît pas A et ses
    // diffusions n'arrivent pas chez A. Avant la correction, A voyait B 8 s puis le perdait.
    let pair = start_pair("sens-unique", |_, b| b.extra_targets.clear());
    see_each_other(&pair);
    thread::sleep(Duration::from_secs(12));
    assert!(peer_id_of(&pair.a, "Machine B").is_some(), "A doit toujours voir B en ligne");
    assert!(peer_id_of(&pair.b, "Machine A").is_some(), "B doit toujours voir A en ligne");
    pair.a.shutdown();
    pair.b.shutdown();
}

#[test]
fn unpaired_receiver_explains_then_delivers_once_paired() {
    let pair = start_pair("refus", |_, _| {});
    see_each_other(&pair);

    // A associe B, mais B n'a PAS associé A.
    pair.a.trust(&peer_id_of(&pair.a, "Machine B").unwrap()).unwrap();
    let source = pair.a_dir.join("secret.bin");
    fs::write(&source, sample(1000)).unwrap();
    pair.a.send_files(vec![source]);

    // A affiche pourquoi (en pause, il retentera) ; B signale la demande d'association.
    wait_for("refus expliqué côté A", Duration::from_secs(20), || {
        matches!(transfer_state(&pair.a.snapshot(), "secret.bin", true),
            Some(TransferState::Paused(reason)) if reason.contains("cliquez sur Associer"))
    });
    wait_for("demande d'association côté B", Duration::from_secs(5), || {
        pair.b.snapshot().peers.iter().any(|p| p.name == "Machine A" && p.wants_pairing)
    });
    assert!(pair.b.snapshot().received.is_empty());

    // B associe A : l'envoi repart tout seul et arrive.
    pair.b.trust(&peer_id_of(&pair.b, "Machine A").unwrap()).unwrap();
    wait_for("livré après association", Duration::from_secs(30), || {
        transfer_state(&pair.a.snapshot(), "secret.bin", true) == Some(TransferState::Complete)
    });
    assert!(!pair.b.snapshot().peers.iter().any(|p| p.wants_pairing));
    pair.a.shutdown();
    pair.b.shutdown();
}

#[test]
fn clipboard_text_and_refusal_are_reported() {
    let pair = start_pair("texte", |_, _| {});
    see_each_other(&pair);
    if pair.a.write_local_clipboard("Salut depuis A — 日本語 ✓").is_err() {
        eprintln!("presse-papiers indisponible dans cet environnement : test ignoré");
        return;
    }

    // Seul A a associé : B refuse et A l'explique (avant, A croyait avoir envoyé).
    pair.a.trust(&peer_id_of(&pair.a, "Machine B").unwrap()).unwrap();
    pair.a.send_clipboard(false);
    assert!(
        pair.a.status().contains("ne vous a pas encore associé"),
        "statut inattendu : {}",
        pair.a.status()
    );
    assert!(pair.b.snapshot().clipboard.is_none());

    pair.b.trust(&peer_id_of(&pair.b, "Machine A").unwrap()).unwrap();
    pair.a.send_clipboard(false);
    assert!(pair.a.status().contains("envoyé à 1"), "statut inattendu : {}", pair.a.status());
    let item = pair.b.snapshot().clipboard.unwrap();
    assert_eq!(item.text, "Salut depuis A — 日本語 ✓");
    assert_eq!(item.from, "Machine A");
    pair.a.shutdown();
    pair.b.shutdown();
}

#[test]
fn interrupted_transfer_resumes_where_it_stopped() {
    // La première connexion est coupée après 2 morceaux ; le nouvel essai a lieu 2 s plus tard.
    let pair = start_pair("reprise", |a, _| {
        a.fail_after_chunks = Some(2);
        a.retry_delay = Duration::from_secs(2);
    });
    pair_up(&pair);
    let source = pair.a_dir.join("gros.bin");
    let content = sample(6 * CHUNK + 1234);
    fs::write(&source, &content).unwrap();
    pair.a.send_files(vec![source]);

    // Pendant la pause : B garde exactement les 2 morceaux reçus.
    wait_for("coupure et fichier partiel", Duration::from_secs(10), || {
        matches!(transfer_state(&pair.a.snapshot(), "gros.bin", true), Some(TransferState::Paused(_)))
            && partial_size(&pair.b_dir.join("data")) == 2 * CHUNK as u64
    });
    wait_for("reprise puis fin", Duration::from_secs(20), || {
        transfer_state(&pair.a.snapshot(), "gros.bin", true) == Some(TransferState::Complete)
    });
    let path = pair.b.snapshot().received[0].path.clone();
    assert_eq!(fs::read(path).unwrap(), content);
    pair.a.shutdown();
    pair.b.shutdown();
}

#[test]
fn transfer_resumes_after_receiver_restart() {
    let pair = start_pair("redemarrage", |a, _| {
        a.fail_after_chunks = Some(3);
        a.retry_delay = Duration::from_secs(4);
    });
    pair_up(&pair);
    let source = pair.a_dir.join("film.bin");
    let content = sample(8 * CHUNK);
    fs::write(&source, &content).unwrap();
    pair.a.send_files(vec![source]);
    wait_for("fichier partiel", Duration::from_secs(10), || {
        partial_size(&pair.b_dir.join("data")) == 3 * CHUNK as u64
    });

    // B s'arrête puis redémarre (même dossier, mêmes ports) : il retrouve le fichier partiel.
    pair.b.shutdown();
    thread::sleep(Duration::from_millis(1500));
    let b2 = Engine::start(pair.b_config.clone()).unwrap();
    wait_for("terminé après redémarrage", Duration::from_secs(30), || {
        transfer_state(&b2.snapshot(), "film.bin", false) == Some(TransferState::Complete)
    });
    let path = b2.snapshot().received[0].path.clone();
    assert_eq!(fs::read(path).unwrap(), content);
    pair.a.shutdown();
    b2.shutdown();
}

#[test]
fn cancelled_transfer_removes_the_partial_file() {
    let pair = start_pair("annulation", |a, _| {
        a.fail_after_chunks = Some(1);
        a.retry_delay = Duration::from_secs(30);
    });
    pair_up(&pair);
    let source = pair.a_dir.join("annule.bin");
    fs::write(&source, sample(4 * CHUNK)).unwrap();
    pair.a.send_files(vec![source]);
    wait_for("fichier partiel", Duration::from_secs(10), || {
        partial_size(&pair.b_dir.join("data")) == CHUNK as u64
    });

    let id = pair.a.snapshot().transfers.iter().find(|t| t.file_name == "annule.bin").unwrap().id.clone();
    pair.a.cancel_transfer(&id);
    assert_eq!(transfer_state(&pair.a.snapshot(), "annule.bin", true), Some(TransferState::Failed("annulé".into())));
    wait_for("fichier partiel supprimé chez B", Duration::from_secs(10), || {
        partial_size(&pair.b_dir.join("data")) == 0
    });
    pair.a.shutdown();
    pair.b.shutdown();
}

#[test]
fn manual_address_connects_devices_that_cannot_see_each_other() {
    // Ports différents et aucune adresse de test : les diffusions de l'un n'arrivent jamais
    // chez l'autre. Seule l'adresse saisie à la main peut les relier.
    let base = NEXT_PORT.fetch_add(4, Ordering::SeqCst);
    let (a_disc, a_tcp, b_disc, b_tcp) = (base, base + 1, base + 2, base + 3);
    let a = Engine::start(config(&temp_dir("manuel-a"), a_disc, a_tcp, vec![])).unwrap();
    let b = Engine::start(config(&temp_dir("manuel-b"), b_disc, b_tcp, vec![])).unwrap();
    a.set_device_name("Machine A").unwrap();
    b.set_device_name("Machine B").unwrap();

    thread::sleep(Duration::from_secs(3));
    assert!(peer_id_of(&a, "Machine B").is_none(), "A ne devrait pas voir B sans adresse");
    assert!(peer_id_of(&b, "Machine A").is_none(), "B ne devrait pas voir A sans adresse");

    // Adresses invalides : refusées avec un message clair.
    assert!(b.add_manual_address("").is_err());
    assert!(b.add_manual_address("nom-qui-n-existe-pas.invalid").is_err());

    // B saisit l'adresse de A : les DEUX se voient, et durablement.
    b.add_manual_address(&format!("127.0.0.1:{a_disc}")).unwrap();
    wait_for("B voit A", Duration::from_secs(10), || peer_id_of(&b, "Machine A").is_some());
    wait_for("A voit B", Duration::from_secs(10), || peer_id_of(&a, "Machine B").is_some());
    thread::sleep(Duration::from_secs(10));
    assert!(peer_id_of(&a, "Machine B").is_some() && peer_id_of(&b, "Machine A").is_some(), "liaison perdue");

    let manual = b.snapshot().manual;
    assert_eq!(manual.len(), 1);
    assert_eq!(manual[0].peer_name.as_deref(), Some("Machine A"));

    b.remove_manual_address(&format!("127.0.0.1:{a_disc}")).unwrap();
    assert!(b.snapshot().manual.is_empty());
    a.shutdown();
    b.shutdown();
}

/// Débit réel d'un transfert (chiffré, sur disque) entre deux moteurs, en version optimisée :
/// `cargo test --release --test e2e debit_d_un_transfert -- --ignored --nocapture`
#[test]
#[ignore]
fn debit_d_un_transfert() {
    let pair = start_pair("debit", |_, _| {});
    pair_up(&pair);
    let size = 200 * 1024 * 1024;
    let source = pair.a_dir.join("gros-fichier.bin");
    fs::write(&source, sample(size)).unwrap();
    let start = Instant::now();
    pair.a.send_files(vec![source]);
    wait_for("transfert", Duration::from_secs(600), || {
        transfer_state(&pair.b.snapshot(), "gros-fichier.bin", false) == Some(TransferState::Complete)
    });
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "transfert de 200 Mo (empreinte + chiffrement + écriture disque) : {seconds:.1} s = {:.0} Mo/s",
        200.0 / seconds
    );
    pair.a.shutdown();
    pair.b.shutdown();
}

/// Un appareil d'une autre version n'est pas ignoré en silence : la page Appareils l'explique,
/// et l'avertissement disparaît dès qu'il annonce la bonne version.
#[test]
fn other_version_is_explained() {
    let dir = temp_dir("autre-version");
    let discovery = NEXT_PORT.fetch_add(2, Ordering::SeqCst);
    let engine = Engine::start(config(&dir, discovery, discovery + 1, Vec::new())).unwrap();
    let socket = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    let send = |protocol: &str, extra: &str| {
        let hello = format!(
            r#"{{"protocol":"{protocol}","device":{{"id":"pc-ancien","name":"PC du salon","platform":"windows","publicKey":"AAAA"}},"tcpPort":1,"discoveryPort":1{extra}}}"#
        );
        socket.send_to(hello.as_bytes(), loopback(discovery)).unwrap();
    };

    // Version 0.3 (pas de version dans l'annonce) : plus ancienne.
    send("magicplus/3", "");
    wait_for("avertissement 0.3", Duration::from_secs(5), || !engine.snapshot().other_versions.is_empty());
    let other = engine.snapshot().other_versions[0].clone();
    assert_eq!(other.name, "PC du salon");
    assert_eq!(other.version, "0.3");
    assert!(!other.newer);
    assert!(other.message.contains("ancienne version"), "{}", other.message);
    assert!(engine.snapshot().peers.is_empty(), "un appareil d'une autre version ne doit pas être utilisable");

    // Version plus récente, qui donne son numéro.
    send("magicplus/9", r#","appVersion":"0.9.1""#);
    wait_for("avertissement 0.9.1", Duration::from_secs(5), || {
        engine.snapshot().other_versions.first().is_some_and(|o| o.version == "0.9.1" && o.newer)
    });
    assert!(engine.snapshot().status.contains("Mettez à jour cet appareil"));
    engine.shutdown();
}
