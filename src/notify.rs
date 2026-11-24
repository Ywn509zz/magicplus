//! Notifications du système pour les nouveaux messages (Windows : centre de notifications ;
//! macOS : centre de notifications), communes aux deux interfaces. Un clic sur une notification
//! rouvre MagicPlus sur la conversation (`take_clicked`, relu par le minuteur de l'interface).

use magicplus_core::{presence::should_notify, Engine, NotificationView};
use std::sync::{Arc, Mutex};

/// Identifiant d'application Windows (AUMID) sous lequel les notifications apparaissent.
#[cfg(target_os = "windows")]
const APP_ID: &str = "MagicPlus.Discussions";

#[derive(Clone, Default)]
pub struct Notifier {
    clicked: Arc<Mutex<Option<String>>>,
    /// macOS : délégué du centre de notifications (reçoit les clics), gardé en vie ici.
    #[cfg(all(target_os = "macos", not(feature = "slint-on-mac")))]
    delegate: Option<objc2::rc::Retained<mac::Delegate>>,
}

impl Notifier {
    /// Prépare les notifications (Windows : nom et icône de MagicPlus ; macOS : son identifiant).
    pub fn new(engine: &Engine) -> Self {
        #[cfg(target_os = "windows")]
        register_app_id(engine);
        #[cfg(all(target_os = "macos", not(feature = "slint-on-mac")))]
        {
            let clicked = Arc::new(Mutex::new(None));
            let delegate = mac::setup(clicked.clone());
            if delegate.is_none() {
                engine.set_status("Notifications indisponibles hors de MagicPlus.app (lancé par cargo run ?)");
            }
            Self { clicked, delegate }
        }
        #[cfg(not(all(target_os = "macos", not(feature = "slint-on-mac"))))]
        {
            #[cfg(not(target_os = "windows"))]
            let _ = engine;
            Self::default()
        }
    }

    /// Conversation dont la notification vient d'être cliquée (une seule fois).
    pub fn take_clicked(&self) -> Option<String> {
        self.clicked.lock().ok().and_then(|mut clicked| clicked.take())
    }

    /// Montre une notification ; un clic mémorise `conversation`.
    pub fn show(&self, title: &str, body: &str, sound: bool, conversation: &str) {
        #[cfg(target_os = "windows")]
        {
            use tauri_winrt_notification::{Sound, Toast};
            let clicked = self.clicked.clone();
            let conversation = conversation.to_string();
            let result = Toast::new(APP_ID)
                .title(title)
                .text1(body)
                .sound(if sound { Some(Sound::IM) } else { None })
                .on_activated(move |_| {
                    if let Ok(mut slot) = clicked.lock() {
                        *slot = Some(conversation.clone());
                    }
                    Ok(())
                })
                .show();
            if let Err(error) = result {
                eprintln!("[notifications] {error}");
            }
        }
        #[cfg(all(target_os = "macos", not(feature = "slint-on-mac")))]
        if self.delegate.is_some() {
            mac::show(title, body, sound, conversation);
        }
        #[cfg(not(any(target_os = "windows", all(target_os = "macos", not(feature = "slint-on-mac")))))]
        let _ = (title, body, sound, conversation, &self.clicked);
    }

    /// Montre les nouveaux messages du moteur selon les préférences et la présence. Rien pour la
    /// conversation affichée quand MagicPlus est au premier plan (sauf préférence contraire).
    pub fn deliver(&self, engine: &Engine, focused_conversation: Option<&str>) {
        let notes = engine.take_notifications();
        if notes.is_empty() {
            return;
        }
        let settings = engine.snapshot().settings;
        let prefs = settings.notifications;
        let presence = engine.presence();
        let shown: Vec<&NotificationView> = notes
            .iter()
            .filter(|note| should_notify(note, &prefs, presence))
            .filter(|note| prefs.when_focused || focused_conversation != Some(note.conversation.as_str()))
            .collect();
        // Une rafale (plusieurs conversations d'un coup) : les trois dernières seulement.
        for note in shown.iter().rev().take(3).rev() {
            let (title, body) = note.render(&prefs);
            self.show(&title, &body, prefs.sound, &note.conversation);
        }
    }
}

/// Windows : une application non installée doit déclarer son identifiant (AUMID), avec son nom
/// et son icône, pour que ses notifications s'affichent (clé du registre de l'utilisateur).
#[cfg(target_os = "windows")]
fn register_app_id(engine: &Engine) {
    use std::{os::windows::process::CommandExt, process::Command};
    const ICON: &[u8] = include_bytes!("../packaging/macos/icon.png");
    let icon = engine.data_dir().join("icone-notifications.png");
    if std::fs::read(&icon).ok().as_deref() != Some(ICON) {
        let _ = std::fs::write(&icon, ICON);
    }
    let key = format!(r"HKCU\Software\Classes\AppUserModelId\{APP_ID}");
    for (name, value) in [("DisplayName", "MagicPlus".to_string()), ("IconUri", icon.to_string_lossy().to_string())] {
        let _ = Command::new("reg")
            .args(["add", &key, "/v", name, "/t", "REG_EXPAND_SZ", "/d", &value, "/f"])
            .creation_flags(0x0800_0000)
            .output();
    }
}

/// macOS : centre de notifications (`UNUserNotificationCenter`). Il faut une vraie application
/// (MagicPlus.app, avec son identifiant) ; lancé autrement, rien n'est montré.
#[cfg(all(target_os = "macos", not(feature = "slint-on-mac")))]
mod mac {
    use block2::{DynBlock, RcBlock};
    use objc2::{
        define_class, msg_send,
        rc::Retained,
        runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject},
        AnyThread, DefinedClass,
    };
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationPresentationOptions,
        UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
        UNUserNotificationCenterDelegate,
    };
    use std::sync::{Arc, Mutex};

    pub struct Ivars {
        clicked: Arc<Mutex<Option<String>>>,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "MagicPlusNotificationDelegate"]
        #[ivars = Ivars]
        pub struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            /// MagicPlus au premier plan : la notification s'affiche quand même (le tri est déjà fait).
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                handler.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List
                    | UNNotificationPresentationOptions::Sound,));
            }

            /// Clic : la conversation est dans l'identifiant (« magicplus|<conversation>|<n> »).
            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(&self, _center: &UNUserNotificationCenter, response: &UNNotificationResponse, handler: &DynBlock<dyn Fn()>) {
                let id = response.notification().request().identifier().to_string();
                if let (Some(conversation), Ok(mut clicked)) = (id.split('|').nth(1), self.ivars().clicked.lock()) {
                    *clicked = Some(conversation.to_string());
                }
                handler.call(());
            }
        }
    );

    /// Délégué installé et autorisation demandée ; `None` hors d'une application empaquetée.
    pub fn setup(clicked: Arc<Mutex<Option<String>>>) -> Option<Retained<Delegate>> {
        NSBundle::mainBundle().bundleIdentifier()?;
        let delegate: Retained<Delegate> = unsafe { msg_send![super(Delegate::alloc().set_ivars(Ivars { clicked })), init] };
        let center = UNUserNotificationCenter::currentNotificationCenter();
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let done = RcBlock::new(|_granted: Bool, _error: *mut NSError| {});
        center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &done);
        Some(delegate)
    }

    pub fn show(title: &str, body: &str, sound: bool, conversation: &str) {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        if sound {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        let id = format!("magicplus|{conversation}|{}", uuid::Uuid::new_v4());
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&id), &content, None);
        UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, None);
    }
}
