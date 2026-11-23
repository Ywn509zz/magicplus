// Pas de fenêtre de console sous Windows.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod shared;
#[cfg(all(target_os = "macos", not(feature = "slint-on-mac")))]
mod ui_mac;
#[cfg(any(not(target_os = "macos"), feature = "slint-on-mac"))]
mod ui_slint;

use magicplus_core::{util::platform_name, Config, Engine, UpdateState};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::start(Config::for_app())?;
    eprintln!("[magicplus] démarré sur {}", platform_name());

    // Développement : essaie toute la chaîne de mise à jour sans interface, puis quitte.
    // Avec MAGICPLUS_FAKE_VERSION=0.0.1 pour se croire en retard sur la version publiée.
    if std::env::var_os("MAGICPLUS_UPDATE_TEST").is_some() {
        let result = update_test(&engine);
        engine.shutdown();
        return result;
    }

    // macOS : interface AppKit native (fluide même sur les Mac sans accélération Metal).
    // Windows et Linux : interface Slint au style Fluent.
    #[cfg(all(target_os = "macos", not(feature = "slint-on-mac")))]
    ui_mac::run(engine.clone());
    #[cfg(any(not(target_os = "macos"), feature = "slint-on-mac"))]
    ui_slint::run(engine.clone())?;

    engine.shutdown();
    Ok(())
}

/// Recherche, téléchargement vérifié et installation d'une mise à jour, sans interface.
fn update_test(engine: &Engine) -> Result<(), Box<dyn std::error::Error>> {
    let wait = |what: &str, limit: Duration, done: &dyn Fn(&UpdateState) -> bool| -> UpdateState {
        let start = Instant::now();
        loop {
            let state = engine.update().state;
            if done(&state) || start.elapsed() > limit {
                eprintln!("[mise à jour] {what} : {state:?}");
                return state;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    };
    engine.check_update_now();
    let state = wait("recherche", Duration::from_secs(60), &|s| !matches!(s, UpdateState::Idle | UpdateState::Checking));
    if state != UpdateState::Available {
        return Err(format!("pas de mise à jour à installer ({state:?})").into());
    }
    let view = engine.update();
    eprintln!("[mise à jour] {} → {}", view.current, view.latest);
    engine.download_update();
    let state = wait("téléchargement", Duration::from_secs(900), &|s| matches!(s, UpdateState::Ready | UpdateState::Failed(_)));
    if state != UpdateState::Ready {
        return Err(format!("téléchargement : {state:?}").into());
    }
    engine.install_update()?;
    eprintln!("[mise à jour] installée, la nouvelle version démarre");
    Ok(())
}
