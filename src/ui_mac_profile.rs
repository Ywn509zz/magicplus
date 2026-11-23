//! Profils dans l'interface Mac : photos rondes, carte de profil façon Discord (bannière, photo,
//! pronoms, « À propos de moi ») et réglages « Mon profil ».

use super::markdown_view::{fill_view, markdown_view};
use super::*;
use crate::shared::avatar_hue;
use magicplus_core::{ProfileImage, ProfileView};
use objc2::AnyThread;
use objc2_app_kit::{NSPopover, NSPopoverBehavior, NSViewController};
use objc2_foundation::NSRectEdge;
use std::path::Path;

/// Couleurs proposées pour la bannière (sans image).
pub(super) const BANNER_COLORS: [(&str, u32); 10] = [
    ("Rouge", 0xE5484D),
    ("Orange", 0xF76B15),
    ("Jaune", 0xE2A300),
    ("Vert", 0x30A46C),
    ("Cyan", 0x12A5B5),
    ("Bleu", 0x3E7BFA),
    ("Violet", 0x8E4EC6),
    ("Rose", 0xD6409F),
    ("Marron", 0xA0714F),
    ("Gris", 0x8B8D98),
];

/// Couleur de bannière par défaut : la teinte de l'avatar.
fn default_color(seed: &str) -> u32 {
    let color = NSColor::colorWithHue_saturation_brightness_alpha(avatar_hue(seed) as f64, 0.45, 0.72, 1.0);
    let rgb = color.colorUsingColorSpace(&objc2_app_kit::NSColorSpace::sRGBColorSpace());
    rgb.map(|c| {
        let channel = |v: f64| ((v * 255.0).round() as u32).min(255);
        channel(c.redComponent()) << 16 | channel(c.greenComponent()) << 8 | channel(c.blueComponent())
    })
    .unwrap_or(0x5865F2)
}

/// Image ronde (photo de profil) ; un GIF reste animé.
pub(super) fn round_image(path: &Path, size: f64, mtm: MainThreadMarker) -> Option<Retained<NSView>> {
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(&path.to_string_lossy()))?;
    let view = NSImageView::imageViewWithImage(&image, mtm);
    view.setAnimates(true);
    view.setImageScaling(NSImageScaling::ScaleAxesIndependently);
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.widthAnchor().constraintEqualToConstant(size).setActive(true);
    view.heightAnchor().constraintEqualToConstant(size).setActive(true);
    view.setWantsLayer(true);
    if let Some(layer) = view.layer() {
        layer.setCornerRadius(size / 2.0);
        layer.setMasksToBounds(true);
    }
    Some(Retained::into_super(Retained::into_super(view)))
}

/// Carte de profil (sans boutons) de largeur `width`.
pub(super) fn profile_card(profile: &ProfileView, width: f64, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let card = stack(true, 0.0, mtm);
    card.widthAnchor().constraintEqualToConstant(width).setActive(true);

    // Bannière et photo qui déborde dessus.
    let banner_height = (width * 0.4).round();
    let top = NSView::new(mtm);
    top.setTranslatesAutoresizingMaskIntoConstraints(false);
    top.heightAnchor().constraintEqualToConstant(banner_height + 44.0).setActive(true);
    let banner: Retained<NSView> = match profile.banner.as_deref().and_then(|path| {
        NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(&path.to_string_lossy()))
    }) {
        Some(image) => {
            let view = NSImageView::imageViewWithImage(&image, mtm);
            view.setAnimates(true);
            view.setImageScaling(NSImageScaling::ScaleAxesIndependently);
            Retained::into_super(Retained::into_super(view))
        }
        None => Retained::into_super(fill_view(0, profile.color.unwrap_or_else(|| default_color(&profile.id)), 0.0, mtm)),
    };
    banner.setTranslatesAutoresizingMaskIntoConstraints(false);
    top.addSubview(&banner);
    banner.topAnchor().constraintEqualToAnchor(&top.topAnchor()).setActive(true);
    banner.leadingAnchor().constraintEqualToAnchor(&top.leadingAnchor()).setActive(true);
    banner.trailingAnchor().constraintEqualToAnchor(&top.trailingAnchor()).setActive(true);
    banner.heightAnchor().constraintEqualToConstant(banner_height).setActive(true);

    // Anneau autour de la photo, de la couleur du fond.
    let ring = fill_view(3, 0, 44.0, mtm);
    ring.widthAnchor().constraintEqualToConstant(88.0).setActive(true);
    ring.heightAnchor().constraintEqualToConstant(88.0).setActive(true);
    top.addSubview(&ring);
    ring.leadingAnchor().constraintEqualToAnchor_constant(&top.leadingAnchor(), 14.0).setActive(true);
    ring.bottomAnchor().constraintEqualToAnchor(&top.bottomAnchor()).setActive(true);
    let photo = super::chat_page::avatar(&profile.name, &profile.id, 80.0, profile.avatar.as_deref(), mtm);
    ring.addSubview(&photo);
    photo.centerXAnchor().constraintEqualToAnchor(&ring.centerXAnchor()).setActive(true);
    photo.centerYAnchor().constraintEqualToAnchor(&ring.centerYAnchor()).setActive(true);
    card.addArrangedSubview(&top);
    top.widthAnchor().constraintEqualToAnchor(&card.widthAnchor()).setActive(true);

    let texts = stack(true, 4.0, mtm);
    texts.setEdgeInsets(NSEdgeInsets { top: 8.0, left: 16.0, bottom: 14.0, right: 16.0 });
    texts.addArrangedSubview(&strong(&profile.name, 18.0, mtm));
    if !profile.pronouns.is_empty() {
        texts.addArrangedSubview(&secondary(&profile.pronouns, mtm));
    }
    if !profile.bio.is_empty() {
        let line = NSBox::new(mtm);
        line.setBoxType(NSBoxType::Separator);
        line.setTranslatesAutoresizingMaskIntoConstraints(false);
        texts.addArrangedSubview(&line);
        line.widthAnchor().constraintEqualToConstant(width - 32.0).setActive(true);
        let title = strong("À PROPOS DE MOI", 10.5, mtm);
        title.setTextColor(Some(&NSColor::secondaryLabelColor()));
        texts.addArrangedSubview(&title);
        texts.addArrangedSubview(&markdown_view(&profile.bio, 12.5, width - 32.0, mtm));
    }
    full_width(&card, &texts);
    card
}

impl Controller {
    /// Carte de profil dans une bulle surgissante, à côté de `anchor`.
    pub(super) fn show_profile(&self, id: &str, anchor: &NSView) {
        let mtm = self.mtm();
        let Some(profile) = self.engine().profile(id) else { return };
        let card = profile_card(&profile, 300.0, mtm);
        let buttons = stack(false, 8.0, mtm);
        buttons.setEdgeInsets(NSEdgeInsets { top: 0.0, left: 16.0, bottom: 14.0, right: 16.0 });
        if profile.me {
            buttons.addArrangedSubview(&button("Modifier mon profil", sel!(profileEdit:), self, mtm));
        } else if self.engine().snapshot().peers.iter().any(|p| p.id == profile.id && p.trusted) {
            buttons.addArrangedSubview(&button("Discuter", sel!(profileDirect:), self, mtm));
        }
        if !buttons.arrangedSubviews().is_empty() {
            card.addArrangedSubview(&buttons);
        }
        card.layoutSubtreeIfNeeded();
        let size = card.fittingSize();
        card.setFrameSize(NSSize::new(300.0, size.height));

        let controller = NSViewController::new(mtm);
        controller.setView(&card);
        let popover = NSPopover::new(mtm);
        popover.setBehavior(NSPopoverBehavior::Transient);
        popover.setContentViewController(Some(&controller));
        popover.setContentSize(NSSize::new(300.0, size.height));
        popover.showRelativeToRect_ofView_preferredEdge(anchor.bounds(), anchor, NSRectEdge::MaxX);
        *self.ivars().profile_popover.borrow_mut() = Some((popover, profile.id));
    }

    pub(super) fn close_profile_popover(&self) -> Option<String> {
        let (popover, id) = self.ivars().profile_popover.borrow_mut().take()?;
        popover.close();
        Some(id)
    }

    // ---------------------------------------------------------------- réglages « Mon profil »

    pub(super) fn profile_choose(&self, kind: ProfileImage) {
        let title = match kind {
            ProfileImage::Avatar => "Photo de profil",
            ProfileImage::Banner => "Bannière du profil",
        };
        let picked = rfd::FileDialog::new()
            .set_title(title)
            .add_filter("Images et GIF", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
            .pick_file();
        if let Some(path) = picked {
            match self.engine().set_profile_image(kind, &path) {
                Ok(()) => self.engine().set_status("Profil mis à jour : il sera envoyé à vos contacts"),
                Err(error) => self.engine().set_status(format!("Image refusée : {error}")),
            }
        }
    }

    pub(super) fn profile_remove(&self, kind: ProfileImage) {
        if let Err(error) = self.engine().remove_profile_image(kind) {
            self.engine().set_status(error.to_string());
        }
    }

    pub(super) fn profile_save(&self) {
        let widgets = self.widgets();
        let bio = widgets.profile_bio.stringValue().to_string();
        let pronouns = widgets.profile_pronouns.stringValue().to_string();
        let color = match widgets.profile_color.indexOfSelectedItem() {
            index if index > 0 => BANNER_COLORS.get(index as usize - 1).map(|(_, rgb)| *rgb),
            _ => None,
        };
        match self.engine().set_profile_details(&bio, &pronouns, color) {
            Ok(()) => self.engine().set_status("Profil enregistré : il sera envoyé à vos contacts"),
            Err(error) => self.engine().set_status(error.to_string()),
        }
    }

    /// Met à jour l'aperçu (et les champs au premier affichage).
    pub(super) fn refresh_profile_settings(&self, first: bool) {
        let revision = self.engine().profiles_revision();
        if !first && self.ivars().profile_revision.get() == revision {
            return;
        }
        self.ivars().profile_revision.set(revision);
        let mtm = self.mtm();
        let widgets = self.widgets();
        let profile = self.engine().my_profile();
        clear(&widgets.profile_preview);
        let preview = fill_view(4, 0, 10.0, mtm);
        let card = profile_card(&profile, 300.0, mtm);
        preview.addSubview(&card);
        pin(&card, &preview, 0.0, 0.0, 0.0);
        widgets.profile_preview.addArrangedSubview(&preview);
        widgets.profile_remove_photo.setEnabled(profile.avatar.is_some());
        widgets.profile_remove_banner.setEnabled(profile.banner.is_some());
        if first {
            widgets.profile_bio.setStringValue(&ns(&profile.bio));
            widgets.profile_pronouns.setStringValue(&ns(&profile.pronouns));
            let index = profile.color.and_then(|c| BANNER_COLORS.iter().position(|(_, rgb)| *rgb == c)).map(|i| i + 1).unwrap_or(0);
            widgets.profile_color.selectItemAtIndex(index as isize);
        }
    }
}
