//! Discussions de bout en bout : plusieurs moteurs complets dans le même processus.

use magicplus_core::{AttachmentKind, Config, Engine, MessageStatus, MessageView};
use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU16, Ordering},
    thread,
    time::{Duration, Instant},
};

static NEXT_PORT: AtomicU16 = AtomicU16::new(48100);

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("magicplus-chat-{}-{label}", std::process::id()));
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

struct Node {
    engine: Engine,
    config: Config,
    dir: PathBuf,
}

/// `count` moteurs qui se voient tous (adresses de boucle locale), nommés « Machine A », « B »…
fn start_nodes(label: &str, count: usize) -> Vec<Node> {
    let base = NEXT_PORT.fetch_add(10, Ordering::SeqCst);
    let ports: Vec<(u16, u16)> = (0..count as u16).map(|i| (base + 2 * i, base + 2 * i + 1)).collect();
    (0..count)
        .map(|i| {
            let dir = temp_dir(&format!("{label}-{i}"));
            let mut config = Config::new(dir.join("data"), ports[i].0, ports[i].1);
            config.extra_targets = ports
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, (discovery, _))| SocketAddr::from(([127, 0, 0, 1], *discovery)))
                .collect();
            config.retry_delay = Duration::from_millis(500);
            let engine = Engine::start(config.clone()).unwrap();
            engine.set_device_name(&format!("Machine {}", (b'A' + i as u8) as char)).unwrap();
            let inbox = dir.join("inbox").to_string_lossy().to_string();
            engine.update_settings(|s| s.save_folder = Some(inbox)).unwrap();
            Node { engine, config, dir }
        })
        .collect()
}

fn id_of(engine: &Engine) -> String {
    engine.snapshot().identity.id
}

fn sees(engine: &Engine, name: &str) -> Option<String> {
    engine.snapshot().peers.into_iter().find(|p| p.name == name && p.online).map(|p| p.id)
}

/// `from` associe `to` (après l'avoir vu sur le réseau).
fn trust(from: &Engine, to: &Engine) {
    let name = to.snapshot().identity.name;
    wait_for(&format!("voir {name}"), Duration::from_secs(10), || sees(from, &name).is_some());
    from.trust(&id_of(to)).unwrap();
}

fn texts(messages: &[MessageView]) -> Vec<String> {
    messages.iter().filter(|m| !m.system).map(|m| m.text.clone()).collect()
}

fn find<'a>(messages: &'a [MessageView], text: &str) -> &'a MessageView {
    messages.iter().find(|m| m.text == text).unwrap_or_else(|| panic!("message « {text} » introuvable"))
}

fn has_conversation(engine: &Engine, id: &str) -> bool {
    engine.conversations().iter().any(|c| c.id == id)
}

#[test]
fn direct_chat_reply_edit_reaction_read_typing_delete() {
    let nodes = start_nodes("direct", 2);
    let (a, b) = (&nodes[0].engine, &nodes[1].engine);
    trust(a, b);
    trust(b, a);

    let conversation = a.open_direct(&id_of(b)).unwrap();
    a.send_chat(&conversation, "Salut **B** 👋", None, &[]).unwrap();
    wait_for("B reçoit le message", Duration::from_secs(10), || {
        has_conversation(b, &conversation) && texts(&b.messages(&conversation, 50)) == ["Salut **B** 👋"]
    });
    let b_list = b.conversations();
    let b_conversation = b_list.iter().find(|c| c.id == conversation).unwrap();
    assert_eq!(b_conversation.title, "Machine A");
    assert_eq!(b_conversation.unread, 1);

    // Réponse.
    let original = find(&b.messages(&conversation, 50), "Salut **B** 👋").id.clone();
    b.send_chat(&conversation, "Coucou !", Some(&original), &[]).unwrap();
    wait_for("A reçoit la réponse", Duration::from_secs(10), || texts(&a.messages(&conversation, 50)).len() == 2);
    let reply = find(&a.messages(&conversation, 50), "Coucou !").reply.clone().unwrap();
    assert_eq!(reply.id, original);
    // Les extraits sont en texte brut (sans les marques Markdown).
    assert_eq!(reply.excerpt, "Salut B 👋");

    // Modification, réaction, puis lecture.
    a.edit_chat(&conversation, &original, "Salut **B** ! (modifié)").unwrap();
    b.toggle_reaction(&conversation, &original, "👍").unwrap();
    wait_for("modification chez B", Duration::from_secs(10), || {
        b.messages(&conversation, 50).iter().any(|m| m.id == original && m.edited && m.text == "Salut **B** ! (modifié)")
    });
    wait_for("réaction chez A", Duration::from_secs(10), || {
        a.messages(&conversation, 50)
            .iter()
            .any(|m| m.id == original && m.reactions.iter().any(|r| r.emoji == "👍" && r.count == 1 && !r.mine))
    });
    b.mark_read(&conversation);
    wait_for("accusé de lecture chez A", Duration::from_secs(10), || {
        a.messages(&conversation, 50)
            .iter()
            .any(|m| m.id == original && m.status == MessageStatus::Read(vec!["Machine B".into()]))
    });
    assert_eq!(b.conversations().iter().find(|c| c.id == conversation).unwrap().unread, 0);

    // « En train d'écrire ».
    b.notify_typing(&conversation);
    wait_for("indicateur de frappe chez A", Duration::from_secs(5), || {
        a.conversations().iter().any(|c| c.id == conversation && c.typing == ["Machine B"])
    });

    // Suppression.
    let reply_id = find(&b.messages(&conversation, 50), "Coucou !").id.clone();
    b.delete_chat(&conversation, &reply_id).unwrap();
    wait_for("suppression chez A", Duration::from_secs(10), || {
        a.messages(&conversation, 50).iter().any(|m| m.id == reply_id && m.deleted && m.text.is_empty())
    });
    // On ne modifie pas le message d'un autre.
    assert!(a.edit_chat(&conversation, &reply_id, "piraté").is_err());

    for node in &nodes {
        node.engine.shutdown();
    }
}

#[test]
fn messages_arrive_complete_and_in_order_after_being_offline() {
    let nodes = start_nodes("horsligne", 2);
    let (a, b) = (&nodes[0].engine, &nodes[1].engine);
    trust(a, b);
    trust(b, a);
    let conversation = a.open_direct(&id_of(b)).unwrap();
    a.send_chat(&conversation, "0", None, &[]).unwrap();
    wait_for("premier message", Duration::from_secs(10), || texts(&b.messages(&conversation, 100)) == ["0"]);

    // B s'éteint ; A écrit 30 messages pendant ce temps.
    b.shutdown();
    thread::sleep(Duration::from_millis(1500));
    for n in 1..=30 {
        a.send_chat(&conversation, &n.to_string(), None, &[]).unwrap();
    }
    thread::sleep(Duration::from_secs(1));
    assert!(matches!(
        find(&a.messages(&conversation, 100), "30").status,
        MessageStatus::Sending
    ));

    // B redémarre : tout arrive, sans trou et dans l'ordre.
    let b2 = Engine::start(nodes[1].config.clone()).unwrap();
    let expected: Vec<String> = (0..=30).map(|n| n.to_string()).collect();
    wait_for("30 messages livrés dans l'ordre", Duration::from_secs(30), || texts(&b2.messages(&conversation, 100)) == expected);
    wait_for("confirmés chez A", Duration::from_secs(10), || {
        find(&a.messages(&conversation, 100), "30").status == MessageStatus::Delivered
    });
    a.shutdown();
    b2.shutdown();
}

#[test]
fn group_of_three_members_introduced_by_the_creator() {
    let nodes = start_nodes("groupe", 3);
    let (a, b, c) = (&nodes[0].engine, &nodes[1].engine, &nodes[2].engine);
    // A associe B et C ; B et C n'associent que A. B et C ne se sont jamais associés.
    trust(a, b);
    trust(a, c);
    trust(b, a);
    trust(c, a);
    assert!(!b.snapshot().peers.iter().any(|p| p.name == "Machine C" && p.trusted));

    let group = a.create_group("Équipe", &[id_of(b), id_of(c)]).unwrap();
    wait_for("B et C reçoivent le groupe", Duration::from_secs(10), || {
        has_conversation(b, &group) && has_conversation(c, &group)
    });
    let in_b = b.conversations().into_iter().find(|conv| conv.id == group).unwrap();
    assert_eq!(in_b.title, "Équipe");
    assert_eq!(in_b.members.len(), 3);
    assert!(!in_b.is_creator);
    assert!(b.messages(&group, 10)[0].system, "message « groupe créé »");

    // B et C se parlent grâce aux clés présentées par A.
    b.send_chat(&group, "de B", None, &[]).unwrap();
    c.send_chat(&group, "de C", None, &[]).unwrap();
    a.send_chat(&group, "de A", None, &[]).unwrap();
    for (engine, name) in [(a, "A"), (b, "B"), (c, "C")] {
        wait_for(&format!("{name} a les trois messages"), Duration::from_secs(15), || texts(&engine.messages(&group, 50)).len() == 3);
    }

    // Rafale simultanée : au final, les trois appareils affichent exactement le même ordre.
    for n in 0..5 {
        a.send_chat(&group, &format!("A{n}"), None, &[]).unwrap();
        b.send_chat(&group, &format!("B{n}"), None, &[]).unwrap();
        c.send_chat(&group, &format!("C{n}"), None, &[]).unwrap();
    }
    for engine in [a, b, c] {
        wait_for("rafale reçue", Duration::from_secs(20), || texts(&engine.messages(&group, 100)).len() == 18);
    }
    let order_a: Vec<String> = a.messages(&group, 100).into_iter().map(|m| m.id).collect();
    let order_b: Vec<String> = b.messages(&group, 100).into_iter().map(|m| m.id).collect();
    let order_c: Vec<String> = c.messages(&group, 100).into_iter().map(|m| m.id).collect();
    assert_eq!(order_a, order_b);
    assert_eq!(order_a, order_c);
    // Chaque auteur reste dans son propre ordre.
    let mine_b: Vec<String> = texts(&c.messages(&group, 100)).into_iter().filter(|t| t.starts_with('B')).collect();
    assert_eq!(mine_b, ["B0", "B1", "B2", "B3", "B4"]);

    // Lecture dans un groupe : « lu par » nomme les lecteurs.
    b.mark_read(&group);
    c.mark_read(&group);
    wait_for("lu par B et C", Duration::from_secs(10), || {
        matches!(&find(&a.messages(&group, 100), "A4").status, MessageStatus::Read(names) if names.len() == 2)
    });

    // C quitte le groupe : les autres le voient.
    c.leave_group(&group).unwrap();
    wait_for("départ de C", Duration::from_secs(10), || {
        a.conversations().iter().any(|conv| conv.id == group && conv.members.len() == 2)
    });
    assert!(c.send_chat(&group, "encore là ?", None, &[]).is_err());

    for node in &nodes {
        node.engine.shutdown();
    }
}

fn write_png(path: &Path) {
    let image = image::RgbaImage::from_fn(640, 400, |x, y| image::Rgba([(x % 255) as u8, (y % 255) as u8, 128, 255]));
    image.save(path).unwrap();
}

#[test]
fn attachments_and_stickers_are_delivered() {
    let nodes = start_nodes("pieces", 2);
    let (a, b) = (&nodes[0].engine, &nodes[1].engine);
    trust(a, b);
    trust(b, a);
    let conversation = a.open_direct(&id_of(b)).unwrap();

    let photo = nodes[0].dir.join("photo.png");
    write_png(&photo);
    let notes = nodes[0].dir.join("notes.txt");
    fs::write(&notes, "compte rendu").unwrap();
    a.send_chat(&conversation, "Voici les fichiers", None, &[photo.clone(), notes]).unwrap();

    wait_for("pièces jointes reçues", Duration::from_secs(20), || {
        b.messages(&conversation, 10)
            .iter()
            .any(|m| m.attachments.len() == 2 && m.attachments.iter().all(|a| a.path.is_some()))
    });
    let message = b.messages(&conversation, 10).into_iter().find(|m| m.attachments.len() == 2).unwrap();
    let image = message.attachments.iter().find(|a| a.file_name == "photo.png").unwrap();
    assert_eq!(image.kind, AttachmentKind::Image);
    assert_eq!(fs::read(image.path.as_ref().unwrap()).unwrap(), fs::read(&photo).unwrap());
    // Les pièces jointes restent dans la discussion, pas dans le dossier de réception.
    assert!(fs::read_dir(nodes[1].dir.join("inbox")).map(|d| d.count()).unwrap_or(0) == 0);

    // Autocollant : image réduite, ajoutée à la bibliothèque, envoyée comme autocollant.
    let sticker = a.add_sticker(&photo).unwrap();
    assert_eq!(a.stickers().len(), 1);
    let thumbnail = image::open(&sticker.path).unwrap();
    assert!(thumbnail.width() <= 320 && thumbnail.height() <= 320);
    a.send_sticker(&conversation, &sticker.id).unwrap();
    wait_for("autocollant reçu", Duration::from_secs(20), || {
        b.messages(&conversation, 10)
            .iter()
            .any(|m| m.attachments.iter().any(|a| a.kind == AttachmentKind::Sticker && a.path.is_some()))
    });

    for node in &nodes {
        node.engine.shutdown();
    }
}

/// Données de démonstration pour regarder l'interface (pas un vrai test) :
/// `MAGICPLUS_DEMO_DIR=/dossier cargo test --test chat donnees_de_demo -- --ignored`
/// puis `MAGICPLUS_DATA_DIR=/dossier/data MAGICPLUS_START_PAGE=1 ./target/debug/magicplus`.
#[test]
#[ignore]
fn donnees_de_demo() {
    let Some(target) = std::env::var_os("MAGICPLUS_DEMO_DIR").map(PathBuf::from) else { return };
    let nodes = start_nodes("demo", 3);
    let (a, b, c) = (&nodes[0].engine, &nodes[1].engine, &nodes[2].engine);
    a.set_device_name("Mac de démo").unwrap();
    b.set_device_name("PC-Bureau").unwrap();
    c.set_device_name("Portable de Léa").unwrap();
    for (from, to) in [(a, b), (a, c), (b, a), (c, a)] {
        let name = to.snapshot().identity.name;
        wait_for("voir", Duration::from_secs(10), || sees(from, &name).is_some());
        from.trust(&id_of(to)).unwrap();
    }
    let direct = b.open_direct(&id_of(a)).unwrap();
    b.send_chat(&direct, "Salut ! Tu as reçu le **rapport** pour demain ?", None, &[]).unwrap();
    wait_for("1", Duration::from_secs(10), || has_conversation(a, &direct));
    thread::sleep(Duration::from_millis(400));
    let first = texts_ids(a, &direct)[0].clone();
    a.send_chat(&direct, "Oui merci 🙏 je regarde ça ce soir", None, &[]).unwrap();
    thread::sleep(Duration::from_millis(400));
    a.send_chat(&direct, "Le passage sur le `budget` est à *revoir*, et ~~la date~~ aussi. Détails : https://exemple.fr", Some(&first), &[]).unwrap();
    let photo = nodes[1].dir.join("tableau.png");
    write_png(&photo);
    thread::sleep(Duration::from_millis(400));
    b.send_chat(&direct, "Voici la photo du tableau blanc", None, &[photo]).unwrap();
    thread::sleep(Duration::from_millis(600));
    let reply = texts_ids(b, &direct)[2].clone();
    b.toggle_reaction(&direct, &reply, "👍").unwrap();
    a.send_chat(&direct, "🎉🎉", None, &[]).unwrap();

    let group = a.create_group("Projet Vacances", &[id_of(b), id_of(c)]).unwrap();
    wait_for("groupe", Duration::from_secs(10), || has_conversation(b, &group) && has_conversation(c, &group));
    c.send_chat(&group, "J'ai trouvé une location près de la mer 🌊", None, &[]).unwrap();
    thread::sleep(Duration::from_millis(400));
    b.send_chat(&group, "Top ! Combien de chambres ?", None, &[]).unwrap();
    thread::sleep(Duration::from_millis(400));
    c.send_chat(&group, "**Trois**, et une terrasse.", None, &[]).unwrap();
    wait_for("tout reçu", Duration::from_secs(20), || {
        texts(&a.messages(&group, 50)).len() == 3
            && a.messages(&direct, 50).iter().any(|m| m.attachments.iter().any(|x| x.path.is_some()))
    });
    a.mark_read(&group);
    thread::sleep(Duration::from_secs(2));
    for node in &nodes {
        node.engine.shutdown();
    }
    thread::sleep(Duration::from_secs(1));
    let _ = fs::remove_dir_all(&target);
    copy_dir(&nodes[0].dir.join("data"), &target.join("data"));
}

fn texts_ids(engine: &Engine, conversation: &str) -> Vec<String> {
    engine.messages(conversation, 50).into_iter().filter(|m| !m.system).map(|m| m.id).collect()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            copy_dir(&path, &to.join(entry.file_name()));
        } else {
            fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}
