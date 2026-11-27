//! Image ou GIF en grand (clic sur une image d'une discussion, sur une photo ou une bannière du
//! profil en grand) : fenêtre sombre à la taille de l'image, GIF animé.

use super::*;
use crate::shared::open_path;
use objc2::AnyThread;
use objc2_app_kit::NSScreen;
use std::path::{Path, PathBuf};

/// Extensions affichées dans la visionneuse (les autres fichiers s'ouvrent avec le système).
pub(super) fn is_viewable(path: &Path) -> bool {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|ext| matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" | "tif" | "tiff"))
}

/// Visionneuse ouverte et son fichier.
pub(super) struct Viewer {
    window: Retained<NSWindow>,
    path: PathBuf,
}

impl Controller {
    pub(super) fn view_image(&self, path: &Path) {
        let mtm = self.mtm();
        let Some(image) = NSImage::initWithContentsOfFile(NSImage::alloc(), &ns(&path.to_string_lossy())) else {
            open_path(&path.to_string_lossy());
            return;
        };
        if let Some(open) = self.ivars().viewer.borrow_mut().take() {
            open.window.close();
        }
        // Taille : celle de l'image, réduite pour tenir dans 85 % de l'écran.
        let size = image.size();
        let screen = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame().size).unwrap_or(NSSize::new(1280.0, 800.0));
        let bar = 48.0;
        let (max_w, max_h) = (screen.width * 0.85, screen.height * 0.85 - bar);
        let scale = (max_w / size.width.max(1.0)).min(max_h / size.height.max(1.0)).min(1.0);
        let (width, height) = ((size.width * scale).max(320.0), (size.height * scale).max(200.0));

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height + bar)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&ns(&path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
        window.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(0.08, 1.0)));
        window.setContentMinSize(NSSize::new(320.0, 240.0));

        let column = stack(true, 0.0, mtm);
        let view = NSImageView::imageViewWithImage(&image, mtm);
        view.setAnimates(true);
        view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        column.addArrangedSubview(&view);
        let buttons = stack(false, 8.0, mtm);
        buttons.setEdgeInsets(NSEdgeInsets { top: 10.0, left: 12.0, bottom: 10.0, right: 12.0 });
        let open = button("Ouvrir", sel!(viewerOpen:), self, mtm);
        let reveal = button("Afficher dans le Finder", sel!(viewerReveal:), self, mtm);
        let close = button("Fermer", sel!(viewerClose:), self, mtm);
        close.setKeyEquivalent(&ns("\u{1b}"));
        buttons.addView_inGravity(&open, NSStackViewGravity::Leading);
        buttons.addView_inGravity(&reveal, NSStackViewGravity::Leading);
        buttons.addView_inGravity(&close, NSStackViewGravity::Trailing);
        column.addArrangedSubview(&buttons);
        buttons.heightAnchor().constraintEqualToConstant(bar).setActive(true);
        window.setContentView(Some(&column));
        view.widthAnchor().constraintEqualToAnchor(&column.widthAnchor()).setActive(true);
        buttons.widthAnchor().constraintEqualToAnchor(&column.widthAnchor()).setActive(true);
        window.center();
        window.makeKeyAndOrderFront(None);
        *self.ivars().viewer.borrow_mut() = Some(Viewer { window, path: path.to_path_buf() });
    }

    pub(super) fn viewer_action(&self, action: u8) {
        let Some((window, path)) = self.ivars().viewer.borrow().as_ref().map(|v| (v.window.clone(), v.path.clone())) else { return };
        match action {
            0 => open_path(&path.to_string_lossy()),
            1 => reveal(&path.to_string_lossy()),
            _ => {
                window.close();
                self.ivars().viewer.borrow_mut().take();
            }
        }
    }
}
