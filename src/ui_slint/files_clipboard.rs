//! Copie d'un fichier dans le presse-papiers du système (pour le coller dans l'Explorateur,
//! Discord, un e-mail…) : Windows, liste de fichiers `CF_HDROP` ; ailleurs, le chemin en texte.

use magicplus_core::Engine;
use std::path::Path;

#[cfg(target_os = "windows")]
pub fn copy_file(engine: &Engine, path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    };
    const CF_HDROP: u32 = 15;
    /// En-tête `DROPFILES` suivi des chemins (UTF-16, chacun terminé par 0, puis un 0 final).
    #[repr(C)]
    struct DropFiles {
        files_offset: u32,
        x: i32,
        y: i32,
        non_client: i32,
        wide: i32,
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let header = std::mem::size_of::<DropFiles>();
    let size = header + wide.len() * 2;
    // SAFETY : mémoire globale allouée ici, remplie dans ses limites, confiée au presse-papiers.
    let done = unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, size);
        if memory.is_null() {
            false
        } else {
            let pointer = GlobalLock(memory) as *mut u8;
            if pointer.is_null() {
                false
            } else {
                let drop = DropFiles { files_offset: header as u32, x: 0, y: 0, non_client: 0, wide: 1 };
                std::ptr::write_unaligned(pointer as *mut DropFiles, drop);
                std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, pointer.add(header), wide.len() * 2);
                GlobalUnlock(memory);
                let opened = OpenClipboard(std::ptr::null_mut()) != 0;
                let set = opened && EmptyClipboard() != 0 && !SetClipboardData(CF_HDROP, memory).is_null();
                if opened {
                    CloseClipboard();
                }
                set
            }
        }
    };
    engine.set_status(if done { "Image copiée : collez-la où vous voulez" } else { "Copie de l'image impossible" });
}

#[cfg(not(target_os = "windows"))]
pub fn copy_file(engine: &Engine, path: &Path) {
    match engine.write_local_clipboard(&path.to_string_lossy()) {
        Ok(()) => engine.set_status("Chemin de l'image copié"),
        Err(error) => engine.set_status(error.to_string()),
    }
}
