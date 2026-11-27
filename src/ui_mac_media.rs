//! Images et GIF reçus, réutilisables (Mac) : voir en grand, ajouter à mes GIF et autocollants,
//! copier (pour les coller dans une autre app), envoyer à un appareil.

use super::menus::Entry;
use super::*;

use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSURL};
use std::path::{Path, PathBuf};

/// Type d'image (UTI) d'après l'extension, pour le presse-papiers.
fn image_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_string_lossy().to_ascii_lowercase().as_str() {
        "gif" => Some("com.compuserve.gif"),
        "png" => Some("public.png"),
        "jpg" | "jpeg" => Some("public.jpeg"),
        "webp" => Some("org.webmproject.webp"),
        _ => None,
    }
}

impl Controller {
    /// Première image ou GIF (déjà reçu) du message `index`.
    fn media_of(&self, index: isize) -> Option<PathBuf> {
        let message = self.message_at(index)?;
        message
            .attachments
            .iter()
            .filter(|a| matches!(a.kind, magicplus_core::AttachmentKind::Image | magicplus_core::AttachmentKind::Gif | magicplus_core::AttachmentKind::Sticker))
            .find_map(|a| a.path.clone())
    }

    /// Entrées du menu d'un message qui contient une image ou un GIF.
    pub(super) fn media_entries(&self, index: isize, entries: &mut Vec<Entry>) {
        if self.media_of(index).is_none() {
            return;
        }
        entries.push(Entry::Separator);
        entries.push(Entry::Item("arrow.up.left.and.arrow.down.right", "Voir en grand".into(), false, Box::new(move |c: &Controller| {
            if let Some(path) = c.media_of(index) {
                c.view_image(&path);
            }
        })));
        entries.push(Entry::Item("star", "Ajouter à mes GIF et autocollants".into(), false, Box::new(move |c: &Controller| {
            if let Some(path) = c.media_of(index) {
                match c.engine().add_sticker(&path) {
                    Ok(_) => c.engine().set_status("Ajouté à vos GIF et autocollants (bouton autocollant de la zone de saisie)"),
                    Err(error) => c.engine().set_status(error.to_string()),
                }
            }
        })));
        entries.push(Entry::Item("doc.on.clipboard", "Copier l'image".into(), false, Box::new(move |c: &Controller| {
            if let Some(path) = c.media_of(index) {
                c.copy_file_to_pasteboard(&path);
            }
        })));
        entries.push(Entry::Item("laptopcomputer.and.arrow.down", "Envoyer à un appareil…".into(), false, Box::new(move |c: &Controller| {
            if let Some(path) = c.media_of(index) {
                if let Some(only) = c.choose_targets("cette image") {
                    c.engine().send_files_to(vec![path], only.as_deref());
                }
            }
        })));
    }

    /// Met le fichier dans le presse-papiers du Mac (à coller dans le Finder, Discord, Messages…),
    /// avec ses octets d'image : un GIF collé reste animé.
    pub(super) fn copy_file_to_pasteboard(&self, path: &Path) {
        let url = NSURL::fileURLWithPath(&ns(&path.to_string_lossy()));
        let item = NSPasteboardItem::new();
        if let Some(text) = url.absoluteString() {
            item.setString_forType(&text, unsafe { NSPasteboardTypeFileURL });
        }
        if let (Some(kind), Ok(bytes)) = (image_type(path), std::fs::read(path)) {
            item.setData_forType(&NSData::with_bytes(&bytes), &ns(kind));
        }
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        let objects: Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>> =
            NSArray::from_retained_slice(&[ProtocolObject::from_retained(item)]);
        if pasteboard.writeObjects(&objects) {
            self.engine().set_status("Image copiée : collez-la où vous voulez");
        } else {
            self.engine().set_status("Copie de l'image impossible");
        }
    }
}

