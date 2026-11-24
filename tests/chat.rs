//! Discussions de bout en bout : plusieurs moteurs complets dans le même processus.

use magicplus_core::{AttachmentKind, Config, Engine, FriendState, MessageStatus, MessageView};
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
/// Profils : arrivent chez un appareil associé et chez un membre de groupe jamais associé ;
/// une modification est rediffusée, une image retirée disparaît.
#[test]
fn profiles_reach_paired_devices_and_group_members() {
    let nodes = start_nodes("profils", 3);
    let (a, b, c) = (&nodes[0].engine, &nodes[1].engine, &nodes[2].engine);
    trust(a, b);
    trust(a, c);
    trust(b, a);
    trust(c, a);
    let group = a.create_group("Profils", &[id_of(b), id_of(c)]).unwrap();
    wait_for("groupe", Duration::from_secs(10), || has_conversation(b, &group) && has_conversation(c, &group));

    // B change son profil : A (associé) et C (seulement membre du groupe) le reçoivent.
    let photo = nodes[1].dir.join("photo.png");
    image::RgbImage::from_pixel(400, 300, image::Rgb([20, 120, 220])).save(&photo).unwrap();
    b.set_profile_image(magicplus_core::ProfileImage::Avatar, &photo).unwrap();
    b.set_profile_details("Fan de **randonnée** 🏔️", "il/lui", Some(0x3E7BFA)).unwrap();
    for (engine, name) in [(a, "A"), (c, "C")] {
        wait_for(&format!("{name} reçoit le profil de B"), Duration::from_secs(15), || {
            engine.profile(&id_of(b)).is_some_and(|p| p.bio.contains("randonnée") && p.avatar.is_some())
        });
    }
    let seen = c.profile(&id_of(b)).unwrap();
    assert_eq!(seen.pronouns, "il/lui");
    assert_eq!(seen.color, Some(0x3E7BFA));
    assert_eq!(seen.name, "Machine B");
    let avatar = image::open(seen.avatar.unwrap()).unwrap();
    assert_eq!((avatar.width(), avatar.height()), (256, 256));

    // Retrait de la photo : rediffusé aussi.
    b.remove_profile_image(magicplus_core::ProfileImage::Avatar).unwrap();
    wait_for("A voit la photo retirée", Duration::from_secs(15), || a.profile(&id_of(b)).is_some_and(|p| p.avatar.is_none()));
    assert!(b.my_profile().avatar.is_none());
    for node in &nodes {
        node.engine.shutdown();
    }
}

/// Canevas : créé par A, écrit en même temps par B et C (jamais associés entre eux), même texte
/// partout, arrivée rapide des frappes, historique et restauration.
#[test]
fn group_canvas_real_time_and_history() {
    let nodes = start_nodes("canevas", 3);
    let (a, b, c) = (&nodes[0].engine, &nodes[1].engine, &nodes[2].engine);
    trust(a, b);
    trust(a, c);
    trust(b, a);
    trust(c, a);
    let group = a.create_group("Canevas", &[id_of(b), id_of(c)]).unwrap();
    wait_for("groupe", Duration::from_secs(10), || has_conversation(b, &group) && has_conversation(c, &group));

    let id = a.create_canvas(&group, "Courses").unwrap();
    a.set_canvas_text(&group, &id, "# Courses\n- pain\n").unwrap();
    for (engine, name) in [(b, "B"), (c, "C")] {
        wait_for(&format!("{name} voit le canevas"), Duration::from_secs(10), || {
            engine.canvases(&group).iter().any(|cv| cv.id == id && cv.title == "Courses" && cv.text.contains("pain"))
        });
    }

    // Temps réel : une frappe de B arrive vite chez C.
    let started = Instant::now();
    b.set_canvas_text(&group, &id, "# Courses\n- pain\n- lait\n").unwrap();
    wait_for("C voit « lait »", Duration::from_secs(5), || c.canvases(&group)[0].text.contains("lait"));
    assert!(started.elapsed() < Duration::from_secs(3), "trop lent : {:?}", started.elapsed());

    // B et C écrivent en même temps à deux endroits : rien ne se perd, tout le monde converge.
    let base = c.canvases(&group)[0].text.clone();
    b.set_canvas_text(&group, &id, &base.replace("# Courses", "# Courses de la semaine")).unwrap();
    c.set_canvas_text(&group, &id, &format!("{base}- œufs\n")).unwrap();
    wait_for("les trois ont le même texte", Duration::from_secs(10), || {
        let texts: Vec<String> = [a, b, c].iter().map(|e| e.canvases(&group)[0].text.clone()).collect();
        texts.iter().all(|t| t == &texts[0] && t.contains("de la semaine") && t.contains("œufs"))
    });

    // Historique : une version (forcée ici, sinon après 45 s de pause), puis restauration.
    let before = a.canvases(&group)[0].text.clone();
    b.save_canvas_version(&group, &id).unwrap();
    wait_for("A voit la version de B", Duration::from_secs(10), || {
        a.canvas_versions(&group, &id).iter().any(|v| v.authors == ["Machine B"])
    });
    a.set_canvas_text(&group, &id, "tout effacé").unwrap();
    a.save_canvas_version(&group, &id).unwrap();
    let version = a.canvas_versions(&group, &id).into_iter().find(|v| v.authors == ["Machine B"]).unwrap();
    a.name_canvas_version(&group, &id, &version.id, "Avant le ménage").unwrap();
    a.restore_canvas_version(&group, &id, &version.id).unwrap();
    wait_for("C reçoit la restauration et le nom de version", Duration::from_secs(10), || {
        let text = c.canvases(&group)[0].text.clone();
        let versions = c.canvas_versions(&group, &id);
        text == before
            && versions.iter().any(|v| v.label == "Avant le ménage")
            && versions.iter().any(|v| v.summary.starts_with("Restauration"))
    });
    assert!(before.contains("œufs"));

    // Renommer puis supprimer : partout.
    c.rename_canvas(&group, &id, "Liste").unwrap();
    wait_for("A voit le nouveau titre", Duration::from_secs(10), || a.canvases(&group)[0].title == "Liste");
    a.delete_canvas(&group, &id).unwrap();
    wait_for("B ne voit plus le canevas", Duration::from_secs(10), || b.canvases(&group).is_empty());
    for node in &nodes {
        node.engine.shutdown();
    }
}

/// Pseudo public, surnom local, épingles, transfert et réaction avec n'importe quel émoji.
#[test]
fn pseudo_alias_pins_forward_and_any_reaction() {
    let nodes = start_nodes("pseudo", 3);
    let (a, b, c) = (&nodes[0].engine, &nodes[1].engine, &nodes[2].engine);
    trust(a, b);
    trust(b, a);
    trust(a, c);
    trust(c, a);

    // Pseudo : vu par les autres dès l'annonce, même sans association.
    a.set_pseudo("Léa").unwrap();
    wait_for("B voit le pseudo de A", Duration::from_secs(10), || {
        b.snapshot().peers.iter().any(|p| p.id == id_of(a) && p.name == "Léa" && p.device_name == "Machine A")
    });
    // Surnom local : seulement chez B.
    b.set_alias(&id_of(a), "Maman").unwrap();
    let peer = b.snapshot().peers.into_iter().find(|p| p.id == id_of(a)).unwrap();
    assert_eq!((peer.name.as_str(), peer.alias.as_str(), peer.pseudo.as_str()), ("Maman", "Maman", "Léa"));
    assert!(c.snapshot().peers.iter().all(|p| p.name != "Maman"));

    let direct = a.open_direct(&id_of(b)).unwrap();
    a.send_chat(&direct, "Rendez-vous **samedi**", None, &[]).unwrap();
    wait_for("B reçoit", Duration::from_secs(10), || has_conversation(b, &direct) && !b.messages(&direct, 10).is_empty());
    let title = b.conversations().into_iter().find(|cv| cv.id == direct).unwrap().title;
    assert_eq!(title, "Maman");
    assert_eq!(b.messages(&direct, 10)[0].author_name, "Maman");
    assert_eq!(b.profile(&id_of(a)).unwrap().name, "Maman");

    // N'importe quel émoji en réaction.
    let message = b.messages(&direct, 10)[0].id.clone();
    b.toggle_reaction(&direct, &message, "🦀").unwrap();
    wait_for("A voit 🦀", Duration::from_secs(10), || {
        a.messages(&direct, 10).iter().any(|m| m.reactions.iter().any(|r| r.emoji == "🦀"))
    });

    // Épingler (par B), visible chez A avec une ligne « a épinglé », puis désépingler.
    b.pin_chat(&direct, &message, true).unwrap();
    wait_for("A voit le message épinglé", Duration::from_secs(10), || a.pinned_messages(&direct).len() == 1);
    assert_eq!(a.pinned_messages(&direct)[0].pinned_by.as_deref(), Some("Machine B"));
    assert!(a.messages(&direct, 10).iter().any(|m| m.system && m.text.contains("a épinglé") && m.text.contains("samedi")));
    a.pin_chat(&direct, &message, false).unwrap();
    wait_for("B voit le message désépinglé", Duration::from_secs(10), || b.pinned_messages(&direct).is_empty());

    // Transfert vers un groupe : C voit le message et le nom public de son auteur (pas le surnom de B).
    let group = a.create_group("Famille", &[id_of(b), id_of(c)]).unwrap();
    wait_for("groupe", Duration::from_secs(10), || has_conversation(c, &group));
    b.forward_chat(&direct, &message, &group).unwrap();
    wait_for("C reçoit le message transféré", Duration::from_secs(10), || {
        c.messages(&group, 20).iter().any(|m| m.text.contains("samedi") && m.forwarded.as_deref() == Some("Léa"))
    });
    for node in &nodes {
        node.engine.shutdown();
    }
}

/// Message de démonstration qui utilise tout le Markdown pris en charge.
const DEMO_MARKDOWN: &str = "## Programme du voyage
-# mis à jour ce matin
1. **Samedi** : départ à 8 h
2. *Dimanche* : plage et __pique-nique__
   - [x] glacière
   - [ ] parasol

> [!TIP]
> Pensez à la crème solaire, <font color=\"orange\">indice 50</font> !

| Qui | Apporte | Budget |
|:--|:--|--:|
| Léa | Tente | 120 € |
| Max | ~~Barbecue~~ Réchaud | 45 € |

```python
def partage(total, personnes=3):
    # chacun paie la même chose
    return round(total / personnes, 2)
```";

/// `MAGICPLUS_DEMO_DIR=/dossier cargo test --test chat donnees_de_demo -- --ignored`
/// puis `MAGICPLUS_DATA_DIR=/dossier/data MAGICPLUS_START_PAGE=1 ./target/debug/magicplus`.
#[test]
fn any_member_adds_people_and_friend_requests() {
    let nodes = start_nodes("amis", 5);
    let (a, b, c, d, e) = (&nodes[0].engine, &nodes[1].engine, &nodes[2].engine, &nodes[3].engine, &nodes[4].engine);
    // A ↔ B, B ↔ C, A ↔ D, A ↔ E : C ne connaît que B ; D et E arrivent après la création.
    for (x, y) in [(a, b), (b, c), (a, d), (a, e)] {
        trust(x, y);
        trust(y, x);
    }
    let (id_a, id_c, id_e) = (id_of(a), id_of(c), id_of(e));

    let group = a.create_group("Projet", &[id_of(b)]).unwrap();
    a.send_chat(&group, "Bienvenue", None, &[]).unwrap();
    wait_for("B dans le groupe", Duration::from_secs(10), || texts(&b.messages(&group, 50)) == ["Bienvenue"]);

    // B (pas le créateur) ajoute C, que A ne connaît pas.
    assert!(b.conversations().iter().any(|conv| conv.id == group && !conv.is_creator));
    b.add_group_members(&group, std::slice::from_ref(&id_c)).unwrap();
    assert!(b.add_group_members(&group, std::slice::from_ref(&id_c)).is_err(), "déjà membre");
    wait_for("C rejoint le groupe et lit l'historique de A", Duration::from_secs(15), || {
        texts(&c.messages(&group, 50)) == ["Bienvenue"]
    });
    wait_for("A apprend l'ajout de C", Duration::from_secs(10), || {
        a.conversations().iter().any(|conv| conv.id == group && conv.members.iter().any(|m| m.id == id_c))
    });
    let notes: Vec<String> = a.messages(&group, 50).into_iter().filter(|m| m.system).map(|m| m.text).collect();
    assert!(notes.iter().any(|note| note == "Machine B a ajouté Machine C"), "{notes:?}");

    // C écrit : A le reçoit sans être associé à C.
    c.send_chat(&group, "Merci B !", None, &[]).unwrap();
    wait_for("A reçoit le message de C", Duration::from_secs(15), || texts(&a.messages(&group, 50)).contains(&"Merci B !".to_string()));

    // Le créateur ajoute D plus tard (autrefois refusé chez D).
    a.add_group_members(&group, &[id_of(d)]).unwrap();
    wait_for("D rejoint le groupe", Duration::from_secs(15), || {
        let list = texts(&d.messages(&group, 50));
        list.contains(&"Bienvenue".to_string()) && list.contains(&"Merci B !".to_string())
    });
    assert!(d.conversations().iter().any(|conv| conv.id == group && !conv.closed && conv.title == "Projet"));

    // Demande d'ami de A vers C, acceptée.
    let state_of = |engine: &Engine, member: &str| {
        engine.conversations().into_iter().find(|conv| conv.id == group).unwrap().members.into_iter().find(|m| m.id == member).unwrap().friend
    };
    assert_eq!(state_of(a, &id_c), FriendState::None);
    assert_eq!(state_of(a, &id_of(b)), FriendState::Friend);
    a.send_friend_request(&group, &id_c).unwrap();
    assert_eq!(state_of(a, &id_c), FriendState::Sent);
    wait_for("C reçoit la demande", Duration::from_secs(15), || {
        c.snapshot().friend_requests.iter().any(|r| r.id == id_a && r.incoming && r.group == "Projet")
    });
    wait_for("A voit la demande arrivée", Duration::from_secs(10), || {
        a.snapshot().friend_requests.iter().any(|r| r.id == id_c && !r.incoming && r.delivered)
    });
    assert_eq!(state_of(c, &id_a), FriendState::Received);
    c.answer_friend_request(&id_a, true).unwrap();
    assert!(c.snapshot().peers.iter().any(|p| p.id == id_a && p.trusted));
    wait_for("A associé à C", Duration::from_secs(15), || a.snapshot().peers.iter().any(|p| p.id == id_c && p.trusted));
    assert!(a.snapshot().friend_requests.is_empty() && c.snapshot().friend_requests.is_empty());
    assert_eq!(state_of(a, &id_c), FriendState::Friend);
    // Associés : la discussion à deux fonctionne directement.
    let direct = c.open_direct(&id_a).unwrap();
    c.send_chat(&direct, "On est amis", None, &[]).unwrap();
    wait_for("discussion à deux A ↔ C", Duration::from_secs(10), || texts(&a.messages(&direct, 50)) == ["On est amis"]);

    // Demande croisée entre C et D : acceptée des deux côtés sans clic.
    a.send_chat(&group, "re", None, &[]).unwrap();
    wait_for("D connaît C", Duration::from_secs(15), || {
        d.conversations().iter().any(|conv| conv.id == group && conv.members.iter().any(|m| m.id == id_c))
    });
    let id_d = id_of(d);
    c.send_friend_request(&group, &id_d).unwrap();
    wait_for("D reçoit la demande de C", Duration::from_secs(15), || d.snapshot().friend_requests.iter().any(|r| r.id == id_c));
    d.send_friend_request(&group, &id_c).unwrap(); // = accepter
    wait_for("C et D associés", Duration::from_secs(15), || {
        c.snapshot().peers.iter().any(|p| p.id == id_d && p.trusted) && d.snapshot().peers.iter().any(|p| p.id == id_c && p.trusted)
    });

    // Refus : E (ajouté par A) refuse la demande de B.
    a.add_group_members(&group, std::slice::from_ref(&id_e)).unwrap();
    wait_for("B connaît E", Duration::from_secs(15), || {
        b.conversations().iter().any(|conv| conv.id == group && conv.members.iter().any(|m| m.id == id_e))
    });
    b.send_friend_request(&group, &id_e).unwrap();
    wait_for("E reçoit la demande de B", Duration::from_secs(15), || e.snapshot().friend_requests.iter().any(|r| r.id == id_of(b)));
    e.answer_friend_request(&id_of(b), false).unwrap();
    wait_for("B apprend le refus", Duration::from_secs(15), || b.snapshot().friend_requests.is_empty());
    assert!(!b.snapshot().peers.iter().any(|p| p.id == id_e && p.trusted));
    assert!(!e.snapshot().peers.iter().any(|p| p.id == id_of(b) && p.trusted));
    assert_eq!(state_of(b, &id_e), FriendState::None);
}

#[test]
#[ignore]
fn donnees_de_demo() {
    let Some(target) = std::env::var_os("MAGICPLUS_DEMO_DIR").map(PathBuf::from) else { return };
    let nodes = start_nodes("demo", 5);
    let (a, b, c, d, e) = (&nodes[0].engine, &nodes[1].engine, &nodes[2].engine, &nodes[3].engine, &nodes[4].engine);
    a.set_device_name("Mac de démo").unwrap();
    b.set_device_name("PC-Bureau").unwrap();
    c.set_device_name("Portable de Léa").unwrap();
    d.set_device_name("Tablette de Sam").unwrap();
    e.set_device_name("PC-Atelier").unwrap();
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

    // Profils : photos et bannières dégradées, présentations en Markdown.
    let gradient = |path: &Path, w: u32, h: u32, from: [u8; 3], to: [u8; 3]| {
        let image = image::RgbImage::from_fn(w, h, |x, y| {
            let t = (x + y) as f32 / (w + h) as f32;
            image::Rgb([0, 1, 2].map(|i| (from[i] as f32 * (1.0 - t) + to[i] as f32 * t) as u8))
        });
        image.save(path).unwrap();
    };
    for (index, (node, bio, pronouns)) in [
        (&nodes[0], "Développeur du dimanche 🦀\n**MagicPlus** sur tous mes appareils.", "il/lui"),
        (&nodes[1], "Bureau de la maison · *toujours allumé*", ""),
        (&nodes[2], "Voyages, photo et __randonnée__ 🏔️", "elle/elle"),
    ]
    .into_iter()
    .enumerate()
    {
        let colors = [([88, 101, 242], [235, 69, 158]), ([46, 204, 113], [52, 152, 219]), ([241, 196, 15], [231, 76, 60])][index];
        let photo = node.dir.join("photo.png");
        let banner = node.dir.join("banniere.png");
        gradient(&photo, 300, 300, colors.0, colors.1);
        gradient(&banner, 1000, 400, colors.1, colors.0);
        node.engine.set_profile_image(magicplus_core::ProfileImage::Avatar, &photo).unwrap();
        if index != 1 {
            node.engine.set_profile_image(magicplus_core::ProfileImage::Banner, &banner).unwrap();
        }
        node.engine.set_profile_details(bio, pronouns, Some(0x30A46C)).unwrap();
    }
    wait_for("profils reçus", Duration::from_secs(20), || {
        [b, c].iter().all(|other| a.profile(&id_of(other)).is_some_and(|p| p.avatar.is_some()))
    });
    c.send_chat(&group, "J'ai trouvé une location près de la mer 🌊", None, &[]).unwrap();
    thread::sleep(Duration::from_millis(400));
    b.send_chat(&group, "Top ! Combien de chambres ?", None, &[]).unwrap();
    thread::sleep(Duration::from_millis(400));
    c.send_chat(&group, "**Trois**, et une terrasse.", None, &[]).unwrap();
    thread::sleep(Duration::from_millis(400));
    b.send_chat(&group, DEMO_MARKDOWN, None, &[]).unwrap();
    wait_for("tout reçu", Duration::from_secs(20), || {
        texts(&a.messages(&group, 50)).len() == 4
            && a.messages(&direct, 50).iter().any(|m| m.attachments.iter().any(|x| x.path.is_some()))
    });
    // Membres ajoutés par d'autres que le créateur, et une demande d'ami reçue.
    for (x, y) in [(b, d), (d, b), (c, e), (e, c)] {
        trust(x, y);
    }
    b.add_group_members(&group, &[id_of(d)]).unwrap();
    c.add_group_members(&group, &[id_of(e)]).unwrap();
    wait_for("D et E dans le groupe", Duration::from_secs(20), || {
        [d, e].iter().all(|x| x.conversations().iter().any(|conv| conv.id == group && conv.members.len() == 5))
    });
    d.send_chat(&group, "Merci pour l'invitation 👋", None, &[]).unwrap();
    e.send_friend_request(&group, &id_of(a)).unwrap();
    wait_for("demande d'ami chez A", Duration::from_secs(20), || !a.snapshot().friend_requests.is_empty());
    wait_for("message de D chez A", Duration::from_secs(20), || texts(&a.messages(&group, 50)).len() == 5);
    // Canevas partagé, écrit à plusieurs, avec un historique.
    let canvas = a.create_canvas(&group, "Préparatifs").unwrap();
    a.set_canvas_text(&group, &canvas, "# Préparatifs du week-end\n\n## À emporter\n- [x] tente\n- [ ] glacière\n").unwrap();
    a.save_canvas_version(&group, &canvas).unwrap();
    wait_for("B voit le canevas", Duration::from_secs(10), || b.canvases(&group).iter().any(|cv| cv.text.contains("glacière")));
    let text = b.canvases(&group)[0].text.clone();
    b.set_canvas_text(
        &group,
        &canvas,
        &format!(
            "{text}- [ ] <font color=\"orange\">crème solaire</font>\n\n## Budget\n| Poste | Montant |\n|:--|--:|\n| Location | 420 € |\n| Courses | 160 € |\n\n> [!NOTE]\n> Départ **samedi 8 h** devant chez Léa.\n"
        ),
    )
    .unwrap();
    b.save_canvas_version(&group, &canvas).unwrap();
    wait_for("A voit le canevas complet", Duration::from_secs(10), || {
        a.canvases(&group)[0].text.contains("Budget") && a.canvas_versions(&group, &canvas).len() == 2
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
