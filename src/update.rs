//! Mises à jour depuis les versions publiées sur GitHub (seule fonction qui utilise Internet,
//! facultative : sans connexion, l'app fonctionne comme avant et la recherche échoue en silence).
//!
//! 1. **Recherche** (au démarrage puis toutes les 6 h si le réglage est actif, ou à la demande) :
//!    `GET https://api.github.com/repos/<dépôt>/releases/latest`, comparaison avec notre version.
//! 2. **Téléchargement** (à la demande) : le fichier de la plateforme (`MagicPlus-windows-x64.exe`
//!    ou `MagicPlus-macos.zip`) dans `mise-a-jour/` du dossier de données, puis vérification de son
//!    SHA-256 avec `SHA256SUMS.txt` de la même version.
//! 3. **Installation** (à la demande) : Windows renomme l'exécutable en cours en `*.old.exe` et met
//!    le nouveau à sa place ; macOS remplace le paquet `MagicPlus.app`. La nouvelle version est
//!    lancée, l'interface quitte. Les restes de l'ancienne version sont effacés au démarrage suivant.
//!
//! Les requêtes passent par `curl` (fourni avec macOS et Windows 10/11) : pas de bibliothèque TLS
//! à embarquer.

use crate::engine::{lock, Engine, Inner};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};

/// Dépôt GitHub des versions publiées.
pub const DEFAULT_REPOSITORY: &str = "Ywn509zz/magicplus";
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(15);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
const UPDATE_DIR: &str = "mise-a-jour";
const CHECKSUMS: &str = "SHA256SUMS.txt";

/// Nom du fichier publié pour cette plateforme.
pub fn asset_name() -> Option<&'static str> {
    if cfg!(target_os = "windows") {
        Some("MagicPlus-windows-x64.exe")
    } else if cfg!(target_os = "macos") {
        Some("MagicPlus-macos.zip")
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum UpdateState {
    /// Pas encore cherché.
    Idle,
    Checking,
    UpToDate,
    /// Une version plus récente est publiée.
    Available,
    /// Téléchargement en cours (0 à 1).
    Downloading(f32),
    /// Téléchargée et vérifiée : il reste à redémarrer.
    Ready,
    Failed(String),
}

/// État des mises à jour, pour l'interface.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateView {
    pub current: String,
    /// Dernière version publiée connue (vide si jamais cherché).
    pub latest: String,
    /// Notes de version (Markdown), à afficher telles quelles.
    pub notes: String,
    pub state: UpdateState,
    /// Page de la version sur GitHub (téléchargement manuel).
    pub page_url: String,
    /// Faux si cette installation ne peut pas se remplacer seule (ex. lancée hors de MagicPlus.app).
    pub can_install: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Release {
    version: String,
    notes: String,
    page_url: String,
    asset_url: String,
    asset_size: u64,
    checksums_url: Option<String>,
}

pub(crate) struct UpdateStatus {
    state: UpdateState,
    release: Option<Release>,
    staged: Option<PathBuf>,
    busy: bool,
}

impl Default for UpdateStatus {
    fn default() -> Self {
        Self { state: UpdateState::Idle, release: None, staged: None, busy: false }
    }
}

// ------------------------------------------------------------------ versions

/// Notre version, ou celle imposée par `MAGICPLUS_FAKE_VERSION` (essais des mises à jour).
pub fn current_version() -> String {
    std::env::var("MAGICPLUS_FAKE_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

/// « v0.5.0 » → [0, 5, 0] ; une préversion (« 0.5.0-beta ») passe avant la version finale.
fn version_key(version: &str) -> (Vec<u64>, bool) {
    let version = version.trim().trim_start_matches(['v', 'V']);
    let (numbers, pre) = match version.split_once('-') {
        Some((numbers, _)) => (numbers, true),
        None => (version, false),
    };
    (numbers.split('.').map(|part| part.parse().unwrap_or(0)).collect(), !pre)
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (mut a, a_final) = version_key(candidate);
    let (mut b, b_final) = version_key(current);
    let length = a.len().max(b.len());
    a.resize(length, 0);
    b.resize(length, 0);
    (a, a_final) > (b, b_final)
}

// ------------------------------------------------------------------ réseau (curl)

fn curl() -> Command {
    let mut command = Command::new("curl");
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // Pas de fenêtre de console.
        command.creation_flags(0x0800_0000);
    }
    command
        .args(["--fail", "--silent", "--show-error", "--location", "--proto", "=https", "--connect-timeout", "15"])
        .args(["--user-agent", "MagicPlus-updater"])
        .stdin(Stdio::null());
    command
}

fn fetch_text(url: &str) -> Result<String> {
    let output = curl()
        .args(["--max-time", "30", "--header", "Accept: application/vnd.github+json", url])
        .output()
        .context("curl introuvable")?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("{}", if error.is_empty() { "pas de réponse".to_string() } else { error });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

fn latest_release(repository: &str) -> Result<Option<Release>> {
    let text = fetch_text(&format!("https://api.github.com/repos/{repository}/releases/latest"))?;
    let release: GithubRelease = serde_json::from_str(&text).context("réponse de GitHub illisible")?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let Some(name) = asset_name() else { return Ok(None) };
    let Some(asset) = release.assets.iter().find(|asset| asset.name == name) else { return Ok(None) };
    Ok(Some(Release {
        version: release.tag_name.trim_start_matches(['v', 'V']).to_string(),
        notes: release.body.unwrap_or_default().chars().take(4000).collect(),
        page_url: release.html_url,
        asset_url: asset.browser_download_url.clone(),
        asset_size: asset.size,
        checksums_url: release.assets.iter().find(|a| a.name == CHECKSUMS).map(|a| a.browser_download_url.clone()),
    }))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ------------------------------------------------------------------ installation

/// Paquet `.app` qui contient l'exécutable en cours (macOS).
fn running_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?.to_path_buf();
    (bundle.extension().is_some_and(|ext| ext == "app") && exe.parent()?.ends_with("Contents/MacOS")).then_some(bundle)
}

/// Cette installation peut-elle se remplacer seule ?
pub fn can_self_install() -> bool {
    if cfg!(target_os = "windows") {
        std::env::current_exe().is_ok()
    } else if cfg!(target_os = "macos") {
        running_bundle().is_some()
    } else {
        false
    }
}

/// Variables d'essai à ne pas transmettre à la version relancée (sinon elle se remettrait à jour
/// en boucle : `open` transmet l'environnement à l'app qu'il lance).
const TEST_VARIABLES: [&str; 2] = ["MAGICPLUS_UPDATE_TEST", "MAGICPLUS_FAKE_VERSION"];

fn relaunch(mut command: Command) -> Result<()> {
    for variable in TEST_VARIABLES {
        command.env_remove(variable);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("impossible de lancer la nouvelle version")?;
    Ok(())
}

fn old_windows_exe(exe: &Path) -> PathBuf {
    exe.with_extension("old.exe")
}

fn old_mac_bundle(bundle: &Path) -> Option<PathBuf> {
    Some(bundle.parent()?.join(".MagicPlus-ancienne-version.app"))
}

/// Efface ce qui reste d'une version remplacée (appelé au démarrage).
pub(crate) fn clean_previous(data_dir: &Path) {
    let leftovers: Vec<PathBuf> = if cfg!(target_os = "windows") {
        std::env::current_exe().ok().map(|exe| old_windows_exe(&exe)).into_iter().collect()
    } else if cfg!(target_os = "macos") {
        running_bundle().and_then(|bundle| old_mac_bundle(&bundle)).into_iter().collect()
    } else {
        Vec::new()
    };
    let downloads = data_dir.join(UPDATE_DIR);
    thread::spawn(move || {
        // L'ancienne version met parfois quelques secondes à se fermer.
        for _ in 0..20 {
            for path in &leftovers {
                if path.is_dir() {
                    let _ = fs::remove_dir_all(path);
                } else if path.exists() {
                    let _ = fs::remove_file(path);
                }
            }
            if leftovers.iter().all(|path| !path.exists()) {
                break;
            }
            thread::sleep(Duration::from_secs(1));
        }
        let _ = fs::remove_dir_all(downloads);
    });
}

fn install(staged: &Path) -> Result<()> {
    if cfg!(target_os = "windows") {
        let exe = std::env::current_exe()?;
        let old = old_windows_exe(&exe);
        let _ = fs::remove_file(&old);
        // Windows permet de renommer un exécutable en cours d'exécution, pas de l'écraser.
        fs::rename(&exe, &old).context("impossible de mettre l'ancienne version de côté")?;
        if fs::rename(staged, &exe).is_err() {
            if let Err(error) = fs::copy(staged, &exe) {
                let _ = fs::rename(&old, &exe);
                return Err(anyhow!("impossible d'installer la nouvelle version : {error}"));
            }
        }
        return relaunch(Command::new(&exe));
    }
    if cfg!(target_os = "macos") {
        let bundle = running_bundle().ok_or_else(|| anyhow!("MagicPlus n'a pas été lancé depuis MagicPlus.app"))?;
        let unpacked = staged.with_extension("contenu");
        let _ = fs::remove_dir_all(&unpacked);
        let status = Command::new("ditto").arg("-x").arg("-k").arg(staged).arg(&unpacked).status()?;
        let new_bundle = unpacked.join("MagicPlus.app");
        if !status.success() || !new_bundle.join("Contents/MacOS/magicplus").is_file() {
            bail!("archive de mise à jour invalide");
        }
        let old = old_mac_bundle(&bundle).ok_or_else(|| anyhow!("dossier de l'application introuvable"))?;
        let _ = fs::remove_dir_all(&old);
        fs::rename(&bundle, &old).context("impossible de mettre l'ancienne version de côté")?;
        let moved = fs::rename(&new_bundle, &bundle).is_ok()
            || Command::new("ditto").arg(&new_bundle).arg(&bundle).status().is_ok_and(|s| s.success());
        if !moved {
            let _ = fs::remove_dir_all(&bundle);
            let _ = fs::rename(&old, &bundle);
            bail!("impossible d'installer la nouvelle version");
        }
        // Relance une fois que cette instance a quitté.
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("sleep 1; /usr/bin/open -n \"$0\"").arg(&bundle);
        return relaunch(command);
    }
    bail!("mise à jour automatique indisponible sur ce système")
}

// ------------------------------------------------------------------ moteur

impl Inner {
    fn set_update_state(&self, state: UpdateState) {
        lock(&self.update).state = state;
        self.touch();
    }

    /// Cherche une nouvelle version. `quiet` : pas de message d'erreur (recherche automatique).
    pub(crate) fn check_update(self: &Arc<Self>, quiet: bool) {
        let Some(repository) = self.cfg.update_repository.clone() else { return };
        {
            let mut update = lock(&self.update);
            if update.busy {
                return;
            }
            update.busy = true;
            if !matches!(update.state, UpdateState::Ready | UpdateState::Downloading(_)) {
                update.state = UpdateState::Checking;
            }
        }
        self.touch();
        let result = latest_release(&repository);
        let current = current_version();
        let mut update = lock(&self.update);
        update.busy = false;
        match result {
            Ok(Some(release)) if is_newer(&release.version, &current) => {
                let already_known = update.release.as_ref().is_some_and(|known| known.version == release.version);
                if !matches!(update.state, UpdateState::Ready) || !already_known {
                    update.state = UpdateState::Available;
                    update.staged = None;
                }
                let version = release.version.clone();
                update.release = Some(release);
                drop(update);
                if !already_known {
                    self.set_status(format!("MagicPlus {version} est disponible : Paramètres → Mises à jour"));
                }
            }
            Ok(_) => {
                update.state = UpdateState::UpToDate;
                update.release = None;
                update.staged = None;
            }
            Err(error) => {
                update.state = if quiet { UpdateState::Idle } else { UpdateState::Failed(format!("recherche impossible : {error}")) };
            }
        }
        self.touch();
    }

    fn download_update(self: &Arc<Self>) -> Result<PathBuf> {
        let release = lock(&self.update).release.clone().ok_or_else(|| anyhow!("aucune mise à jour à télécharger"))?;
        let name = asset_name().ok_or_else(|| anyhow!("pas de version publiée pour ce système"))?;
        let dir = self.cfg.data_dir.join(UPDATE_DIR);
        fs::create_dir_all(&dir)?;
        let target = dir.join(format!("{}-{name}", release.version));
        let partial = target.with_extension("partiel");
        let _ = fs::remove_file(&partial);

        let mut child = curl()
            .args(["--max-time", "3600", "--output"])
            .arg(&partial)
            .arg(&release.asset_url)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("curl introuvable")?;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if self.shutdown.load(Ordering::SeqCst) {
                let _ = child.kill();
                bail!("arrêt de l'application");
            }
            let done = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
            let progress = if release.asset_size > 0 { (done as f32 / release.asset_size as f32).min(0.99) } else { 0.0 };
            self.set_update_state(UpdateState::Downloading(progress));
            thread::sleep(Duration::from_millis(250));
        };
        if !status.success() {
            let mut error = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                let _ = stderr.read_to_string(&mut error);
            }
            bail!("téléchargement impossible : {}", error.trim());
        }
        let size = fs::metadata(&partial)?.len();
        if release.asset_size > 0 && size != release.asset_size {
            bail!("fichier incomplet ({size} octets sur {})", release.asset_size);
        }

        // Empreinte publiée avec la version.
        let checksums_url = release.checksums_url.ok_or_else(|| anyhow!("empreintes absentes de la version publiée"))?;
        let checksums = fetch_text(&checksums_url)?;
        let expected = checksums
            .lines()
            .filter_map(|line| line.split_once(char::is_whitespace))
            .find(|(_, file)| file.trim().trim_start_matches('*') == name)
            .map(|(hash, _)| hash.trim().to_ascii_lowercase())
            .ok_or_else(|| anyhow!("empreinte de {name} absente"))?;
        let actual = sha256_file(&partial)?;
        if actual != expected {
            let _ = fs::remove_file(&partial);
            bail!("empreinte incorrecte : fichier refusé");
        }
        fs::rename(&partial, &target)?;
        Ok(target)
    }
}

impl Engine {
    pub fn update(&self) -> UpdateView {
        let update = lock(&self.inner.update);
        let release = update.release.as_ref();
        UpdateView {
            current: current_version(),
            latest: release.map(|r| r.version.clone()).unwrap_or_default(),
            notes: release.map(|r| r.notes.clone()).unwrap_or_default(),
            state: update.state.clone(),
            page_url: release
                .map(|r| r.page_url.clone())
                .or_else(|| self.inner.cfg.update_repository.as_ref().map(|repo| format!("https://github.com/{repo}/releases/latest")))
                .unwrap_or_default(),
            can_install: can_self_install(),
        }
    }

    /// Cherche maintenant une nouvelle version (en arrière-plan).
    pub fn check_update_now(&self) {
        let inner = self.inner.clone();
        thread::spawn(move || inner.check_update(false));
    }

    /// Télécharge et vérifie la nouvelle version (en arrière-plan).
    pub fn download_update(&self) {
        let inner = self.inner.clone();
        {
            let mut update = lock(&inner.update);
            if update.busy || update.release.is_none() {
                return;
            }
            update.busy = true;
            update.state = UpdateState::Downloading(0.0);
        }
        inner.touch();
        thread::spawn(move || {
            let result = inner.download_update();
            let mut update = lock(&inner.update);
            update.busy = false;
            match result {
                Ok(path) => {
                    update.staged = Some(path);
                    update.state = UpdateState::Ready;
                }
                Err(error) => update.state = UpdateState::Failed(error.to_string()),
            }
            drop(update);
            inner.touch();
        });
    }

    /// Installe la version téléchargée et lance la nouvelle. En cas de succès, l'interface doit
    /// quitter tout de suite.
    pub fn install_update(&self) -> Result<()> {
        let staged = lock(&self.inner.update).staged.clone().ok_or_else(|| anyhow!("rien à installer"))?;
        if let Err(error) = install(&staged) {
            self.inner.set_update_state(UpdateState::Failed(error.to_string()));
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn spawn_update_checker(&self) {
        crate::update::clean_previous(&self.inner.cfg.data_dir);
        if self.inner.cfg.update_repository.is_none() {
            return;
        }
        let inner = self.inner.clone();
        thread::spawn(move || {
            let mut next = Instant::now() + FIRST_CHECK_DELAY;
            while !inner.shutdown.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_secs(1));
                if Instant::now() < next {
                    continue;
                }
                next = Instant::now() + CHECK_EVERY;
                if lock(&inner.settings).auto_update {
                    inner.check_update(true);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("v0.5.0", "0.4.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.99"));
        assert!(is_newer("0.5.0", "0.5.0-beta"));
        assert!(!is_newer("0.4.0", "0.4.0"));
        assert!(!is_newer("v0.4.0", "0.4"));
        assert!(!is_newer("0.3.9", "0.4.0"));
        assert!(!is_newer("0.5.0-beta", "0.5.0"));
    }

    #[test]
    fn sha256_matches_known_value() {
        let dir = std::env::temp_dir().join(format!("magicplus-sha-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abc");
        fs::write(&file, b"abc").unwrap();
        assert_eq!(sha256_file(&file).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let _ = fs::remove_dir_all(&dir);
    }
}
