//! macOS : MagicPlus reste actif en arrière-plan (fenêtre cachée au lieu de quitter, icône dans
//! la barre des menus, pas de mise en veille « App Nap » qui ralentirait les annonces réseau) et
//! affiche les notifications du système pour les messages reçus.

use super::*;
use block2::RcBlock;
use magicplus_core::Notice;
use objc2::{runtime::Bool, AllocAnyThread};
use objc2_app_kit::{NSStatusBar, NSStatusItem, NSVariableStatusItemLength};
use objc2_foundation::{NSActivityOptions, NSBundle, NSDictionary, NSError, NSProcessInfo};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions,
    UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use std::sync::Mutex;

/// Notifications cliquées : conversation à ouvrir (vide : page Appareils).
static CLICKED: Mutex<Vec<String>> = Mutex::new(Vec::new());

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MagicPlusNotificationDelegate"]
    pub(super) struct NotificationDelegate;

    unsafe impl NSObjectProtocol for NotificationDelegate {}

    unsafe impl UNUserNotificationCenterDelegate for NotificationDelegate {
        /// Même au premier plan : l'interface a déjà écarté ce qui est sous les yeux.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            handler: &block2::DynBlock<dyn Fn()>,
        ) {
            let info = response.notification().request().content().userInfo();
            let key = ns("conversation");
            let conversation = info
                .objectForKey(&key)
                .and_then(|value| value.downcast::<NSString>().ok())
                .map(|value| value.to_string())
                .unwrap_or_default();
            CLICKED.lock().unwrap_or_else(|p| p.into_inner()).push(conversation);
            handler.call(());
        }
    }
);

/// Objets à garder en vie tant que l'app tourne.
pub(super) struct SystemState {
    status_item: Retained<NSStatusItem>,
    _delegate: Option<Retained<NotificationDelegate>>,
    _activity: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    /// App empaquetée (`.app`) : notifications du système ; sinon (développement) `osascript`.
    bundled: bool,
    unread: Cell<usize>,
}

impl Controller {
    /// Barre des menus, notifications, anti-« App Nap ».
    pub(super) fn setup_system(&self, mtm: MainThreadMarker) {
        // Annonces toutes les 2 s même fenêtre cachée : macOS ne doit pas endormir l'app.
        let activity = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
            NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
            &ns("Liaison avec les appareils MagicPlus"),
        );

        let status_item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = status_item.button(mtm) {
            if let Some(image) =
                NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns("bubble.left.and.bubble.right"), Some(&ns("MagicPlus")))
            {
                image.setTemplate(true);
                button.setImage(Some(&image));
            }
            button.setToolTip(Some(&ns("MagicPlus est actif : les autres appareils peuvent vous joindre")));
        }
        let menu = NSMenu::new(mtm);
        let open = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns("Ouvrir MagicPlus"), Some(sel!(showMainWindow:)), &ns("")) };
        unsafe { open.setTarget(Some(as_target(self))) };
        menu.addItem(&open);
        let messages = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns("Discussions"), Some(sel!(showDiscussions:)), &ns("")) };
        unsafe { messages.setTarget(Some(as_target(self))) };
        menu.addItem(&messages);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let quit = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns("Quitter MagicPlus"), Some(sel!(terminate:)), &ns("q")) };
        menu.addItem(&quit);
        status_item.setMenu(Some(&menu));

        let bundled = NSBundle::mainBundle().bundleIdentifier().is_some();
        let delegate = bundled.then(|| {
            let delegate: Retained<NotificationDelegate> = unsafe { msg_send![NotificationDelegate::alloc(), init] };
            let center = UNUserNotificationCenter::currentNotificationCenter();
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            let engine = self.engine().clone();
            let done = RcBlock::new(move |granted: Bool, error: *mut NSError| {
                if !granted.as_bool() {
                    // SAFETY : erreur fournie par le système, valable pendant l'appel (ou nulle).
                    let reason = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string()).unwrap_or_default();
                    eprintln!("[magicplus] notifications non autorisées : {reason}");
                    if engine.snapshot().settings.notifications.enabled {
                        engine.set_status(
                            "Notifications désactivées pour MagicPlus : autorisez-les dans Réglages Système › Notifications \
                             (MagicPlus doit être rangé dans le dossier Applications)",
                        );
                    }
                }
            });
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge,
                &done,
            );
            delegate
        });
        let _ = self.ivars().system.set(SystemState {
            status_item,
            _delegate: delegate,
            _activity: activity,
            bundled,
            unread: Cell::new(usize::MAX),
        });
    }

    /// Montre la fenêtre (icône du Dock, barre des menus, notification cliquée).
    pub(super) fn show_main_window(&self) {
        let mtm = self.mtm();
        if let Some(window) = self.ivars().main_window.get() {
            window.makeKeyAndOrderFront(None);
        }
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
    }

    /// Toutes les 150 ms : notifications à afficher et clics reçus.
    pub(super) fn system_tick(&self) {
        let engine = self.engine();
        for notice in engine.take_notices() {
            if notice.even_if_visible || !self.notice_is_visible(&notice) {
                self.post_notification(&notice);
            }
        }
        let clicked: Vec<String> = std::mem::take(&mut *CLICKED.lock().unwrap_or_else(|p| p.into_inner()));
        for conversation in clicked {
            self.show_main_window();
            if conversation.is_empty() {
                self.show_page(0);
            } else {
                self.show_page(CHAT_PAGE);
                self.open_conversation(&conversation);
            }
        }
    }

    /// Nombre de messages non lus sur l'icône du Dock et dans la barre des menus.
    pub(super) fn set_unread_badge(&self, unread: usize) {
        let Some(system) = self.ivars().system.get() else { return };
        if system.unread.get() != unread {
            system.unread.set(unread);
            let label = if unread > 0 { unread.to_string() } else { String::new() };
            if let Some(button) = system.status_item.button(self.mtm()) {
                button.setTitle(&ns(if unread > 0 { &label } else { "" }));
            }
            let tile = NSApplication::sharedApplication(self.mtm()).dockTile();
            let badge = ns(&label);
            tile.setBadgeLabel(if unread > 0 { Some(&badge) } else { None });
        }
    }

    /// La conversation est déjà sous les yeux de l'utilisateur : pas de notification.
    fn notice_is_visible(&self, notice: &Notice) -> bool {
        let app = NSApplication::sharedApplication(self.mtm());
        let window_key = self.ivars().main_window.get().is_some_and(|w| w.isKeyWindow() && w.isVisible());
        app.isActive()
            && window_key
            && self.ivars().current_page.get() == CHAT_PAGE
            && notice.conversation.is_some()
            && notice.conversation == self.current_conversation_id()
    }

    fn post_notification(&self, notice: &Notice) {
        let bundled = self.ivars().system.get().is_some_and(|s| s.bundled);
        if !bundled {
            // Développement (programme hors de MagicPlus.app) : notification d'AppleScript.
            let quote = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
            let script = format!("display notification \"{}\" with title \"{}\"", quote(&notice.body), quote(&notice.title));
            let _ = std::process::Command::new("osascript").args(["-e", &script]).spawn();
            return;
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&ns(&notice.title));
        content.setBody(&ns(&notice.body));
        if !notice.silent {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        if let Some(conversation) = &notice.conversation {
            content.setThreadIdentifier(&ns(conversation));
        }
        let key = ns("conversation");
        let value = ns(notice.conversation.as_deref().unwrap_or(""));
        let info = NSDictionary::<NSString, NSString>::from_slices(&[&*key], &[&*value]);
        // SAFETY : un dictionnaire de chaînes est un `NSDictionary` valable pour `userInfo`.
        unsafe {
            let info: Retained<NSDictionary> = Retained::cast_unchecked(info);
            content.setUserInfo(&info);
        }
        let identifier = ns(&format!("magicplus-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&identifier, &content, None);
        UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, None);
    }
}
