//! Cœur de MagicPlus : réseau local, chiffrement, presse-papiers et transferts de fichiers.

pub mod chat;
pub mod crypto;
pub mod engine;
pub mod highlight;
pub mod markdown;
mod net;
pub mod protocol;
mod secure;
pub mod store;
mod transfer;
pub mod update;
pub mod util;

pub use chat::{
    AttachmentView, ConversationKind, ConversationView, MemberView, MessageStatus, MessageView, ReactionView, ReplyView,
    StickerView,
};
pub use protocol::AttachmentKind;
pub use update::{UpdateState, UpdateView};
pub use engine::{
    ClipboardView, Config, Engine, IdentityView, ManualView, OtherVersionView, PeerView, ReceivedView, Snapshot,
    TransferState, TransferView,
};
