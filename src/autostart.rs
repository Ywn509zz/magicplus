//! Lancement de MagicPlus à l'ouverture de la session, en arrière-plan (`--demarrage`).
//!
//! - macOS : agent `~/Library/LaunchAgents/com.magicplus.app.plist`, qui ouvre l'app sans la
//!   mettre au premier plan (`open -g`).
//! - Windows : valeur « MagicPlus » de `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! - Linux : `~/.config/autostart/magicplus.desktop`.
//!
//! Le chemin enregistré est celui du programme en cours : une mise à jour (qui remplace le
//! programme sur place) le garde valable.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// Argument donné au lancement automatique : la fenêtre reste cachée.
pub const STARTUP_ARG: &str = "--demarrage";

/// Vrai si MagicPlus a été lancé par l'ouverture de session.
pub fn launched_at_login() -> bool {
    std::env::args().any(|arg| arg == STARTUP_ARG)
}

/// Programme à lancer : le paquet `.app` sur Mac s'il y en a un, sinon l'exécutable.
fn program() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("chemin du programme inconnu")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    Ok(app_bundle(&exe).unwrap_or(exe))
}

/// `…/MagicPlus.app` si `exe` est `…/MagicPlus.app/Contents/MacOS/magicplus`.
pub fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && bundle.extension()? == "app")
        .then(|| bundle.to_path_buf())
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Contenu de l'agent macOS.
pub fn launch_agent(program: &Path) -> String {
    let path = xml_escape(&program.to_string_lossy());
    let arguments = if program.extension().is_some_and(|ext| ext == "app") {
        format!(
            "    <string>/usr/bin/open</string>\n    <string>-g</string>\n    <string>-a</string>\n    <string>{path}</string>\n    \
             <string>--args</string>\n    <string>{STARTUP_ARG}</string>\n"
        )
    } else {
        format!("    <string>{path}</string>\n    <string>{STARTUP_ARG}</string>\n")
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{LABEL}</string>\n  \
         <key>ProgramArguments</key>\n  <array>\n{arguments}  </array>\n  <key>RunAtLoad</key>\n  <true/>\n  \
         <key>ProcessType</key>\n  <string>Interactive</string>\n</dict>\n</plist>\n"
    )
}

/// Fichier `.desktop` (Linux).
pub fn desktop_entry(program: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=MagicPlus\nExec=\"{}\" {STARTUP_ARG}\nX-GNOME-Autostart-enabled=true\n",
        program.to_string_lossy()
    )
}

const LABEL: &str = "com.magicplus.app";

#[cfg(target_os = "macos")]
fn entry_path() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("dossier personnel introuvable"))?;
    Ok(home.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn entry_path() -> Result<PathBuf> {
    let config = dirs::config_dir().ok_or_else(|| anyhow!("dossier de configuration introuvable"))?;
    Ok(config.join("autostart/magicplus.desktop"))
}

#[cfg(unix)]
pub fn is_enabled() -> bool {
    entry_path().is_ok_and(|path| path.exists())
}

#[cfg(unix)]
pub fn set_enabled(enabled: bool) -> Result<()> {
    let path = entry_path()?;
    if !enabled {
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("impossible de retirer {}", path.display()))?;
        }
        return Ok(());
    }
    let program = program()?;
    let content = if cfg!(target_os = "macos") { launch_agent(&program) } else { desktop_entry(&program) };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content).with_context(|| format!("impossible d'écrire {}", path.display()))
}

#[cfg(windows)]
mod windows_run {
    use super::*;
    use anyhow::bail;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "MagicPlus";

    fn reg() -> Command {
        let mut command = Command::new("reg.exe");
        // Pas de fenêtre de console.
        command.creation_flags(0x0800_0000).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        command
    }

    pub fn is_enabled() -> bool {
        reg().args(["query", KEY, "/v", VALUE]).status().is_ok_and(|status| status.success())
    }

    pub fn set_enabled(enabled: bool) -> Result<()> {
        let status = if enabled {
            let command = format!("\"{}\" {STARTUP_ARG}", program()?.to_string_lossy().trim_start_matches(r"\\?\"));
            reg().args(["add", KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &command, "/f"]).status()
        } else if is_enabled() {
            reg().args(["delete", KEY, "/v", VALUE, "/f"]).status()
        } else {
            return Ok(());
        };
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(_) => bail!("Windows a refusé la modification du démarrage automatique"),
            Err(error) => Err(anyhow!("reg.exe introuvable : {error}")),
        }
    }
}

#[cfg(windows)]
pub use windows_run::{is_enabled, set_enabled};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_and_agent() {
        let exe = Path::new("/Applications/MagicPlus.app/Contents/MacOS/magicplus");
        let bundle = app_bundle(exe).unwrap();
        assert_eq!(bundle, Path::new("/Applications/MagicPlus.app"));
        assert!(app_bundle(Path::new("/usr/local/bin/magicplus")).is_none());
        let agent = launch_agent(&bundle);
        assert!(agent.contains("<string>/usr/bin/open</string>"));
        assert!(agent.contains("<string>/Applications/MagicPlus.app</string>"));
        assert!(agent.contains("<string>--demarrage</string>"));
        assert!(agent.contains("<key>RunAtLoad</key>"));
        let plain = launch_agent(Path::new("/tmp/a&b/magicplus"));
        assert!(plain.contains("<string>/tmp/a&amp;b/magicplus</string>") && !plain.contains("/usr/bin/open"));
        assert!(desktop_entry(Path::new("/opt/magicplus")).contains("Exec=\"/opt/magicplus\" --demarrage"));
    }
}
