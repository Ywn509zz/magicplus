//! Cœur de MagicPlus : réseau local, chiffrement, presse-papiers et transferts de fichiers.

pub mod animation;
pub mod autostart;
pub mod canvas;
pub mod chat;
pub mod crypto;
pub mod emoji;
pub mod engine;
pub mod giphy;
pub mod firewall;
pub mod friends;
pub mod highlight;
pub mod markdown;
mod net;
pub mod presence;
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
pub use emoji::{CustomEmojiView, EmojiPrefs};
pub use giphy::GifResult;
pub use presence::{GroupNotifications, NotificationPrefs, Presence};
pub use profile::{ProfileImage, ProfileView};
pub use update::{UpdateState, UpdateView};
pub use firewall::FirewallState;
pub use friends::{FriendRequestView, FriendState};
pub use engine::{
    ClipboardView, Config, Engine, IdentityView, ManualView, Notice, OtherVersionView, PeerView, ReceivedView, Snapshot,
    TransferState, TransferView,
};
