//! Windows : icône dans la zone de notification (MagicPlus reste actif fenêtre fermée) et
//! notifications du système (bulles de l'icône, affichées comme des notifications Windows 10/11 ;
//! un clic ouvre la conversation).
//!
//! Ailleurs (Linux, essais sur Mac) : rien, la fermeture de la fenêtre quitte l'app.

/// Ce que l'utilisateur a demandé depuis l'icône.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub enum TrayEvent {
    /// Clic sur l'icône ou « Ouvrir MagicPlus ».
    Open,
    /// Clic sur la dernière notification.
    NoticeClicked,
    /// « Quitter MagicPlus ».
    Quit,
}

#[cfg(target_os = "windows")]
mod system {
    use super::TrayEvent;
    use std::sync::{
        atomic::{AtomicIsize, AtomicU32, Ordering},
        Mutex,
    };
    use windows_sys::Win32::{
        Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_LARGE_ICON, NIIF_USER, NIM_ADD,
                NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NIN_BALLOONUSERCLICK, NIN_SELECT, NOTIFYICONDATAW,
                NOTIFYICON_VERSION_4,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, EnumWindows,
                GetCursorPos, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsIconic, PostMessageW, RegisterClassW, RegisterWindowMessageW,
                SetForegroundWindow, ShowWindow, TrackPopupMenu, HICON, MF_SEPARATOR, MF_STRING, SW_RESTORE,
                TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_NULL, WNDCLASSW,
            },
        },
    };

    const WM_TRAY: u32 = WM_APP + 1;
    /// Icône choisie au clavier (`NIN_SELECT | NINF_KEY`).
    const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
    const ID_OPEN: usize = 1;
    const ID_QUIT: usize = 2;
    const ICON_ID: u32 = 1;

    static EVENTS: Mutex<Vec<TrayEvent>> = Mutex::new(Vec::new());
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static ICON: AtomicIsize = AtomicIsize::new(0);
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn copy_into(target: &mut [u16], text: &str) {
        let units: Vec<u16> = text.encode_utf16().take(target.len() - 1).collect();
        target[..units.len()].copy_from_slice(&units);
        target[units.len()] = 0;
    }

    fn push(event: TrayEvent) {
        EVENTS.lock().unwrap_or_else(|p| p.into_inner()).push(event);
    }

    fn base_data() -> NOTIFYICONDATAW {
        // SAFETY : structure C sans invariant, remplie de zéros puis complétée.
        let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = WINDOW.load(Ordering::SeqCst) as HWND;
        data.uID = ICON_ID;
        data
    }

    fn add_icon() {
        let mut data = base_data();
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = ICON.load(Ordering::SeqCst) as HICON;
        copy_into(&mut data.szTip, "MagicPlus — actif, joignable par vos appareils");
        // SAFETY : `data` est complète et vit pendant l'appel.
        unsafe {
            Shell_NotifyIconW(NIM_ADD, &data);
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
    }

    fn show_menu(hwnd: HWND) {
        // SAFETY : appels Win32 sur notre fenêtre, menu détruit après usage.
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            AppendMenuW(menu, MF_STRING, ID_OPEN, wide("Ouvrir MagicPlus").as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            AppendMenuW(menu, MF_STRING, ID_QUIT, wide("Quitter MagicPlus").as_ptr());
            let mut point = POINT { x: 0, y: 0 };
            GetCursorPos(&mut point);
            // Sans cela, le menu ne se ferme pas quand on clique ailleurs.
            SetForegroundWindow(hwnd);
            TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, point.x, point.y, 0, hwnd, std::ptr::null());
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyMenu(menu);
        }
    }

    unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if message == WM_TRAY {
            match (lparam as u32) & 0xFFFF {
                NIN_SELECT | NIN_KEYSELECT => push(TrayEvent::Open),
                WM_CONTEXTMENU => show_menu(hwnd),
                NIN_BALLOONUSERCLICK => push(TrayEvent::NoticeClicked),
                _ => {}
            }
            return 0;
        }
        if message == WM_COMMAND {
            match wparam & 0xFFFF {
                ID_OPEN => push(TrayEvent::Open),
                ID_QUIT => push(TrayEvent::Quit),
                _ => {}
            }
            return 0;
        }
        // L'Explorateur a redémarré : l'icône doit être remise.
        let taskbar_created = TASKBAR_CREATED.load(Ordering::SeqCst);
        if taskbar_created != 0 && message == taskbar_created {
            add_icon();
            return 0;
        }
        // SAFETY : traitement par défaut de Windows.
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }

    /// Icône 32 × 32 à partir de l'icône de l'app (PNG intégré au programme).
    fn create_icon() -> HICON {
        let Ok(image) = image::load_from_memory(include_bytes!("../../packaging/macos/icon.png")) else {
            return std::ptr::null_mut();
        };
        let image = image::imageops::resize(&image.to_rgba8(), 32, 32, image::imageops::FilterType::Lanczos3);
        let color: Vec<u8> = image.pixels().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
        let mask = vec![0_u8; 32 * 32 / 8];
        // SAFETY : tampons de la bonne taille (32 × 32, 32 bits ; masque 1 bit), copiés par Windows.
        unsafe { CreateIcon(GetModuleHandleW(std::ptr::null()), 32, 32, 1, 32, mask.as_ptr(), color.as_ptr()) }
    }

    pub struct Tray;

    impl Tray {
        /// À créer sur le fil de l'interface (sa boucle de messages fait vivre l'icône).
        pub fn new() -> Option<Tray> {
            // SAFETY : création d'une classe et d'une fenêtre cachée qui ne sert qu'aux messages de l'icône.
            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let class_name = wide("MagicPlusTray");
                let mut class: WNDCLASSW = std::mem::zeroed();
                class.lpfnWndProc = Some(window_proc);
                class.hInstance = instance;
                class.lpszClassName = class_name.as_ptr();
                RegisterClassW(&class);
                let hwnd = CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    wide("MagicPlus (icône)").as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
                if hwnd.is_null() {
                    return None;
                }
                WINDOW.store(hwnd as isize, Ordering::SeqCst);
                ICON.store(create_icon() as isize, Ordering::SeqCst);
                TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::SeqCst);
            }
            add_icon();
            Some(Tray)
        }

        /// Notification du système (bulle de l'icône).
        pub fn notify(&self, title: &str, body: &str) {
            let mut data = base_data();
            data.uFlags = NIF_INFO;
            copy_into(&mut data.szInfoTitle, title);
            copy_into(&mut data.szInfo, if body.is_empty() { " " } else { body });
            data.dwInfoFlags = NIIF_USER | NIIF_LARGE_ICON;
            data.hBalloonIcon = ICON.load(Ordering::SeqCst) as HICON;
            // SAFETY : `data` est complète et vit pendant l'appel.
            unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
        }

        pub fn take_events(&self) -> Vec<TrayEvent> {
            std::mem::take(&mut *EVENTS.lock().unwrap_or_else(|p| p.into_inner()))
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            let data = base_data();
            // SAFETY : retire notre icône.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        }
    }

    /// Fenêtre de ce programme portant ce titre (jamais celle d'une autre app).
    fn main_window(title: &str) -> HWND {
        struct Search {
            title: Vec<u16>,
            found: HWND,
        }
        unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
            // SAFETY : `lparam` pointe sur la recherche en cours (voir plus bas).
            let search = unsafe { &mut *(lparam as *mut Search) };
            let mut process = 0_u32;
            let mut text = [0_u16; 128];
            // SAFETY : lecture du propriétaire et du titre d'une fenêtre.
            let length = unsafe {
                GetWindowThreadProcessId(hwnd, &mut process);
                GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32)
            };
            if process == std::process::id() && text[..length.max(0) as usize] == search.title[..search.title.len() - 1] {
                search.found = hwnd;
                return 0;
            }
            1
        }
        let mut search = Search { title: wide(title), found: std::ptr::null_mut() };
        // SAFETY : la recherche vit pendant tout l'appel.
        unsafe { EnumWindows(Some(visit), &mut search as *mut Search as LPARAM) };
        search.found
    }

    /// Met la fenêtre de l'app au premier plan (après l'avoir montrée).
    pub fn bring_to_front(title: &str) {
        let hwnd = main_window(title);
        if !hwnd.is_null() {
            // SAFETY : fenêtre trouvée ci-dessus.
            unsafe {
                if IsIconic(hwnd) != 0 {
                    ShowWindow(hwnd, SW_RESTORE);
                }
                SetForegroundWindow(hwnd);
            }
        }
    }

    /// Vrai si la fenêtre de l'app est celle que l'utilisateur regarde.
    pub fn is_foreground(title: &str) -> bool {
        let hwnd = main_window(title);
        // SAFETY : simple lecture de la fenêtre active.
        !hwnd.is_null() && unsafe { GetForegroundWindow() } == hwnd
    }
}

#[cfg(not(target_os = "windows"))]
mod system {
    use super::TrayEvent;

    pub struct Tray;

    impl Tray {
        pub fn new() -> Option<Tray> {
            None
        }

        pub fn notify(&self, _title: &str, _body: &str) {}

        pub fn take_events(&self) -> Vec<TrayEvent> {
            Vec::new()
        }
    }

    pub fn bring_to_front(_title: &str) {}

    pub fn is_foreground(_title: &str) -> bool {
        true
    }
}

pub use system::{bring_to_front, is_foreground, Tray};
