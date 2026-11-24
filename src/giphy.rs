//! GIF de GIPHY : recherche et tendances (seul usage d'Internet avec les mises à jour), GIF
//! favoris gardés sur cet appareil. Un GIF choisi est téléchargé puis envoyé comme une pièce
//! jointe ordinaire : les autres appareils le reçoivent en direct, sans Internet.
//!
//! Il faut une clé d'API GIPHY (gratuite, https://developers.giphy.com/dashboard/), saisie dans
//! les paramètres ou donnée par la variable d'environnement `MAGICPLUS_GIPHY_KEY`.

use crate::{engine::Engine, update::curl};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

const API: &str = "https://api.giphy.com/v1/gifs";
pub const PAGE: usize = 24;
/// Poids maximal d'un GIF téléchargé (aperçu ou GIF à envoyer).
const MAX_DOWNLOAD: u64 = 8 * 1024 * 1024;
const FAVORITES: &str = "gif-favoris.json";
const DIR: &str = "giphy";
pub const MAX_FAVORITES: usize = 200;

/// Un GIF de GIPHY : petit aperçu animé (grille) et version à envoyer (2 Mo au plus).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GifResult {
    pub id: String,
    pub title: String,
    pub preview_url: String,
    pub url: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Deserialize)]
struct Response {
    data: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: String,
    #[serde(default)]
    title: String,
    images: Images,
}

#[derive(Deserialize)]
struct Images {
    fixed_width_small: Option<Rendition>,
    fixed_width: Option<Rendition>,
    downsized: Option<Rendition>,
}

#[derive(Deserialize)]
struct Rendition {
    url: String,
    #[serde(default)]
    width: String,
    #[serde(default)]
    height: String,
}

/// Encodage d'une valeur dans une adresse (`%XX` pour tout sauf lettres, chiffres et `-_.~`).
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// Seules les adresses HTTPS de GIPHY sont téléchargées.
fn trusted(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?']).next().unwrap_or("");
    host == "giphy.com" || host.ends_with(".giphy.com")
}

fn parse(json: &str) -> Result<Vec<GifResult>> {
    let response: Response = serde_json::from_str(json).context("réponse de GIPHY illisible")?;
    Ok(response
        .data
        .into_iter()
        .filter_map(|item| {
            let preview = item.images.fixed_width_small.or_else(|| item.images.fixed_width.as_ref().map(|r| Rendition { url: r.url.clone(), width: r.width.clone(), height: r.height.clone() }))?;
            let full = item.images.downsized.or(item.images.fixed_width)?;
            let valid_id = !item.id.is_empty() && item.id.len() <= 64 && item.id.chars().all(|c| c.is_ascii_alphanumeric());
            (valid_id && trusted(&preview.url) && trusted(&full.url)).then(|| GifResult {
                id: item.id,
                title: item.title.chars().take(120).collect(),
                width: preview.width.parse().unwrap_or(100),
                height: preview.height.parse().unwrap_or(100),
                preview_url: preview.url,
                url: full.url,
            })
        })
        .collect())
}

fn download(url: &str) -> Result<Vec<u8>> {
    if !trusted(url) {
        bail!("adresse refusée");
    }
    let output = curl()
        .args(["--max-time", "30", "--max-filesize", &MAX_DOWNLOAD.to_string(), url])
        .output()
        .context("curl introuvable")?;
    if !output.status.success() {
        bail!("téléchargement impossible : {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    if output.stdout.len() as u64 > MAX_DOWNLOAD {
        bail!("GIF trop lourd");
    }
    Ok(output.stdout)
}

impl Engine {
    /// Clé d'API GIPHY (paramètres, sinon variable d'environnement `MAGICPLUS_GIPHY_KEY`).
    pub fn giphy_key(&self) -> Option<String> {
        self.snapshot()
            .settings
            .giphy_key
            .filter(|k| !k.trim().is_empty())
            .or_else(|| std::env::var("MAGICPLUS_GIPHY_KEY").ok())
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
    }

    pub fn set_giphy_key(&self, key: &str) -> Result<()> {
        let key = key.trim();
        if !key.is_empty() && (key.len() > 80 || !key.chars().all(|c| c.is_ascii_alphanumeric())) {
            bail!("clé GIPHY invalide (lettres et chiffres seulement)");
        }
        self.update_settings(|settings| settings.giphy_key = (!key.is_empty()).then(|| key.to_string()))
    }

    /// Recherche (`query` vide : tendances). Bloquant : à appeler hors du fil de l'interface.
    pub fn giphy_search(&self, query: &str, offset: usize) -> Result<Vec<GifResult>> {
        let key = self.giphy_key().ok_or_else(|| anyhow!("clé GIPHY manquante : voir Paramètres → GIF"))?;
        let query = query.trim();
        let url = if query.is_empty() {
            format!("{API}/trending?api_key={}&limit={PAGE}&offset={offset}&rating=pg-13", encode(&key))
        } else {
            format!("{API}/search?api_key={}&q={}&limit={PAGE}&offset={offset}&rating=pg-13&lang=fr", encode(&key), encode(query))
        };
        let output = curl().args(["--max-time", "20", &url]).output().context("curl introuvable")?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if error.contains("401") || error.contains("403") {
                bail!("GIPHY refuse la clé : vérifiez-la dans Paramètres → GIF");
            }
            bail!("GIPHY injoignable : {}", if error.is_empty() { "pas de réponse".to_string() } else { error });
        }
        parse(&String::from_utf8_lossy(&output.stdout))
    }

    fn giphy_dir(&self) -> PathBuf {
        self.inner.store.dir().join(DIR)
    }

    /// Aperçu animé d'un GIF (téléchargé une fois, puis gardé). Bloquant.
    pub fn giphy_preview(&self, gif: &GifResult) -> Result<PathBuf> {
        let path = self.giphy_dir().join("apercus").join(format!("{}.gif", gif.id));
        if !path.is_file() {
            let bytes = download(&gif.preview_url)?;
            fs::create_dir_all(path.parent().unwrap_or(&self.giphy_dir()))?;
            let temporary = path.with_extension("tmp");
            fs::write(&temporary, bytes)?;
            fs::rename(&temporary, &path)?;
        }
        Ok(path)
    }

    /// Télécharge le GIF puis l'envoie dans la conversation, comme une pièce jointe. Bloquant.
    pub fn send_giphy(&self, conversation: &str, gif: &GifResult) -> Result<()> {
        let path = self.giphy_dir().join("envois").join(format!("{}.gif", gif.id));
        if !path.is_file() {
            let bytes = download(&gif.url)?;
            image::load_from_memory_with_format(&bytes, image::ImageFormat::Gif).context("ce n'est pas un GIF")?;
            fs::create_dir_all(path.parent().unwrap_or(&self.giphy_dir()))?;
            fs::write(&path, bytes)?;
        }
        self.send_chat(conversation, "", None, &[path])
    }

    /// GIF favoris (les plus récents d'abord).
    pub fn gif_favorites(&self) -> Vec<GifResult> {
        self.inner.store.load(FAVORITES).unwrap_or_default()
    }

    pub fn is_gif_favorite(&self, id: &str) -> bool {
        self.gif_favorites().iter().any(|g| g.id == id)
    }

    /// Ajoute aux favoris ou en retire ; vrai s'il en fait partie ensuite.
    pub fn toggle_gif_favorite(&self, gif: &GifResult) -> bool {
        let mut favorites = self.gif_favorites();
        let favorite = match favorites.iter().position(|g| g.id == gif.id) {
            Some(index) => {
                favorites.remove(index);
                false
            }
            None => {
                favorites.insert(0, gif.clone());
                favorites.truncate(MAX_FAVORITES);
                true
            }
        };
        let _ = self.inner.store.save(FAVORITES, &favorites);
        favorite
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_giphy_responses_and_rejects_foreign_urls() {
        let json = r#"{"data":[
            {"id":"abc123","title":"Chat qui danse","images":{
                "fixed_width_small":{"url":"https://media2.giphy.com/media/abc123/100w.gif","width":"100","height":"80"},
                "downsized":{"url":"https://media2.giphy.com/media/abc123/giphy-downsized.gif","width":"480","height":"384"}}},
            {"id":"bad","title":"x","images":{
                "fixed_width_small":{"url":"https://evil.example/x.gif","width":"1","height":"1"},
                "downsized":{"url":"https://media.giphy.com/x.gif"}}},
            {"id":"../x","title":"x","images":{
                "fixed_width_small":{"url":"https://media.giphy.com/a.gif"},
                "downsized":{"url":"https://media.giphy.com/b.gif"}}}
        ],"pagination":{}}"#;
        let results = parse(json).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "abc123");
        assert_eq!((results[0].width, results[0].height), (100, 80));
        assert!(results[0].url.ends_with("downsized.gif"));
    }

    #[test]
    fn encodes_queries_and_checks_hosts() {
        assert_eq!(encode("chat qui danse & co"), "chat%20qui%20danse%20%26%20co");
        assert_eq!(encode("été"), "%C3%A9t%C3%A9");
        assert!(trusted("https://media0.giphy.com/media/x/giphy.gif"));
        assert!(!trusted("http://media.giphy.com/x.gif"));
        assert!(!trusted("https://giphy.com.evil.example/x.gif"));
    }
}
