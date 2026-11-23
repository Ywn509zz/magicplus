//! Stockage local en JSON : identité, appareils associés, réglages.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{fs, io, path::PathBuf};

pub fn default_data_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MagicPlus")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredIdentity {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub public_key: String,
    pub secret_key: String,
}

/// Appareil associé : on mémorise sa clé publique au moment de l'association (après
/// vérification de l'empreinte par l'utilisateur), jamais celle annoncée plus tard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrustedPeer {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Dossier où ranger les fichiers reçus (`None` : dossier Téléchargements).
    pub save_folder: Option<String>,
    /// Durée pendant laquelle un transfert interrompu peut reprendre (heures).
    pub resume_hours: u64,
    /// Mettre automatiquement le texte reçu dans le presse-papiers.
    pub auto_copy_received: bool,
    pub dark_mode: Option<bool>,
    /// Adresses (IP ou nom, port facultatif) des appareils à contacter directement.
    pub manual_addresses: Vec<String>,
    /// Chercher seul les nouvelles versions sur GitHub (seul usage d'Internet).
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            save_folder: None,
            resume_hours: 48,
            auto_copy_received: true,
            dark_mode: None,
            manual_addresses: Vec::new(),
            auto_update: true,
        }
    }
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn load<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let raw = fs::read(self.dir.join(name)).ok()?;
        serde_json::from_slice(&raw).ok()
    }

    /// Écriture atomique (fichier temporaire puis renommage), lisible par l'utilisateur seul.
    pub fn save<T: Serialize>(&self, name: &str, value: &T) -> io::Result<()> {
        let path = self.dir.join(name);
        let temporary = self.dir.join(format!("{name}.tmp"));
        let encoded = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
        fs::write(&temporary, encoded)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(temporary, path)
    }
}
