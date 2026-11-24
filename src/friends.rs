//! Demandes d'ami entre membres d'un même groupe.
//!
//! Dans un groupe, chaque membre connaît la clé des autres (présentée par celui qui les a
//! ajoutés). Un membre que l'on n'a pas associé peut recevoir une demande d'ami : s'il
//! l'accepte, les deux appareils s'associent avec ces clés (comme avec le bouton « Associer »),
//! sans passer par la page Appareils ni comparer les empreintes à la main.
//!
//! - `FriendRequest` part vers le membre dès qu'il est en ligne ; il répond `accepted` (déjà
//!   ami, ou demande croisée) ou `pending` (la demande attend sa décision).
//! - Sa décision revient par `FriendAnswer`, renvoyée jusqu'à ce qu'elle soit reçue.
//! - On n'associe jamais une clé différente de celle présentée dans le groupe, et seulement
//!   l'appareil à qui l'on a soi-même demandé (ou qui nous l'a demandé et que l'on accepte).

use crate::{
    chat::SenderInfo,
    crypto,
    engine::{explain_refusal, lock, read_ack, refusal, Engine, Inner, Target},
    protocol::{MemberInfo, Message},
    store::{Store, TrustedPeer},
    util::now_ms,
};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};

const FILE: &str = "amis.json";
const RETRY: Duration = Duration::from_secs(10);
/// Une demande reçue est oubliée au bout de 30 jours sans réponse.
const INCOMING_DAYS: u64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    id: String,
    name: String,
    platform: String,
    public_key: String,
    /// Groupe commun par lequel la demande est passée.
    conversation: String,
    at_ms: u64,
    /// Demande envoyée : l'autre appareil l'a bien reçue.
    #[serde(default)]
    delivered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Answer {
    id: String,
    name: String,
    public_key: String,
    accepted: bool,
}

/// Demandes en cours, enregistrées dans `amis.json`.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FriendStore {
    incoming: Vec<Request>,
    outgoing: Vec<Request>,
    /// Réponses à faire parvenir.
    answers: Vec<Answer>,
    #[serde(skip)]
    retry: HashMap<String, Instant>,
}

impl FriendStore {
    pub(crate) fn load(store: &Store) -> Self {
        let mut friends: FriendStore = store.load(FILE).unwrap_or_default();
        let limit = now_ms().saturating_sub(INCOMING_DAYS * 24 * 3600 * 1000);
        friends.incoming.retain(|request| request.at_ms >= limit);
        friends
    }
}

/// Relation avec un membre de groupe, pour l'interface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FriendState {
    /// Pas encore ami : « Ajouter en ami » possible.
    #[default]
    None,
    /// Nous-même.
    Me,
    /// Associé (avec la clé présentée dans le groupe).
    Friend,
    /// Demande envoyée, en attente.
    Sent,
    /// Il nous a fait une demande : à accepter ou refuser.
    Received,
}

/// Demande d'ami, pour l'interface.
#[derive(Debug, Clone, PartialEq)]
pub struct FriendRequestView {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub fingerprint: String,
    /// Nom du groupe commun.
    pub group: String,
    /// Reçue (sinon : envoyée).
    pub incoming: bool,
    /// Envoyée et bien arrivée (sinon : partira quand l'appareil sera en ligne).
    pub delivered: bool,
}

impl Inner {
    /// Nom affiché (surnom, pseudo…), sinon celui présenté dans le groupe.
    fn name_or(&self, id: &str, fallback: &str) -> String {
        self.display_name(id).unwrap_or_else(|| fallback.to_string())
    }

    fn save_friends(&self, friends: &FriendStore) {
        let _ = self.store.save(FILE, friends);
    }

    /// Relation avec chaque appareil (id + clé), pour les listes de membres.
    pub(crate) fn friend_states(&self) -> impl Fn(&MemberInfo, bool) -> FriendState {
        let trusted: HashMap<String, String> =
            lock(&self.trusted).iter().map(|(id, peer)| (id.clone(), peer.public_key.clone())).collect();
        let (sent, received): (HashSet<String>, HashSet<String>) = {
            let friends = lock(&self.friends);
            (
                friends.outgoing.iter().map(|r| r.id.clone()).collect(),
                friends.incoming.iter().map(|r| r.id.clone()).collect(),
            )
        };
        move |member: &MemberInfo, me: bool| {
            if me {
                FriendState::Me
            } else if trusted.get(&member.id).is_some_and(|key| key == &member.public_key) {
                FriendState::Friend
            } else if sent.contains(&member.id) {
                FriendState::Sent
            } else if received.contains(&member.id) {
                FriendState::Received
            } else {
                FriendState::None
            }
        }
    }

    /// Appareil à qui nous avons fait une demande, reconnu par sa clé (connexion entrante).
    pub(crate) fn friend_request_sender(&self, id: &str, public_key: &str) -> Option<SenderInfo> {
        let friends = lock(&self.friends);
        friends.outgoing.iter().find(|r| r.id == id && r.public_key == public_key).map(|r| SenderInfo {
            id: r.id.clone(),
            name: r.name.clone(),
            platform: r.platform.clone(),
            public_key: r.public_key.clone(),
            trusted: false,
        })
    }

    /// Associe cet appareil avec la clé présentée dans le groupe.
    fn befriend(&self, id: &str, name: &str, platform: &str, public_key: &str) {
        lock(&self.trusted).insert(
            id.to_string(),
            TrustedPeer { id: id.to_string(), name: name.to_string(), platform: platform.to_string(), public_key: public_key.to_string() },
        );
        lock(&self.refused).remove(id);
        let _ = self.save_trusted();
        self.chat_changed();
        self.touch();
    }

    /// Adresse actuelle de l'appareil `id`, s'il s'annonce avec cette clé.
    fn reachable(&self, id: &str, name: &str, public_key: &str) -> Option<Target> {
        let peers = lock(&self.peers);
        let peer = peers.get(id)?;
        if peer.device.public_key != public_key {
            return None;
        }
        Some(Target {
            id: id.to_string(),
            name: name.to_string(),
            route: peer.route()?,
            public_key: crypto::public_from_b64(public_key).ok()?,
        })
    }

    /// Un membre d'un groupe commun nous demande en ami. Réponse : `accepted` ou `pending`.
    pub(crate) fn receive_friend_request(&self, sender: &SenderInfo, conversation: &str) -> Result<String> {
        if sender.trusted {
            return Ok("accepted".to_string()); // déjà associés de notre côté
        }
        let Some(member) = self.group_member(conversation, &sender.id).filter(|m| m.public_key == sender.public_key) else {
            return Err(refusal("not-member", "vous n'êtes pas membre de ce groupe"));
        };
        let group = self.group_title(conversation);
        let mut friends = lock(&self.friends);
        // Demande croisée : chacun a demandé l'autre, c'est accepté des deux côtés.
        if let Some(index) = friends.outgoing.iter().position(|r| r.id == sender.id && r.public_key == sender.public_key) {
            friends.outgoing.remove(index);
            self.save_friends(&friends);
            drop(friends);
            self.befriend(&member.id, &member.name, &member.platform, &member.public_key);
            self.set_status(format!("{} et vous êtes maintenant amis", self.name_or(&member.id, &member.name)));
            return Ok("accepted".to_string());
        }
        friends.incoming.retain(|r| r.id != sender.id);
        friends.incoming.push(Request {
            id: member.id.clone(),
            name: member.name.clone(),
            platform: member.platform.clone(),
            public_key: member.public_key.clone(),
            conversation: conversation.to_string(),
            at_ms: now_ms(),
            delivered: true,
        });
        self.save_friends(&friends);
        drop(friends);
        let name = self.name_or(&member.id, &member.name);
        self.set_status(format!("{name} vous demande en ami (groupe « {group} ») : répondez dans la page Appareils"));
        self.notify(crate::engine::Notice {
            conversation: None,
            title: "Demande d'ami".to_string(),
            body: format!("{name} (groupe « {group} ») veut devenir votre ami"),
            ..crate::engine::Notice::default()
        });
        self.touch();
        Ok("pending".to_string())
    }

    /// Réponse à une de nos demandes.
    pub(crate) fn receive_friend_answer(&self, sender: &SenderInfo, accepted: bool) -> Result<()> {
        let request = {
            let mut friends = lock(&self.friends);
            let Some(index) = friends.outgoing.iter().position(|r| r.id == sender.id && r.public_key == sender.public_key) else {
                return Ok(()); // déjà traitée (ou annulée)
            };
            let request = friends.outgoing.remove(index);
            self.save_friends(&friends);
            request
        };
        let name = self.name_or(&request.id, &request.name);
        if accepted {
            self.befriend(&request.id, &request.name, &request.platform, &request.public_key);
            self.set_status(format!("{name} a accepté votre demande d'ami"));
        } else {
            self.set_status(format!("{name} a refusé votre demande d'ami"));
            self.touch();
        }
        Ok(())
    }

    fn deliver_request(&self, request: &Request) -> Result<()> {
        let target = self.reachable(&request.id, &request.name, &request.public_key).ok_or_else(|| anyhow!("hors ligne"))?;
        let mut channel = self.connect(&target)?;
        self.write_message(&mut channel, &Message::FriendRequest { conversation: request.conversation.clone() }, &[])?;
        let _ = channel.finish();
        let ack = read_ack(&mut channel).ok_or_else(|| anyhow!("{} ne répond pas", target.name))?;
        if !ack.ok {
            bail!("{}", explain_refusal(&ack, &target.name));
        }
        let mut friends = lock(&self.friends);
        let Some(index) = friends.outgoing.iter().position(|r| r.id == request.id) else { return Ok(()) };
        if ack.data == "accepted" {
            friends.outgoing.remove(index);
            self.save_friends(&friends);
            drop(friends);
            self.befriend(&request.id, &request.name, &request.platform, &request.public_key);
            self.set_status(format!("{} et vous êtes maintenant amis", self.name_or(&request.id, &request.name)));
        } else {
            friends.outgoing[index].delivered = true;
            self.save_friends(&friends);
            drop(friends);
            self.set_status(format!("Demande d'ami reçue par {} : en attente de sa réponse", self.name_or(&request.id, &request.name)));
            self.touch();
        }
        Ok(())
    }

    fn deliver_answer(&self, answer: &Answer) -> Result<()> {
        let target = self.reachable(&answer.id, &answer.name, &answer.public_key).ok_or_else(|| anyhow!("hors ligne"))?;
        let mut channel = self.connect(&target)?;
        self.write_message(&mut channel, &Message::FriendAnswer { accepted: answer.accepted }, &[])?;
        let _ = channel.finish();
        match read_ack(&mut channel) {
            Some(ack) if ack.ok => {
                let mut friends = lock(&self.friends);
                friends.answers.retain(|a| a.id != answer.id);
                self.save_friends(&friends);
                Ok(())
            }
            Some(ack) => bail!("{}", explain_refusal(&ack, &target.name)),
            None => bail!("{} ne répond pas", target.name),
        }
    }

    /// Envoie ce qui attend (demandes, réponses) aux appareils en ligne.
    fn deliver_pending(self: &Arc<Self>) {
        let (requests, answers) = {
            let mut friends = lock(&self.friends);
            let now = Instant::now();
            friends.retry.retain(|_, at| *at > now);
            let waiting = |key: &str, friends: &FriendStore| friends.retry.contains_key(key);
            let requests: Vec<Request> =
                friends.outgoing.iter().filter(|r| !r.delivered && !waiting(&format!("d:{}", r.id), &friends)).cloned().collect();
            let answers: Vec<Answer> =
                friends.answers.iter().filter(|a| !waiting(&format!("r:{}", a.id), &friends)).cloned().collect();
            (requests, answers)
        };
        for request in requests {
            if self.deliver_request(&request).is_err() {
                lock(&self.friends).retry.insert(format!("d:{}", request.id), Instant::now() + RETRY);
            }
        }
        for answer in answers {
            if self.deliver_answer(&answer).is_err() {
                lock(&self.friends).retry.insert(format!("r:{}", answer.id), Instant::now() + RETRY);
            }
        }
    }
}

/// Fil d'envoi des demandes et des réponses.
pub(crate) fn spawn_friend_scheduler(inner: Arc<Inner>) {
    thread::spawn(move || {
        while !inner.shutdown.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(500));
            inner.deliver_pending();
        }
    });
}

impl Engine {
    /// Demande à un membre du groupe `conversation` d'être ami (de s'associer).
    pub fn send_friend_request(&self, conversation: &str, member_id: &str) -> Result<()> {
        let inner = &self.inner;
        if member_id == inner.own_id() {
            bail!("c'est cet appareil");
        }
        let member = inner.group_member(conversation, member_id).ok_or_else(|| anyhow!("ce membre n'est plus dans le groupe"))?;
        if let Some(peer) = lock(&inner.trusted).get(member_id) {
            if peer.public_key == member.public_key {
                bail!("{} est déjà votre ami", inner.name_or(member_id, &member.name));
            }
            bail!("{} a une autre clé que celle de votre association : vérifiez-le dans la page Appareils", member.name);
        }
        // Il nous l'a déjà demandé : on accepte simplement.
        if lock(&inner.friends).incoming.iter().any(|r| r.id == member_id && r.public_key == member.public_key) {
            return self.answer_friend_request(member_id, true);
        }
        let mut friends = lock(&inner.friends);
        friends.outgoing.retain(|r| r.id != member_id);
        friends.retry.remove(&format!("d:{member_id}"));
        friends.outgoing.push(Request {
            id: member.id.clone(),
            name: member.name.clone(),
            platform: member.platform.clone(),
            public_key: member.public_key.clone(),
            conversation: conversation.to_string(),
            at_ms: now_ms(),
            delivered: false,
        });
        inner.save_friends(&friends);
        drop(friends);
        let name = inner.name_or(member_id, &member.name);
        inner.set_status(if inner.reachable(&member.id, &member.name, &member.public_key).is_some() {
            format!("Demande d'ami envoyée à {name}")
        } else {
            format!("Demande d'ami pour {name} : elle partira dès que cet appareil sera en ligne")
        });
        inner.chat_changed();
        inner.touch();
        Ok(())
    }

    /// Accepte (associe cet appareil) ou refuse une demande d'ami reçue.
    pub fn answer_friend_request(&self, member_id: &str, accept: bool) -> Result<()> {
        let inner = &self.inner;
        let request = {
            let mut friends = lock(&inner.friends);
            let index = friends.incoming.iter().position(|r| r.id == member_id).ok_or_else(|| anyhow!("demande introuvable"))?;
            let request = friends.incoming.remove(index);
            friends.answers.retain(|a| a.id != member_id);
            friends.answers.push(Answer {
                id: request.id.clone(),
                name: request.name.clone(),
                public_key: request.public_key.clone(),
                accepted: accept,
            });
            inner.save_friends(&friends);
            request
        };
        let name = inner.name_or(&request.id, &request.name);
        if accept {
            inner.befriend(&request.id, &request.name, &request.platform, &request.public_key);
            inner.set_status(format!("{name} et vous êtes maintenant amis"));
        } else {
            inner.set_status(format!("Demande d'ami de {name} refusée"));
            inner.chat_changed();
            inner.touch();
        }
        Ok(())
    }

    /// Retire une demande envoyée.
    pub fn cancel_friend_request(&self, member_id: &str) -> Result<()> {
        let inner = &self.inner;
        let mut friends = lock(&inner.friends);
        friends.outgoing.retain(|r| r.id != member_id);
        inner.save_friends(&friends);
        drop(friends);
        inner.set_status("Demande d'ami retirée");
        inner.chat_changed();
        inner.touch();
        Ok(())
    }

    /// Demandes reçues puis envoyées.
    pub fn friend_requests(&self) -> Vec<FriendRequestView> {
        let inner = &self.inner;
        let (incoming, outgoing) = {
            let friends = lock(&inner.friends);
            (friends.incoming.clone(), friends.outgoing.clone())
        };
        incoming
            .iter()
            .map(|r| (r, true))
            .chain(outgoing.iter().map(|r| (r, false)))
            .map(|(r, incoming)| FriendRequestView {
                id: r.id.clone(),
                name: inner.name_or(&r.id, &r.name),
                platform: r.platform.clone(),
                fingerprint: crypto::fingerprint(&r.public_key),
                group: inner.group_title(&r.conversation),
                incoming,
                delivered: r.delivered,
            })
            .collect()
    }
}
