//! Profils, comme sur Discord : photo (fixe ou GIF animé), bannière (image ou couleur), pronoms
//! et « À propos de moi ». Le nom affiché reste celui de l'appareil.
//!
//! Chaque appareil envoie lui-même son profil (message `profile`, images à la suite de l'en-tête)
//! à ses contacts : appareils associés et membres de ses groupes. Il retient la dernière version
//! confirmée par chacun et renvoie quand elle change, dès que le contact est joignable.
//!
//! Stockage : `profil/` (le nôtre : `profil.json`, `avatar.*`, `banniere.*`, `envois.json`) et
//! `profils/<id>/` (ceux des contacts).

use crate::{
    chat::{valid_name, SenderInfo},
    engine::{explain_refusal, lock, read_ack, Engine, Inner, Target},
    protocol::{ImageInfo, Message, ProfileInfo},
    util::now_ms,
};
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};

pub const MAX_BIO: usize = 190;
pub const MAX_PRONOUNS: usize = 40;
const AVATAR_SIZE: u32 = 256;
/// Bannière au format 5:2, comme sur Discord.
const BANNER_SIZE: (u32, u32) = (960, 384);
/// Un GIF animé plus lourd est réduit à une image fixe.
const MAX_GIF_AVATAR: usize = 1_500_000;
const MAX_GIF_BANNER: usize = 2_400_000;
const MAX_IMAGE_BYTES: u64 = 3 * 1024 * 1024;
const RETRY_AFTER: Duration = Duration::from_secs(30);
const RETRY_AFTER_REFUSAL: Duration = Duration::from_secs(300);

const MINE_DIR: &str = "profil";
const OTHERS_DIR: &str = "profils";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileImage {
    Avatar,
    Banner,
}

impl ProfileImage {
    fn stem(self) -> &'static str {
        match self {
            ProfileImage::Avatar => "avatar",
            ProfileImage::Banner => "banniere",
        }
    }
}

/// Profil d'un appareil (le nôtre ou celui d'un contact), pour l'interface.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileView {
    pub id: String,
    pub name: String,
    pub bio: String,
    pub pronouns: String,
    /// Couleur de la bannière (0xRRGGBB) quand il n'y a pas d'image.
    pub color: Option<u32>,
    pub avatar: Option<PathBuf>,
    pub banner: Option<PathBuf>,
    pub me: bool,
}

pub(crate) struct ProfileStore {
    dir: PathBuf,
    mine: ProfileInfo,
    others: HashMap<String, ProfileInfo>,
    /// Dernière version de notre profil confirmée par chaque contact.
    sent: HashMap<String, u64>,
    syncing: HashSet<String>,
    retry: HashMap<String, Instant>,
    revision: u64,
}

fn image_path(dir: &Path, kind: ProfileImage, info: &Option<ImageInfo>) -> Option<PathBuf> {
    let info = info.as_ref()?;
    let path = dir.join(format!("{}.{}", kind.stem(), info.ext));
    path.is_file().then_some(path)
}

fn allowed_ext(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "gif")
}

impl ProfileStore {
    pub(crate) fn load(data_dir: &Path) -> Self {
        let mine_dir = data_dir.join(MINE_DIR);
        fn read<T: serde::de::DeserializeOwned>(path: PathBuf) -> Option<T> {
            fs::read(path).ok().and_then(|raw| serde_json::from_slice(&raw).ok())
        }
        let mine: ProfileInfo = read(mine_dir.join("profil.json")).unwrap_or_default();
        let sent: HashMap<String, u64> = read(mine_dir.join("envois.json")).unwrap_or_default();
        let mut others = HashMap::new();
        if let Ok(entries) = fs::read_dir(data_dir.join(OTHERS_DIR)) {
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().to_string();
                if let Some(info) = valid_name(&id).then(|| read::<ProfileInfo>(entry.path().join("profil.json"))).flatten() {
                    others.insert(id, info);
                }
            }
        }
        Self { dir: data_dir.to_path_buf(), mine, others, sent, syncing: HashSet::new(), retry: HashMap::new(), revision: 1 }
    }

    fn mine_dir(&self) -> PathBuf {
        self.dir.join(MINE_DIR)
    }

    fn other_dir(&self, id: &str) -> PathBuf {
        self.dir.join(OTHERS_DIR).join(id)
    }

    fn save_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
        fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    fn save_mine(&mut self) -> Result<()> {
        Self::save_json(&self.mine_dir().join("profil.json"), &self.mine)?;
        self.revision += 1;
        Ok(())
    }

    fn save_sent(&self) {
        let _ = Self::save_json(&self.mine_dir().join("envois.json"), &self.sent);
    }

    fn view(&self, id: &str, name: String, me: bool) -> ProfileView {
        let (info, dir) = if me { (&self.mine, self.mine_dir()) } else {
            match self.others.get(id) {
                Some(info) => (info, self.other_dir(id)),
                None => return ProfileView { id: id.to_string(), name, me, ..ProfileView::default() },
            }
        };
        ProfileView {
            id: id.to_string(),
            name,
            bio: info.bio.clone(),
            pronouns: info.pronouns.clone(),
            color: info.color,
            avatar: image_path(&dir, ProfileImage::Avatar, &info.avatar),
            banner: image_path(&dir, ProfileImage::Banner, &info.banner),
            me,
        }
    }

    /// Notre profil et ses images, prêts à envoyer.
    fn outgoing(&self) -> Result<(ProfileInfo, Vec<u8>)> {
        let mut body = Vec::new();
        for (kind, info) in [(ProfileImage::Avatar, &self.mine.avatar), (ProfileImage::Banner, &self.mine.banner)] {
            if let Some(info) = info {
                let bytes = fs::read(self.mine_dir().join(format!("{}.{}", kind.stem(), info.ext)))
                    .with_context(|| format!("image de profil illisible ({})", kind.stem()))?;
                body.extend_from_slice(&bytes);
            }
        }
        Ok((self.mine.clone(), body))
    }
}

// ------------------------------------------------------------------ images

/// Prépare une image choisie par l'utilisateur : recadrée au centre et réduite (photo carrée
/// 256 px, bannière 960 × 384), sauf un GIF animé assez léger, gardé tel quel.
fn prepare_image(source: &Path, kind: ProfileImage) -> Result<(Vec<u8>, &'static str)> {
    use image::{imageops::FilterType, ImageFormat, ImageReader};
    let bytes = fs::read(source).with_context(|| format!("« {} » est illisible", source.display()))?;
    let reader = ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()?;
    let format = reader.format().ok_or_else(|| anyhow!("ce fichier n'est pas une image reconnue"))?;
    let gif_limit = if kind == ProfileImage::Avatar { MAX_GIF_AVATAR } else { MAX_GIF_BANNER };
    if format == ImageFormat::Gif && bytes.len() <= gif_limit {
        image::load_from_memory_with_format(&bytes, ImageFormat::Gif).context("GIF illisible")?;
        return Ok((bytes, "gif"));
    }
    let image = reader.decode().context("image illisible")?;
    let (target_w, target_h) = match kind {
        ProfileImage::Avatar => (AVATAR_SIZE, AVATAR_SIZE),
        ProfileImage::Banner => BANNER_SIZE,
    };
    // Recadrage au centre à la bonne proportion.
    let (w, h) = (image.width().max(1), image.height().max(1));
    let ratio = target_w as f64 / target_h as f64;
    let (crop_w, crop_h) = if w as f64 / h as f64 > ratio { ((h as f64 * ratio) as u32, h) } else { (w, (w as f64 / ratio) as u32) };
    let cropped = image.crop_imm((w - crop_w) / 2, (h - crop_h) / 2, crop_w.max(1), crop_h.max(1));
    let resized = if crop_w > target_w { cropped.resize_exact(target_w, target_h, FilterType::Lanczos3) } else { cropped };
    let mut out = std::io::Cursor::new(Vec::new());
    let ext = match kind {
        ProfileImage::Avatar => {
            resized.write_to(&mut out, ImageFormat::Png)?;
            "png"
        }
        ProfileImage::Banner => {
            image::DynamicImage::ImageRgb8(resized.to_rgb8()).write_to(&mut out, ImageFormat::Jpeg)?;
            "jpg"
        }
    };
    Ok((out.into_inner(), ext))
}

fn sha_b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(Sha256::digest(bytes))
}

// ------------------------------------------------------------------ moteur

impl Inner {
    /// Nom affiché d'un contact : appareil associé, membre d'un groupe ou appareil détecté.
    pub(crate) fn contact_name(&self, id: &str) -> Option<String> {
        if let Some(name) = self.display_name(id) {
            return Some(name);
        }
        if let Some(peer) = lock(&self.trusted).get(id) {
            return Some(peer.name.clone());
        }
        if let Some(name) = lock(&self.chat).member_name(id) {
            return Some(name);
        }
        lock(&self.peers).get(id).map(|peer| peer.device.name.clone())
    }

    /// Contacts à qui envoyer notre profil, avec de quoi les joindre.
    fn profile_targets(&self) -> Vec<(String, Option<String>)> {
        let mut contacts: Vec<(String, Option<String>)> = lock(&self.trusted).keys().map(|id| (id.clone(), None)).collect();
        for (member, conversation) in lock(&self.chat).group_contacts() {
            if !contacts.iter().any(|(id, _)| id == &member) {
                contacts.push((member, Some(conversation)));
            }
        }
        contacts
    }

    fn resolve_profile_target(&self, id: &str, conversation: Option<&str>) -> Option<Target> {
        match conversation {
            None => self.target_for(id),
            Some(conversation) => self.chat_target(conversation, id),
        }
    }

    fn send_profile(&self, target: &Target) -> Result<u64> {
        let (info, body) = lock(&self.profiles).outgoing()?;
        let mut channel = self.connect(target)?;
        self.write_message(&mut channel, &Message::Profile { profile: info.clone() }, &body)?;
        let _ = channel.get_ref().shutdown(std::net::Shutdown::Write);
        match read_ack(&mut channel) {
            Some(ack) if ack.ok => Ok(info.revision),
            Some(ack) => Err(anyhow!("{}", explain_refusal(&ack, &target.name))),
            None => bail!("{} n'a pas confirmé", target.name),
        }
    }

    /// Profil reçu d'un contact (associé ou membre d'un de nos groupes).
    pub(crate) fn receive_profile(&self, sender: &SenderInfo, info: ProfileInfo, body: &[u8]) -> Result<()> {
        if !valid_name(&sender.id) {
            bail!("identifiant invalide");
        }
        if info.bio.chars().count() > MAX_BIO * 2 || info.pronouns.chars().count() > MAX_PRONOUNS * 2 {
            bail!("profil trop long");
        }
        let images: Vec<(ProfileImage, &ImageInfo)> = [(ProfileImage::Avatar, &info.avatar), (ProfileImage::Banner, &info.banner)]
            .into_iter()
            .filter_map(|(kind, image)| image.as_ref().map(|image| (kind, image)))
            .collect();
        let total: u64 = images.iter().map(|(_, image)| image.size).sum();
        if total != body.len() as u64 || images.iter().any(|(_, image)| image.size > MAX_IMAGE_BYTES || !allowed_ext(&image.ext)) {
            bail!("images de profil invalides");
        }
        let mut profiles = lock(&self.profiles);
        if profiles.others.get(&sender.id).is_some_and(|known| known.revision >= info.revision) {
            return Ok(()); // déjà à jour
        }
        let dir = profiles.other_dir(&sender.id);
        fs::create_dir_all(&dir)?;
        let mut offset = 0;
        for (kind, image) in &images {
            let bytes = &body[offset..offset + image.size as usize];
            offset += image.size as usize;
            if sha_b64(bytes) != image.sha {
                bail!("image de profil altérée");
            }
            image::load_from_memory(bytes).context("image de profil illisible")?;
            let path = dir.join(format!("{}.{}", kind.stem(), image.ext));
            let temporary = path.with_extension("tmp");
            fs::write(&temporary, bytes)?;
            fs::rename(&temporary, &path)?;
        }
        // Retire les anciennes images (autre format ou supprimées).
        for kind in [ProfileImage::Avatar, ProfileImage::Banner] {
            let keep = images.iter().find(|(k, _)| *k == kind).map(|(_, image)| image.ext.clone());
            for ext in ["png", "jpg", "gif"] {
                if keep.as_deref() != Some(ext) {
                    let _ = fs::remove_file(dir.join(format!("{}.{ext}", kind.stem())));
                }
            }
        }
        ProfileStore::save_json(&dir.join("profil.json"), &info)?;
        profiles.others.insert(sender.id.clone(), info);
        profiles.revision += 1;
        drop(profiles);
        self.chat_changed();
        self.touch();
        Ok(())
    }

    fn profile_changed(&self) {
        lock(&self.profiles).retry.clear();
        self.chat_changed();
        self.touch();
    }
}

pub(crate) fn spawn_profile_scheduler(inner: Arc<Inner>) {
    thread::spawn(move || {
        while !inner.shutdown.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_secs(2));
            let revision = lock(&inner.profiles).mine.revision;
            if revision == 0 {
                continue; // jamais rempli : rien à diffuser
            }
            for (id, conversation) in inner.profile_targets() {
                {
                    let profiles = lock(&inner.profiles);
                    let done = profiles.sent.get(&id).copied().unwrap_or(0) >= revision;
                    let waiting = profiles.retry.get(&id).is_some_and(|at| *at > Instant::now());
                    if done || waiting || profiles.syncing.contains(&id) {
                        continue;
                    }
                }
                let Some(target) = inner.resolve_profile_target(&id, conversation.as_deref()) else { continue };
                lock(&inner.profiles).syncing.insert(id.clone());
                let worker = inner.clone();
                thread::spawn(move || {
                    let result = worker.send_profile(&target);
                    let mut profiles = lock(&worker.profiles);
                    profiles.syncing.remove(&target.id);
                    match result {
                        Ok(sent) => {
                            profiles.sent.insert(target.id.clone(), sent);
                            profiles.retry.remove(&target.id);
                            profiles.save_sent();
                        }
                        Err(error) => {
                            let refused = error.to_string().contains("refus") || error.to_string().contains("illisible");
                            let delay = if refused { RETRY_AFTER_REFUSAL } else { RETRY_AFTER };
                            profiles.retry.insert(target.id.clone(), Instant::now() + delay);
                        }
                    }
                });
            }
        }
    });
}

impl Engine {
    /// Change à chaque modification d'un profil (le nôtre ou celui d'un contact).
    pub fn profiles_revision(&self) -> u64 {
        lock(&self.inner.profiles).revision
    }

    pub fn my_profile(&self) -> ProfileView {
        let identity = self.inner.identity.read().unwrap_or_else(|p| p.into_inner());
        let (id, name) = (identity.id.clone(), if identity.pseudo.is_empty() { identity.name.clone() } else { identity.pseudo.clone() });
        drop(identity);
        lock(&self.inner.profiles).view(&id, name, true)
    }

    /// Profil d'un appareil (le nôtre compris), s'il est connu.
    pub fn profile(&self, id: &str) -> Option<ProfileView> {
        if id == self.inner.own_id() {
            return Some(self.my_profile());
        }
        let name = self.inner.contact_name(id)?;
        Some(lock(&self.inner.profiles).view(id, name, false))
    }

    /// Change le texte du profil : « À propos » (Markdown, 190 caractères), pronoms, couleur.
    pub fn set_profile_details(&self, bio: &str, pronouns: &str, color: Option<u32>) -> Result<()> {
        let bio = bio.trim();
        let pronouns = pronouns.trim();
        if bio.chars().count() > MAX_BIO {
            bail!("« À propos de moi » : {MAX_BIO} caractères au plus");
        }
        if pronouns.chars().count() > MAX_PRONOUNS {
            bail!("pronoms : {MAX_PRONOUNS} caractères au plus");
        }
        {
            let mut profiles = lock(&self.inner.profiles);
            profiles.mine.bio = bio.to_string();
            profiles.mine.pronouns = pronouns.to_string();
            profiles.mine.color = color.map(|c| c & 0xFF_FFFF);
            profiles.mine.revision = now_ms().max(profiles.mine.revision + 1);
            profiles.save_mine()?;
        }
        self.inner.profile_changed();
        Ok(())
    }

    /// Nouvelle photo de profil ou bannière, à partir d'une image (PNG, JPEG, GIF, WebP, BMP).
    pub fn set_profile_image(&self, kind: ProfileImage, source: &Path) -> Result<()> {
        let (bytes, ext) = prepare_image(source, kind)?;
        {
            let mut profiles = lock(&self.inner.profiles);
            let dir = profiles.mine_dir();
            fs::create_dir_all(&dir)?;
            for old in ["png", "jpg", "gif"] {
                let _ = fs::remove_file(dir.join(format!("{}.{old}", kind.stem())));
            }
            fs::write(dir.join(format!("{}.{ext}", kind.stem())), &bytes)?;
            let info = Some(ImageInfo { ext: ext.to_string(), size: bytes.len() as u64, sha: sha_b64(&bytes) });
            match kind {
                ProfileImage::Avatar => profiles.mine.avatar = info,
                ProfileImage::Banner => profiles.mine.banner = info,
            }
            profiles.mine.revision = now_ms().max(profiles.mine.revision + 1);
            profiles.save_mine()?;
        }
        self.inner.profile_changed();
        Ok(())
    }

    pub fn remove_profile_image(&self, kind: ProfileImage) -> Result<()> {
        {
            let mut profiles = lock(&self.inner.profiles);
            let dir = profiles.mine_dir();
            for old in ["png", "jpg", "gif"] {
                let _ = fs::remove_file(dir.join(format!("{}.{old}", kind.stem())));
            }
            match kind {
                ProfileImage::Avatar => profiles.mine.avatar = None,
                ProfileImage::Banner => profiles.mine.banner = None,
            }
            profiles.mine.revision = now_ms().max(profiles.mine.revision + 1);
            profiles.save_mine()?;
        }
        self.inner.profile_changed();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_cropped_and_resized() {
        let dir = std::env::temp_dir().join(format!("magicplus-profil-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("large.png");
        image::RgbImage::from_pixel(1200, 600, image::Rgb([200, 30, 30])).save(&source).unwrap();
        let (avatar, ext) = prepare_image(&source, ProfileImage::Avatar).unwrap();
        assert_eq!(ext, "png");
        let decoded = image::load_from_memory(&avatar).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (256, 256));
        let (banner, ext) = prepare_image(&source, ProfileImage::Banner).unwrap();
        assert_eq!(ext, "jpg");
        let decoded = image::load_from_memory(&banner).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (960, 384));
        fs::write(dir.join("faux.png"), b"pas une image").unwrap();
        assert!(prepare_image(&dir.join("faux.png"), ProfileImage::Avatar).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
