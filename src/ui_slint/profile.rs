//! Profils → `ProfileItem` (ui/profile.slint).

use super::{chat::avatar_color, images::still, markdown, ProfileItem};
use crate::shared::initials;
use magicplus_core::ProfileView;
use slint::Color;

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
