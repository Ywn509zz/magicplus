//! Profils dans l'interface Mac : photos rondes, carte de profil façon Discord (bannière, photo,
//! pronoms, « À propos de moi ») et réglages « Mon profil ».

use super::markdown_view::{fill_view, markdown_view};
use super::*;
use crate::shared::avatar_hue;
use magicplus_core::{Crop, CropSource, FriendState, ProfileImage, ProfileView};
use objc2::AnyThread;
use objc2_app_kit::{NSPopover, NSPopoverBehavior, NSSlider, NSViewController};
use objc2_foundation::{NSData, NSRectEdge};
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

/// Image qui remplit `width` × `height` sans être déformée (les bords dépassent et sont coupés,
/// comme « object-fit: cover ») ; un GIF reste animé. Rayon des coins : `radius`.
pub(super) fn cover_image(image: &NSImage, width: f64, height: f64, radius: f64, mtm: MainThreadMarker) -> Retained<NSView> {
    let frame = NSView::new(mtm);
    frame.setTranslatesAutoresizingMaskIntoConstraints(false);
    frame.widthAnchor().constraintEqualToConstant(width).setActive(true);
    frame.heightAnchor().constraintEqualToConstant(height).setActive(true);
    frame.setWantsLayer(true);
    if let Some(layer) = frame.layer() {
        layer.setCornerRadius(radius);
        layer.setMasksToBounds(true);
    }
    let size = image.size();
    let aspect = if size.height > 0.0 { size.width / size.height } else { width / height.max(1.0) };
    let (inner_w, inner_h) = if aspect > width / height { (height * aspect, height) } else { (width, width / aspect.max(0.01)) };
    let view = NSImageView::imageViewWithImage(image, mtm);
    view.setAnimates(true);
    view.setImageScaling(NSImageScaling::ScaleAxesIndependently);
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    frame.addSubview(&view);
    view.widthAnchor().constraintEqualToConstant(inner_w).setActive(true);
    view.heightAnchor().constraintEqualToConstant(inner_h).setActive(true);
    view.centerXAnchor().constraintEqualToAnchor(&frame.centerXAnchor()).setActive(true);
    view.centerYAnchor().constraintEqualToAnchor(&frame.centerYAnchor()).setActive(true);
    frame
}

/// Image ronde (photo de profil) ; un GIF reste animé.
pub(super) fn round_image(path: &Path, size: f64, mtm: MainThreadMarker) -> Option<Retained<NSView>> {
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(&path.to_string_lossy()))?;
    Some(cover_image(&image, size, size, size / 2.0, mtm))
}

/// Bannière (image ou couleur) de `width` × `height`.
fn banner_view(profile: &ProfileView, width: f64, height: f64, mtm: MainThreadMarker) -> Retained<NSView> {
    match profile.banner.as_deref().and_then(|path| NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(&path.to_string_lossy()))) {
        Some(image) => cover_image(&image, width, height, 0.0, mtm),
        None => Retained::into_super(fill_view(0, profile.color.unwrap_or_else(|| default_color(&profile.id)), 0.0, mtm)),
    }
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
    let banner = banner_view(profile, width, banner_height, mtm);
    banner.setTranslatesAutoresizingMaskIntoConstraints(false);
    top.addSubview(&banner);
    banner.topAnchor().constraintEqualToAnchor(&top.topAnchor()).setActive(true);
    banner.leadingAnchor().constraintEqualToAnchor(&top.leadingAnchor()).setActive(true);
    if profile.banner.is_none() {
        banner.trailingAnchor().constraintEqualToAnchor(&top.trailingAnchor()).setActive(true);
        banner.heightAnchor().constraintEqualToConstant(banner_height).setActive(true);
    }

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

/// Recadrage en cours (fenêtre « Recadrer »).
pub(super) struct CropEditor {
    source: CropSource,
    kind: ProfileImage,
    preview: Retained<NSImageView>,
    zoom: Retained<NSSlider>,
    x: Retained<NSSlider>,
    y: Retained<NSSlider>,
}

impl CropEditor {
    fn crop(&self) -> Crop {
        Crop { zoom: self.zoom.doubleValue() as f32, x: self.x.doubleValue() as f32, y: self.y.doubleValue() as f32 }
    }
}

/// Profil en grand ouvert : sa fenêtre et les actions de ses boutons.
pub(super) struct FullProfile {
    window: Retained<NSWindow>,
    actions: Vec<super::menus::Action>,
}

impl Controller {
    /// Profil en grand, comme sur Discord : grande bannière, photo, nom, pseudo, pronoms,
    /// « À propos de moi », groupes en commun, appareil, et les actions possibles.
    pub(super) fn show_full_profile(&self, id: &str) {
        let mtm = self.mtm();
        let Some(details) = self.engine().profile_details(id) else { return };
        let profile = &details.profile;
        if let Some(open) = self.ivars().full_profile.borrow_mut().take() {
            open.window.close();
        }
        const WIDTH: f64 = 560.0;
        let mut actions: Vec<super::menus::Action> = Vec::new();
        let content = stack(true, 0.0, mtm);
        content.widthAnchor().constraintEqualToConstant(WIDTH).setActive(true);

        // Bannière et grande photo.
        let banner_height = 224.0;
        let top = NSView::new(mtm);
        top.setTranslatesAutoresizingMaskIntoConstraints(false);
        top.heightAnchor().constraintEqualToConstant(banner_height + 70.0).setActive(true);
        let banner = banner_view(profile, WIDTH, banner_height, mtm);
        banner.setTranslatesAutoresizingMaskIntoConstraints(false);
        top.addSubview(&banner);
        banner.topAnchor().constraintEqualToAnchor(&top.topAnchor()).setActive(true);
        banner.leadingAnchor().constraintEqualToAnchor(&top.leadingAnchor()).setActive(true);
        if profile.banner.is_none() {
            banner.trailingAnchor().constraintEqualToAnchor(&top.trailingAnchor()).setActive(true);
            banner.heightAnchor().constraintEqualToConstant(banner_height).setActive(true);
        }
        let ring = fill_view(3, 0, 70.0, mtm);
        ring.widthAnchor().constraintEqualToConstant(140.0).setActive(true);
        ring.heightAnchor().constraintEqualToConstant(140.0).setActive(true);
        top.addSubview(&ring);
        ring.leadingAnchor().constraintEqualToAnchor_constant(&top.leadingAnchor(), 22.0).setActive(true);
        ring.bottomAnchor().constraintEqualToAnchor(&top.bottomAnchor()).setActive(true);
        let photo = super::chat_page::avatar(&profile.name, &profile.id, 128.0, profile.avatar.as_deref(), mtm);
        ring.addSubview(&photo);
        photo.centerXAnchor().constraintEqualToAnchor(&ring.centerXAnchor()).setActive(true);
        photo.centerYAnchor().constraintEqualToAnchor(&ring.centerYAnchor()).setActive(true);
        let presence = fill_view(0, if details.online { 0x23A55A } else { 0x80848E }, 14.0, mtm);
        presence.widthAnchor().constraintEqualToConstant(28.0).setActive(true);
        presence.heightAnchor().constraintEqualToConstant(28.0).setActive(true);
        top.addSubview(&presence);
        presence.trailingAnchor().constraintEqualToAnchor_constant(&ring.trailingAnchor(), -8.0).setActive(true);
        presence.bottomAnchor().constraintEqualToAnchor_constant(&ring.bottomAnchor(), -8.0).setActive(true);

        // Boutons à droite, sous la bannière.
        let buttons = stack(false, 8.0, mtm);
        let mut add_button = |title: &str, primary: bool, action: super::menus::Action| {
            let tag = actions.len() as isize;
            actions.push(action);
            let item = button(title, sel!(fullProfileAction:), self, mtm);
            item.setTag(tag);
            if primary {
                item.setKeyEquivalent(&ns("\r"));
            }
            buttons.addArrangedSubview(&item);
        };
        let target = profile.id.clone();
        if profile.me {
            add_button("Modifier mon profil", true, Box::new(|c: &Controller| {
                c.close_full_profile();
                c.show_main_window();
                c.show_page(SETTINGS_PAGE);
            }));
        } else {
            if details.trusted {
                let id = target.clone();
                add_button("Envoyer un message", true, Box::new(move |c: &Controller| {
                    c.close_full_profile();
                    c.show_main_window();
                    c.chat_with(&id);
                }));
            }
            match (details.friend, details.groups.first()) {
                (FriendState::None, Some((group, _))) if !details.trusted => {
                    let (id, group) = (target.clone(), group.clone());
                    add_button("Ajouter en ami", !details.trusted, Box::new(move |c: &Controller| {
                        if let Err(error) = c.engine().send_friend_request(&group, &id) {
                            c.engine().set_status(error.to_string());
                        }
                        c.show_full_profile(&id);
                    }));
                }
                (FriendState::Received, _) => {
                    let id = target.clone();
                    add_button("Accepter sa demande d'ami", true, Box::new(move |c: &Controller| {
                        if let Err(error) = c.engine().answer_friend_request(&id, true) {
                            c.engine().set_status(error.to_string());
                        }
                        c.show_full_profile(&id);
                    }));
                }
                (FriendState::Sent, _) => add_button("Demande d'ami envoyée", false, Box::new(|_c: &Controller| {})),
                _ => {}
            }
        }
        top.addSubview(&buttons);
        buttons.trailingAnchor().constraintEqualToAnchor_constant(&top.trailingAnchor(), -20.0).setActive(true);
        buttons.bottomAnchor().constraintEqualToAnchor_constant(&top.bottomAnchor(), -6.0).setActive(true);
        content.addArrangedSubview(&top);
        top.widthAnchor().constraintEqualToAnchor(&content.widthAnchor()).setActive(true);

        // Nom, pseudo, pronoms, puis les sections.
        let texts = stack(true, 6.0, mtm);
        texts.setEdgeInsets(NSEdgeInsets { top: 12.0, left: 24.0, bottom: 24.0, right: 24.0 });
        let inner_width = WIDTH - 48.0;
        texts.addArrangedSubview(&strong(&profile.name, 26.0, mtm));
        let mut identity = Vec::new();
        if !details.pseudo.is_empty() && details.pseudo != profile.name {
            identity.push(details.pseudo.clone());
        }
        if details.device_name != profile.name {
            identity.push(details.device_name.clone());
        }
        identity.push(platform_label(&details.platform).to_string());
        identity.push(if details.online { "en ligne".to_string() } else { "hors ligne".to_string() });
        texts.addArrangedSubview(&secondary(&identity.join("  ·  "), mtm));
        if !profile.pronouns.is_empty() {
            texts.addArrangedSubview(&label(&profile.pronouns, mtm));
        }
        let section = |title: &str| {
            let line = NSBox::new(mtm);
            line.setBoxType(NSBoxType::Separator);
            line.setTranslatesAutoresizingMaskIntoConstraints(false);
            texts.addArrangedSubview(&line);
            line.widthAnchor().constraintEqualToConstant(inner_width).setActive(true);
            let heading = strong(title, 11.0, mtm);
            heading.setTextColor(Some(&NSColor::secondaryLabelColor()));
            texts.addArrangedSubview(&heading);
        };
        if !profile.bio.trim().is_empty() {
            section("À PROPOS DE MOI");
            texts.addArrangedSubview(&markdown_view(&profile.bio, 14.0, inner_width, mtm));
        }
        if !details.groups.is_empty() {
            section(&format!("GROUPES EN COMMUN — {}", details.groups.len()));
            for (conversation, name) in &details.groups {
                let tag = actions.len() as isize;
                let conversation = conversation.clone();
                actions.push(Box::new(move |c: &Controller| {
                    c.close_full_profile();
                    c.show_main_window();
                    c.show_page(CHAT_PAGE);
                    c.open_conversation(&conversation);
                }));
                let item = button(&format!("👥  {}", if name.is_empty() { "Groupe" } else { name }), sel!(fullProfileAction:), self, mtm);
                item.setTag(tag);
                item.setBezelStyle(objc2_app_kit::NSBezelStyle::AccessoryBar);
                texts.addArrangedSubview(&item);
            }
        }
        section("APPAREIL");
        texts.addArrangedSubview(&secondary(&format!("Empreinte  {}", details.fingerprint), mtm));
        full_width(&content, &texts);
        content.layoutSubtreeIfNeeded();
        let size = content.fittingSize();

        // Fenêtre : défilement si l'écran est petit.
        let height = size.height.min(720.0);
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, height)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&ns(&format!("Profil de {}", profile.name)));
        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setDrawsBackground(false);
        let document = FlippedView::new(mtm);
        document.setFrameSize(NSSize::new(WIDTH, size.height));
        document.addSubview(&content);
        content.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, size.height)));
        scroll.setDocumentView(Some(&document));
        window.setContentView(Some(&scroll));
        window.center();
        window.makeKeyAndOrderFront(None);
        *self.ivars().full_profile.borrow_mut() = Some(FullProfile { window, actions });
    }

    pub(super) fn full_profile_action(&self, tag: isize) {
        let action = {
            let mut open = self.ivars().full_profile.borrow_mut();
            let Some(open) = open.as_mut() else { return };
            let index = tag.max(0) as usize;
            if index >= open.actions.len() {
                return;
            }
            // L'action peut rouvrir la fenêtre : on la sort de la liste avant de l'appeler.
            std::mem::replace(&mut open.actions[index], Box::new(|_c: &Controller| {}))
        };
        action(self);
    }

    pub(super) fn close_full_profile(&self) {
        if let Some(open) = self.ivars().full_profile.borrow_mut().take() {
            open.window.close();
        }
    }

    /// Carte de profil dans une bulle surgissante, à côté de `anchor`.
    pub(super) fn show_profile(&self, id: &str, anchor: &NSView) {
        let mtm = self.mtm();
        let Some(profile) = self.engine().profile(id) else { return };
        let card = profile_card(&profile, 300.0, mtm);
        let buttons = stack(false, 8.0, mtm);
        buttons.setEdgeInsets(NSEdgeInsets { top: 0.0, left: 16.0, bottom: 14.0, right: 16.0 });
        buttons.addArrangedSubview(&button("Profil complet", sel!(profileFull:), self, mtm));
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
            .add_filter("Images et GIF", &["png", "jpg", "jpeg", "gif", "bmp"])
            .pick_file();
        let Some(path) = picked else { return };
        let source = match CropSource::open(&path) {
            Ok(source) => source,
            Err(error) => {
                self.engine().set_status(format!("Image refusée : {error}"));
                return;
            }
        };
        let Some(crop) = self.run_crop_editor(source, kind) else { return };
        // Un GIF animé se recadre image par image : hors du fil de l'interface.
        self.engine().set_status("Préparation de l'image…");
        let engine = self.engine().clone();
        thread::spawn(move || match engine.set_profile_image_cropped(kind, &path, crop) {
            Ok(()) => engine.set_status("Profil mis à jour : il sera envoyé à vos contacts"),
            Err(error) => engine.set_status(format!("Image refusée : {error}")),
        });
    }

    /// Fenêtre de recadrage : aperçu, zoom et position. `None` si l'utilisateur annule.
    fn run_crop_editor(&self, source: CropSource, kind: ProfileImage) -> Option<Crop> {
        let mtm = self.mtm();
        let (width, height) = match kind {
            ProfileImage::Avatar => (220.0, 220.0),
            ProfileImage::Banner => (420.0, 168.0),
        };
        let column = stack(true, 10.0, mtm);
        let preview = NSImageView::new(mtm);
        preview.setImageScaling(NSImageScaling::ScaleAxesIndependently);
        preview.setTranslatesAutoresizingMaskIntoConstraints(false);
        preview.widthAnchor().constraintEqualToConstant(width).setActive(true);
        preview.heightAnchor().constraintEqualToConstant(height).setActive(true);
        preview.setWantsLayer(true);
        if let Some(layer) = preview.layer() {
            layer.setCornerRadius(if kind == ProfileImage::Avatar { width / 2.0 } else { 8.0 });
            layer.setMasksToBounds(true);
        }
        column.addArrangedSubview(&preview);
        let slider = |title: &str, value: f64, min: f64, max: f64| {
            let row = stack(false, 8.0, mtm);
            let name = label(title, mtm);
            name.setTranslatesAutoresizingMaskIntoConstraints(false);
            name.widthAnchor().constraintEqualToConstant(110.0).setActive(true);
            row.addArrangedSubview(&name);
            let control = unsafe {
                NSSlider::sliderWithValue_minValue_maxValue_target_action(value, min, max, Some(as_target(self)), Some(sel!(cropChanged:)), mtm)
            };
            control.setContinuous(true);
            control.setTranslatesAutoresizingMaskIntoConstraints(false);
            control.widthAnchor().constraintEqualToConstant(width.max(300.0) - 118.0).setActive(true);
            row.addArrangedSubview(&control);
            column.addArrangedSubview(&row);
            control
        };
        let zoom = slider("Zoom", 1.0, 1.0, 4.0);
        let x = slider("Gauche ↔ droite", 0.5, 0.0, 1.0);
        let y = slider("Haut ↔ bas", 0.5, 0.0, 1.0);
        if source.animated {
            column.addArrangedSubview(&secondary("GIF animé : il restera animé (aperçu de la première image).", mtm));
        }
        column.setFrameSize(column.fittingSize());
        *self.ivars().crop_editor.borrow_mut() = Some(CropEditor { source, kind, preview, zoom, x, y });
        self.crop_changed();

        let alert = NSAlert::new(mtm);
        alert.setMessageText(&ns(match kind {
            ProfileImage::Avatar => "Recadrer la photo de profil",
            ProfileImage::Banner => "Recadrer la bannière",
        }));
        alert.setInformativeText(&ns("Réglez le zoom et la partie de l'image à garder. Elle n'est jamais étirée."));
        alert.setAccessoryView(Some(&column));
        alert.addButtonWithTitle(&ns("Enregistrer"));
        alert.addButtonWithTitle(&ns("Annuler"));
        let accepted = alert.runModal() == NSAlertFirstButtonReturn;
        let editor = self.ivars().crop_editor.borrow_mut().take()?;
        accepted.then(|| editor.crop())
    }

    /// Curseur bougé : nouvel aperçu.
    pub(super) fn crop_changed(&self) {
        let editor = self.ivars().crop_editor.borrow();
        let Some(editor) = editor.as_ref() else { return };
        let width = match editor.kind {
            ProfileImage::Avatar => 440,
            ProfileImage::Banner => 840,
        };
        let pixels = editor.source.preview(editor.kind, editor.crop(), width);
        let mut png = std::io::Cursor::new(Vec::new());
        if image::DynamicImage::ImageRgba8(pixels).write_to(&mut png, image::ImageFormat::Png).is_err() {
            return;
        }
        let data = NSData::with_bytes(&png.into_inner());
        if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
            editor.preview.setImage(Some(&image));
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
