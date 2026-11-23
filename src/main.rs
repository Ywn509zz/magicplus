// Pas de fenêtre de console sous Windows.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod shared;
#[cfg(all(target_os = "macos", not(feature = "slint-on-mac")))]
mod ui_mac;
#[cfg(any(not(target_os = "macos"), feature = "slint-on-mac"))]
mod ui_slint;

use magicplus_core::{util::platform_name, Config, Engine};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::start(Config::for_app())?;
    eprintln!("[magicplus] démarré sur {}", platform_name());

    // macOS : interface AppKit native (fluide même sur les Mac sans accélération Metal).
    // Windows et Linux : interface Slint au style Fluent.
    #[cfg(all(target_os = "macos", not(feature = "slint-on-mac")))]
    ui_mac::run(engine.clone());
    #[cfg(any(not(target_os = "macos"), feature = "slint-on-mac"))]
    ui_slint::run(engine.clone())?;

    engine.shutdown();
    Ok(())
}
