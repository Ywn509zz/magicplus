//! Formats échangés sur le réseau local.
//!
//! - Découverte : un `Hello` JSON en UDP (diffusion + réponse directe).
//! - Tout le reste passe dans un canal chiffré Noise sur TCP (`secure.rs`). Un message applicatif
//!   = `u32 BE taille de l'en-tête` + en-tête JSON (`Message`) + octets bruts (morceau de fichier).
//!   Les réponses du destinataire sont des `Ack` JSON dans le même canal.

use serde::{Deserialize, Serialize};

/// Version du protocole. Les anciennes versions (`magicplus/1`, application Tauri) sont ignorées.
pub const PROTOCOL: &str = "magicplus/4";
pub const DEFAULT_DISCOVERY_PORT: u16 = 45454;
pub const DEFAULT_TRANSFER_PORT: u16 = 45455;
/// Taille d'un morceau de fichier (avant chiffrement).
pub const CHUNK_SIZE: usize = 256 * 1024;
/// Taille maximale d'une trame reçue (morceau + en-tête + chiffrement, avec de la marge).
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Taille maximale d'un texte de presse-papiers.
pub const MAX_CLIPBOARD_BYTES: usize = 2 * 1024 * 1024;
/// Chat : longueur maximale d'un message, nombre et taille des pièces jointes.
pub const MAX_CHAT_TEXT: usize = 20_000;
pub const MAX_ATTACHMENTS: usize = 15;
pub const MAX_ATTACHMENT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    /// Clé publique (base64). L'empreinte est toujours recalculée par le récepteur.
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    pub protocol: String,
    pub device: DeviceInfo,
    pub tcp_port: u16,
    pub discovery_port: u16,
    /// Vrai pour une réponse directe à une annonce : on n'y répond pas à son tour (pas de boucle).
    #[serde(default)]
    pub reply: bool,
    /// Version de l'application (« 0.4.0 »), pour expliquer une incompatibilité de protocole.
    /// Absente avant la 0.4.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app_version: String,
}

/// En-tête chiffré d'une trame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum Message {
    ClipboardText {
        id: String,
        text: String,
    },
    FileOffer {
        id: String,
        file_name: String,
        size: u64,
        /// SHA-256 du fichier entier, en base64.
        digest: String,
        /// Pièce jointe d'un message de discussion (rangée avec la conversation).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<ChatLink>,
    },
    /// Les octets du morceau suivent l'en-tête.
    FileChunk {
        id: String,
        sequence: u64,
    },
    FileComplete {
        id: String,
    },
    /// L'expéditeur abandonne ce transfert : le récepteur supprime le fichier partiel.
    FileCancel {
        id: String,
    },
    /// Événements d'une conversation écrits par l'expéditeur, dans l'ordre de leur numéro.
    /// Réponse : `Ack { offset: dernier numéro appliqué sans trou }`.
    ChatEvents {
        conversation: String,
        events: Vec<ChatEvent>,
    },
    /// « En train d'écrire » (éphémère, jamais enregistré).
    ChatTyping {
        conversation: String,
    },
    /// Accusé de lecture : tout jusqu'à cette horloge logique a été lu.
    ChatRead {
        conversation: String,
        lamport: u64,
    },
    /// Profil de l'expéditeur ; ses images (photo puis bannière) suivent l'en-tête.
    Profile {
        profile: ProfileInfo,
    },
}

/// Image d'un profil, envoyée à la suite de l'en-tête.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    /// « png », « jpg » ou « gif ».
    pub ext: String,
    pub size: u64,
    /// SHA-256 en base64.
    pub sha: String,
}

/// Profil d'un appareil (le nom affiché reste celui de l'appareil).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProfileInfo {
    /// Version (horodatage) : on ne garde que la plus récente.
    pub revision: u64,
    pub bio: String,
    pub pronouns: String,
    pub color: Option<u32>,
    pub avatar: Option<ImageInfo>,
    pub banner: Option<ImageInfo>,
}

/// Membre d'une conversation : sa clé est présentée par le créateur du groupe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub public_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttachmentKind {
    Image,
    Gif,
    Video,
    Sticker,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentInfo {
    pub id: String,
    pub file_name: String,
    pub size: u64,
    pub kind: AttachmentKind,
}

/// Lien entre un transfert de fichier et une pièce jointe de discussion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatLink {
    pub conversation: String,
    pub attachment: String,
}

/// Événement numéroté d'une conversation (message, modification, réaction, membres…).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEvent {
    /// Numéro chez son auteur : 1, 2, 3… sans trou. Garantit l'ordre et la complétude.
    pub seq: u64,
    /// Horloge logique (Lamport) : tous les membres affichent les messages dans le même ordre.
    pub lamport: u64,
    pub sent_ms: u64,
    pub kind: ChatKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum ChatKind {
    Post {
        id: String,
        text: String,
        #[serde(default)]
        reply_to: Option<String>,
        #[serde(default)]
        attachments: Vec<AttachmentInfo>,
    },
    Edit {
        target: String,
        text: String,
    },
    Delete {
        target: String,
    },
    React {
        target: String,
        emoji: String,
        add: bool,
    },
    /// Création ou mise à jour d'un groupe, toujours écrite par son créateur.
    GroupUpdate {
        name: String,
        members: Vec<MemberInfo>,
    },
    Leave,
}

/// Réponse du récepteur, une ligne JSON sur la connexion TCP :
/// - après une offre de fichier : `{"ok":true,"offset":N}` (N = octets déjà reçus, reprise) ;
/// - à la fin de la connexion : `{"ok":true}` ou `{"ok":false,"error":"…","code":"…"}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ack {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// Raison machine du refus : `not-paired`, `cancelled`, `busy`…
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
}

impl Ack {
    pub fn ok() -> Self {
        Self { ok: true, ..Self::default() }
    }

    pub fn offset(offset: u64) -> Self {
        Self { ok: true, offset: Some(offset), ..Self::default() }
    }

    pub fn refused(error: impl Into<String>, code: impl Into<String>) -> Self {
        Self { ok: false, error: error.into(), code: code.into(), offset: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trip() {
        let message = Message::FileOffer {
            id: "abc".into(),
            file_name: "a.txt".into(),
            size: 12,
            digest: "d".into(),
            chat: None,
        };
        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains("\"kind\":\"file-offer\""));
        assert!(json.contains("\"fileName\":\"a.txt\""));
        let back: Message = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Message::FileOffer { size: 12, .. }));
        assert!(!json.contains("chat"), "champ absent quand il n'y a pas de discussion");
    }

    #[test]
    fn chat_event_round_trip() {
        let event = ChatEvent {
            seq: 3,
            lamport: 9,
            sent_ms: 1,
            kind: ChatKind::Post { id: "m".into(), text: "**salut**".into(), reply_to: None, attachments: vec![] },
        };
        let json = serde_json::to_string(&Message::ChatEvents { conversation: "c".into(), events: vec![event.clone()] }).unwrap();
        assert!(json.contains("\"type\":\"post\""));
        let Message::ChatEvents { events, .. } = serde_json::from_str(&json).unwrap() else { panic!() };
        assert_eq!(events, vec![event]);
    }
}
