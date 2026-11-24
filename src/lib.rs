//! Cœur de MagicPlus : réseau local, chiffrement, presse-papiers et transferts de fichiers.

pub mod autostart;
mod bluetooth;
pub mod canvas;
pub mod chat;
pub mod crypto;
pub mod engine;
pub mod firewall;
pub mod friends;
pub mod highlight;
pub mod markdown;
mod link;
mod net;
pub mod profile;
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
pub use canvas::{CanvasVersionView, CanvasView};
pub use profile::{ProfileImage, ProfileView};
pub use update::{UpdateState, UpdateView};
pub use firewall::FirewallState;
pub use friends::{FriendRequestView, FriendState};
pub use engine::{
    ClipboardView, Config, Engine, IdentityView, ManualView, Notice, OtherVersionView, PeerView, ReceivedView, Snapshot,
    TransferState, TransferView,
};
