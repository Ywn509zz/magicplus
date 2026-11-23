//! Interface Slint (style Fluent / Windows 11), utilisée sous Windows et Linux.
//! La page « Discussions » est dans `ui_slint/chat.rs`.

mod canvas;
mod chat;
mod images;
mod markdown;
mod profile;

use crate::shared::{hotkey_labels, open_path, platform_label, reveal, setup_hotkeys, update_action, update_pending, update_texts};
use magicplus_core::{util::format_size, Engine, Snapshot, TransferState};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::rc::Rc;
use std::{path::Path, thread, time::Duration};

slint::include_modules!();

/// Durées proposées pour reprendre un transfert interrompu (heures), dans l'ordre de la liste.
const RESUME_CHOICES: [u64; 7] = [1, 6, 12, 24, 48, 72, 168];

/// Recopie l'état du moteur dans l'interface.
fn refresh(ui: &AppWindow, snapshot: &Snapshot, first: bool) {
    let peers: Vec<PeerItem> = snapshot
        .peers
        .iter()
        .map(|peer| PeerItem {
            id: peer.id.clone().into(),
            name: peer.name.clone().into(),
            platform: platform_label(&peer.platform).into(),
            address: peer.address.clone().into(),
            fingerprint: peer.fingerprint.clone().into(),
            trusted: peer.trusted,
            online: peer.online,
            key_changed: peer.key_changed,
            wants_pairing: peer.wants_pairing,
        })
        .collect();
    let joinable = snapshot.peers.iter().filter(|peer| peer.trusted && peer.online).count();
    let paired = snapshot.peers.iter().filter(|peer| peer.trusted).count();

    let transfers: Vec<TransferItem> = snapshot
        .transfers
        .iter()
        .map(|transfer| {
            let direction = if transfer.sending { "vers" } else { "depuis" };
            let (state, error) = match &transfer.state {
                TransferState::Active => (0, String::new()),
                TransferState::Complete => (1, String::new()),
                TransferState::Failed(reason) => (2, reason.clone()),
                TransferState::Paused(reason) => (3, reason.clone()),
            };
            TransferItem {
                id: transfer.id.clone().into(),
                file_name: transfer.file_name.clone().into(),
                details: format!("{direction} {}  ·  {}", transfer.peer_name, format_size(transfer.size)).into(),
                progress: transfer.progress,
                state,
                error: error.into(),
                sending: transfer.sending,
            }
        })
        .collect();

    let received: Vec<ReceivedItem> = snapshot
        .received
        .iter()
        .map(|file| ReceivedItem {
            id: file.id.clone().into(),
            file_name: file.file_name.clone().into(),
            details: format!("de {}  ·  {}  ·  {}", file.from, format_size(file.size), file.path.display()).into(),
            saved: true,
            path: file.path.to_string_lossy().to_string().into(),
        })
        .collect();

    ui.set_peers(ModelRc::new(VecModel::from(peers)));
    ui.set_transfers(ModelRc::new(VecModel::from(transfers)));
    ui.set_received(ModelRc::new(VecModel::from(received)));
    ui.set_joinable_count(joinable as i32);
    ui.set_paired_count(paired as i32);
    ui.set_device_name(snapshot.identity.name.clone().into());
    ui.set_device_platform(platform_label(&snapshot.identity.platform).into());
    ui.set_device_fingerprint(snapshot.identity.fingerprint.clone().into());
    ui.set_status(snapshot.status.clone().into());
    ui.set_save_folder(snapshot.settings.save_folder.clone().unwrap_or_default().into());
    ui.set_receive_folder(snapshot.receive_folder.to_string_lossy().to_string().into());
    let manual: Vec<ManualItem> = snapshot
        .manual
        .iter()
        .map(|item| {
            let (status, state) = match (&item.peer_name, item.unresolved) {
                (Some(name), _) => (format!("MagicPlus trouvé : {name}"), 1),
                (None, true) => ("Adresse introuvable".to_string(), 2),
                (None, false) => ("Pas de réponse pour l'instant".to_string(), 0),
            };
            ManualItem { address: item.address.clone().into(), status: status.into(), state }
        })
        .collect();
    ui.set_manual(ModelRc::new(VecModel::from(manual)));
    let warnings: Vec<SharedString> = snapshot.other_versions.iter().map(|other| other.message.clone().into()).collect();
    ui.set_version_warnings(ModelRc::new(VecModel::from(warnings)));
    let (update_text, action) = update_texts(&snapshot.update, snapshot.settings.auto_update);
    ui.set_update_text(update_text.into());
    ui.set_update_action(action.unwrap_or_default().into());
    ui.set_update_busy(matches!(
        snapshot.update.state,
        magicplus_core::UpdateState::Checking | magicplus_core::UpdateState::Downloading(_)
    ));
    ui.set_update_has_page(!snapshot.update.latest.is_empty());
    ui.set_update_pending(update_pending(&snapshot.update));
    let direct: Vec<ChoiceItem> = snapshot
        .peers
        .iter()
        .filter(|peer| peer.trusted)
        .map(|peer| ChoiceItem { id: peer.id.clone().into(), name: peer.name.clone().into(), checked: false })
        .collect();
    ui.global::<ChatData>().set_direct_choices(ModelRc::new(VecModel::from(direct)));
    ui.set_own_addresses(
        if snapshot.local_addresses.is_empty() {
            "Adresse de cet appareil : aucune (Wi-Fi ou Ethernet déconnecté ?)".to_string()
        } else {
            format!("Adresse(s) de cet appareil, à saisir sur l'autre machine :  {}", snapshot.local_addresses.join("   ·   "))
        }
        .into(),
    );
    match &snapshot.clipboard {
        Some(item) => {
            ui.set_has_clip(true);
            ui.set_clip_from(item.from.clone().into());
            ui.set_clip_text(item.text.clone().into());
        }
        None => ui.set_has_clip(false),
    }
    if first {
        ui.set_name_edit(snapshot.identity.name.clone().into());
        let index = RESUME_CHOICES.iter().position(|hours| *hours == snapshot.settings.resume_hours).unwrap_or(4);
        ui.set_resume_index(index as i32);
        ui.set_auto_copy(snapshot.settings.auto_copy_received);
        ui.set_update_auto(snapshot.settings.auto_update);
    }
}

/// Couleurs de bannière proposées (dans l'ordre de `banner-colors` de ui/app.slint).
const BANNER_COLORS: [u32; 10] = [0xE5484D, 0xF76B15, 0xE2A300, 0x30A46C, 0x12A5B5, 0x3E7BFA, 0x8E4EC6, 0xD6409F, 0xA0714F, 0x8B8D98];

/// Aperçu « Mon profil » (et les champs à la première fois).
fn refresh_my_profile(ui: &AppWindow, engine: &Engine, first: bool) {
    let mine = engine.my_profile();
    ui.set_my_profile(profile::profile_item(&mine, false));
    if first {
        ui.set_profile_bio(mine.bio.clone().into());
        ui.set_profile_pronouns(mine.pronouns.clone().into());
        let index = mine.color.and_then(|c| BANNER_COLORS.iter().position(|rgb| *rgb == c)).map(|i| i as i32).unwrap_or(-1);
        ui.set_profile_color_index(index);
    }
}

fn choose_profile_image(engine: &Engine, kind: magicplus_core::ProfileImage) {
    let title = match kind {
        magicplus_core::ProfileImage::Avatar => "Photo de profil",
        magicplus_core::ProfileImage::Banner => "Bannière du profil",
    };
    let picked = rfd::FileDialog::new()
        .set_title(title)
        .add_filter("Images et GIF", &["png", "jpg", "jpeg", "gif", "webp", "bmp"])
        .pick_file();
    if let Some(path) = picked {
        match engine.set_profile_image(kind, &path) {
            Ok(()) => engine.set_status("Profil mis à jour : il sera envoyé à vos contacts"),
            Err(error) => engine.set_status(format!("Image refusée : {error}")),
        }
    }
}

pub fn run(engine: Engine) -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;

    ui.set_version(env!("CARGO_PKG_VERSION").into());
    let (send_label, paste_label) = hotkey_labels();
    ui.set_hotkey_send(send_label.into());
    ui.set_hotkey_paste(paste_label.into());

    let initial = engine.snapshot();
    if let Some(dark) = initial.settings.dark_mode {
        ui.invoke_apply_theme(dark);
    }
    refresh(&ui, &initial, true);
    ui.global::<MarkdownStyle>().set_mono(markdown::monospace_family().into());
    let chat: Rc<chat::Chat> = chat::Chat::new(engine.clone(), &ui);
    let canvas = canvas::Canvas::new(engine.clone(), chat.clone(), &ui);
    refresh_my_profile(&ui, &engine, true);
    let profiles_seen = Rc::new(std::cell::Cell::new(engine.profiles_revision()));
    {
        let engine = engine.clone();
        ui.on_profile_choose_photo(move || choose_profile_image(&engine, magicplus_core::ProfileImage::Avatar));
    }
    {
        let engine = engine.clone();
        ui.on_profile_choose_banner(move || choose_profile_image(&engine, magicplus_core::ProfileImage::Banner));
    }
    {
        let engine = engine.clone();
        ui.on_profile_remove_photo(move || {
            if let Err(error) = engine.remove_profile_image(magicplus_core::ProfileImage::Avatar) {
                engine.set_status(error.to_string());
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_profile_remove_banner(move || {
            if let Err(error) = engine.remove_profile_image(magicplus_core::ProfileImage::Banner) {
                engine.set_status(error.to_string());
            }
        });
    }
    {
        let engine = engine.clone();
        let weak = ui.as_weak();
        ui.on_profile_save(move || {
            let Some(ui) = weak.upgrade() else { return };
            let color = BANNER_COLORS.get(ui.get_profile_color_index().max(-1) as usize).copied().filter(|_| ui.get_profile_color_index() >= 0);
            match engine.set_profile_details(&ui.get_profile_bio(), &ui.get_profile_pronouns(), color) {
                Ok(()) => engine.set_status("Profil enregistré : il sera envoyé à vos contacts"),
                Err(error) => engine.set_status(error.to_string()),
            }
        });
    }

    // ---- actions de l'interface
    {
        let engine = engine.clone();
        ui.on_trust(move |id| {
            if let Err(error) = engine.trust(&id) {
                engine.set_status(error.to_string());
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_forget(move |id| {
            if let Err(error) = engine.forget(&id) {
                engine.set_status(error.to_string());
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_send_clipboard(move || {
            let engine = engine.clone();
            thread::spawn(move || engine.send_clipboard(false));
        });
    }
    {
        let engine = engine.clone();
        ui.on_paste_received(move || engine.paste_last_received());
    }
    {
        let engine = engine.clone();
        ui.on_read_clipboard(move || SharedString::from(engine.read_local_clipboard()));
    }
    {
        let engine = engine.clone();
        ui.on_copy_text(move |text| match engine.write_local_clipboard(&text) {
            Ok(()) => engine.set_status("Texte copié dans le presse-papiers"),
            Err(error) => engine.set_status(error.to_string()),
        });
    }
    {
        let engine = engine.clone();
        ui.on_send_files(move || {
            if let Some(paths) = rfd::FileDialog::new().set_title("Choisir les fichiers à envoyer").pick_files() {
                engine.send_files(paths);
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_choose_folder(move || {
            if let Some(folder) = rfd::FileDialog::new().set_title("Dossier de réception").pick_folder() {
                let path = folder.to_string_lossy().to_string();
                match engine.update_settings(|settings| settings.save_folder = Some(path)) {
                    Ok(()) => engine.set_status("Dossier de réception enregistré"),
                    Err(error) => engine.set_status(error.to_string()),
                }
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_clear_folder(move || {
            if let Err(error) = engine.update_settings(|settings| settings.save_folder = None) {
                engine.set_status(error.to_string());
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_save_name(move |name| {
            if let Err(error) = engine.set_device_name(&name) {
                engine.set_status(error.to_string());
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_resume_changed(move |index| {
            let hours = RESUME_CHOICES.get(index.max(0) as usize).copied().unwrap_or(48);
            match engine.update_settings(|settings| settings.resume_hours = hours) {
                Ok(()) => engine.set_status(format!("Un transfert interrompu peut reprendre pendant {hours} h")),
                Err(error) => engine.set_status(error.to_string()),
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_auto_copy_changed(move |enabled| {
            if let Err(error) = engine.update_settings(|settings| settings.auto_copy_received = enabled) {
                engine.set_status(error.to_string());
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_cancel_transfer(move |id| engine.cancel_transfer(&id));
    }
    ui.on_open_path(|path| reveal(&path));
    {
        let engine = engine.clone();
        ui.on_remove_received(move |id| engine.remove_received(&id));
    }
    {
        let engine = engine.clone();
        let weak = ui.as_weak();
        ui.on_add_address(move |address| match engine.add_manual_address(&address) {
            Ok(_) => {
                if let Some(ui) = weak.upgrade() {
                    ui.set_address_edit("".into());
                }
            }
            Err(error) => engine.set_status(format!("Connexion par adresse : {error}")),
        });
    }
    {
        let engine = engine.clone();
        ui.on_remove_address(move |address| {
            if let Err(error) = engine.remove_manual_address(&address) {
                engine.set_status(error.to_string());
            }
        });
    }
    {
        let chat = Rc::downgrade(&chat);
        ui.on_chat_with(move |peer| {
            if let Some(chat) = chat.upgrade() {
                chat.chat_with(&peer);
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_update_auto_changed(move |enabled| match engine.update_settings(|settings| settings.auto_update = enabled) {
            Ok(()) if enabled => engine.check_update_now(),
            Ok(()) => {}
            Err(error) => engine.set_status(error.to_string()),
        });
    }
    {
        let engine = engine.clone();
        ui.on_update_check(move || engine.check_update_now());
    }
    {
        let engine = engine.clone();
        ui.on_update_act(move || {
            if update_action(&engine) {
                let _ = slint::quit_event_loop();
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_update_open_page(move || {
            let url = engine.update().page_url;
            if !url.is_empty() {
                open_path(&url);
            }
        });
    }
    {
        let engine = engine.clone();
        ui.on_theme_changed(move |dark| {
            let _ = engine.update_settings(|settings| settings.dark_mode = Some(dark));
        });
    }

    // ---- raccourcis globaux (doivent être enregistrés depuis le fil principal)
    let hotkeys = setup_hotkeys(&engine);

    // ---- rafraîchissement : l'interface ne se redessine que si le moteur a changé
    let timer = slint::Timer::default();
    let debug_refresh = std::env::var_os("MAGICPLUS_DEBUG").is_some();
    {
        let engine = engine.clone();
        let weak = ui.as_weak();
        let chat = chat.clone();
        timer.start(slint::TimerMode::Repeated, Duration::from_millis(150), move || {
            chat.tick();
            canvas.tick();
            let profiles = engine.profiles_revision();
            if profiles != profiles_seen.get() {
                profiles_seen.set(profiles);
                if let Some(ui) = weak.upgrade() {
                    refresh_my_profile(&ui, &engine, false);
                }
            }
            if engine.take_dirty() {
                if let Some(ui) = weak.upgrade() {
                    let started = std::time::Instant::now();
                    refresh(&ui, &engine.snapshot(), false);
                    if debug_refresh {
                        eprintln!("[interface] mise à jour en {:?}", started.elapsed());
                    }
                }
            }
        });
    }

    // Animation des GIF des discussions : 20 images par seconde, seulement quand il y en a à l'écran.
    let gif_timer = slint::Timer::default();
    {
        let weak = ui.as_weak();
        let chat = chat.clone();
        gif_timer.start(slint::TimerMode::Repeated, Duration::from_millis(50), move || {
            if let Some(ui) = weak.upgrade() {
                if ui.get_page() == chat::CHAT_PAGE && chat.animating() {
                    let data = ui.global::<ChatData>();
                    data.set_tick(data.get_tick().wrapping_add(1));
                }
            }
        });
    }

    // Mode développement : photographie chaque page puis quitte (MAGICPLUS_SCREENSHOT_DIR=dossier).
    // MAGICPLUS_SCREENSHOT_PAGES=discussion,discussion-2 limite la série à ces captures.
    let screenshot_timer = slint::Timer::default();
    if let Some(dir) = std::env::var_os("MAGICPLUS_SCREENSHOT_DIR") {
        let weak = ui.as_weak();
        let chat = chat.clone();
        let only: Option<Vec<String>> =
            std::env::var("MAGICPLUS_SCREENSHOT_PAGES").ok().map(|list| list.split(',').map(|s| s.trim().to_string()).collect());
        // (nom de la capture, mise en place)
        type Setup = Box<dyn Fn(&AppWindow, &chat::Chat)>;
        let steps: Vec<(&str, Setup)> = vec![
            ("appareils", Box::new(|ui, _| ui.set_page(0))),
            ("discussion", Box::new(|ui, _| ui.set_page(chat::CHAT_PAGE))),
            ("discussion-2", Box::new(|ui, chat| {
                ui.set_page(chat::CHAT_PAGE);
                chat.select_index(1);
            })),
            ("profil", Box::new(|ui, chat| {
                ui.set_page(chat::CHAT_PAGE);
                chat.select_index(1);
                let data = ui.global::<ChatData>();
                data.invoke_show_profile(data.get_peer_id());
            })),
            ("canevas", Box::new(|ui, chat| {
                ui.set_page(chat::CHAT_PAGE);
                chat.select_index(0);
                ui.global::<CanvasData>().invoke_toggle();
            })),
            ("canevas-historique", Box::new(|ui, _| {
                let data = ui.global::<CanvasData>();
                data.invoke_history_toggle();
                data.invoke_version_select(0);
            })),
            ("discussion-clair", Box::new(|ui, chat| {
                ui.global::<CanvasData>().set_open(false);
                ui.invoke_apply_theme(false);
                ui.set_page(chat::CHAT_PAGE);
                chat.select_index(0);
            })),
            ("discussion-sombre", Box::new(|ui, chat| {
                ui.invoke_apply_theme(true);
                ui.set_page(chat::CHAT_PAGE);
                chat.select_index(0);
            })),
            ("presse-papiers", Box::new(|ui, _| {
                ui.invoke_apply_theme(false);
                ui.set_page(1);
            })),
            ("transferts", Box::new(|ui, _| ui.set_page(2))),
            ("parametres", Box::new(|ui, _| ui.set_page(3))),
            ("association", Box::new(|ui, _| {
                ui.set_page(0);
                if let Some(peer) = ui.get_peers().row_data(0) {
                    ui.set_pairing_peer(peer);
                    ui.set_pairing_open(true);
                }
            })),
        ];
        let steps: Vec<(&str, Setup)> =
            steps.into_iter().filter(|(name, _)| only.as_ref().is_none_or(|only| only.iter().any(|o| o == name))).collect();
        let tick = std::cell::Cell::new(0_usize);
        screenshot_timer.start(slint::TimerMode::Repeated, Duration::from_millis(2000), move || {
            let Some(ui) = weak.upgrade() else { return };
            let step = tick.get();
            tick.set(step + 1);
            // Tour impair : mise en place ; tour pair suivant : capture.
            let index = step / 2;
            let Some((name, setup)) = steps.get(index) else {
                let _ = slint::quit_event_loop();
                return;
            };
            if step.is_multiple_of(2) {
                setup(&ui, &chat);
            } else {
                let started = std::time::Instant::now();
                let snapshot = ui.window().take_snapshot();
                eprintln!("[capture] {name} : rendu en {:?}", started.elapsed());
                match snapshot {
                    Ok(buffer) => save_tga(&std::path::PathBuf::from(&dir).join(format!("{name}.tga")), &buffer),
                    Err(error) => eprintln!("[capture] impossible : {error}"),
                }
            }
        });
    }

    // Diagnostic (MAGICPLUS_DEBUG=1) : écart maximal entre deux passages de la boucle d'événements.
    // Un écart de plusieurs centaines de ms signifie que quelque chose bloque le fil de l'interface.
    let debug_timer = slint::Timer::default();
    if std::env::var_os("MAGICPLUS_DEBUG").is_some() {
        let last = std::cell::Cell::new(std::time::Instant::now());
        let worst = std::cell::Cell::new(Duration::ZERO);
        let ticks = std::cell::Cell::new(0_u32);
        debug_timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
            let now = std::time::Instant::now();
            worst.set(worst.get().max(now - last.replace(now)));
            ticks.set(ticks.get() + 1);
            // ~2 s à 16 ms par tour
            if ticks.get() >= 125 {
                eprintln!("[boucle] écart max entre deux tours : {:?} (idéal : ~16 ms)", worst.get());
                worst.set(Duration::ZERO);
                ticks.set(0);
            }
        });
    }

    ui.run()?;
    engine.shutdown();
    drop(hotkeys);
    Ok(())
}

/// Écrit une image TGA non compressée (lisible par `sips` sur macOS).
fn save_tga(path: &Path, buffer: &slint::SharedPixelBuffer<slint::Rgba8Pixel>) {
    let (width, height) = (buffer.width() as u16, buffer.height() as u16);
    let mut out = vec![0_u8; 18];
    out[2] = 2; // image couleur non compressée
    out[12..14].copy_from_slice(&width.to_le_bytes());
    out[14..16].copy_from_slice(&height.to_le_bytes());
    out[16] = 32;
    out[17] = 0x28; // origine en haut à gauche, 8 bits alpha
    for pixel in buffer.as_slice() {
        out.extend_from_slice(&[pixel.b, pixel.g, pixel.r, pixel.a]);
    }
    if let Err(error) = std::fs::write(path, out) {
        eprintln!("[capture] écriture impossible : {error}");
    }
}
