//! Profils → `ProfileItem` (ui/profile.slint).

use super::{chat::avatar_color, images::still, markdown, FullProfileItem, GroupLink, ProfileItem};
use crate::shared::{initials, platform_label};
use magicplus_core::{FriendState, ProfileDetails, ProfileView};
use slint::{Color, ModelRc, VecModel};

pub fn profile_item(profile: &ProfileView, can_chat: bool) -> ProfileItem {
    let photo = profile.avatar.as_deref().and_then(|path| still(path, 192));
    let banner = profile.banner.as_deref().and_then(|path| still(path, 720));
    let banner_color = match profile.color {
        Some(rgb) => Color::from_rgb_u8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8),
        None => avatar_color(&profile.id),
    };
    ProfileItem {
        id: profile.id.clone().into(),
        name: profile.name.clone().into(),
        pronouns: profile.pronouns.clone().into(),
        bio: markdown::blocks(&profile.bio),
        has_bio: !profile.bio.trim().is_empty(),
        has_banner: banner.is_some(),
        banner: banner.unwrap_or_default(),
        banner_color,
        has_photo: photo.is_some(),
        photo: photo.unwrap_or_default(),
        initials: initials(&profile.name).into(),
        color: avatar_color(&profile.id),
        me: profile.me,
        can_chat,
    }
}

/// Profil en grand.
pub fn full_item(details: &ProfileDetails) -> FullProfileItem {
    let profile = &details.profile;
    let mut identity = Vec::new();
    if !details.pseudo.is_empty() && details.pseudo != profile.name {
        identity.push(details.pseudo.clone());
    }
    if details.device_name != profile.name {
        identity.push(details.device_name.clone());
    }
    identity.push(platform_label(&details.platform).to_string());
    identity.push(if details.online { "en ligne".to_string() } else { "hors ligne".to_string() });
    let groups: Vec<GroupLink> =
        details.groups.iter().map(|(id, name)| GroupLink { id: id.clone().into(), name: name.clone().into() }).collect();
    let mut item = profile_item(profile, details.trusted);
    // Bannière plus grande que dans la carte.
    if let Some(banner) = profile.banner.as_deref().and_then(|path| still(path, 1120)) {
        item.banner = banner;
        item.has_banner = true;
    }
    FullProfileItem {
        profile: item,
        identity: identity.join("  ·  ").into(),
        online: details.online,
        fingerprint: details.fingerprint.clone().into(),
        groups: ModelRc::new(VecModel::from(groups)),
        can_message: details.trusted && !profile.me,
        friend_action: match details.friend {
            FriendState::None if !details.trusted && !details.groups.is_empty() => 1,
            FriendState::Sent => 2,
            FriendState::Received => 3,
            _ => 0,
        },
    }
}
