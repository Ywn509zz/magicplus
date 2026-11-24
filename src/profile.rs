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
    emoji::{CustomEmojiView, EmojiInfo},
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
/// Un GIF animé plus lourd est réencodé plus petit (voir `shrink_animation`).
const MAX_GIF_AVATAR: usize = 1_500_000;
const MAX_GIF_BANNER: usize = 2_400_000;
/// Taille d'une photo ou bannière animée réencodée (plus petite qu'une image fixe : un GIF
/// garde toutes ses images).
const ANIMATED_AVATAR_SIZE: (u32, u32) = (160, 160);
const ANIMATED_BANNER_SIZE: (u32, u32) = (600, 240);
const MAX_IMAGE_BYTES: u64 = 3 * 1024 * 1024;
/// Image choisie avant recadrage (un GIF peut être lourd).
const MAX_SOURCE_BYTES: u64 = 40 * 1024 * 1024;
const RETRY_AFTER: Duration = Duration::from_secs(30);
const RETRY_AFTER_REFUSAL: Duration = Duration::from_secs(300);

const MINE_DIR: &str = "profil";
/// Émojis personnalisés, dans le dossier de chaque profil.
const EMOJIS_DIR: &str = "emojis";
/// Récents et favoris du sélecteur d'émojis (sur cet appareil).
const EMOJI_PREFS: &str = "emojis.json";
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

    /// Largeur / hauteur de l'image enregistrée (carré, ou bannière 5:2).
    pub fn aspect(self) -> f64 {
        match self {
            ProfileImage::Avatar => 1.0,
            ProfileImage::Banner => BANNER_SIZE.0 as f64 / BANNER_SIZE.1 as f64,
        }
    }
}

/// Partie de l'image à garder : zoom (1 = le plus grand cadre possible) et position du cadre,
/// de 0 (à gauche, en haut) à 1 (à droite, en bas). Jamais d'étirement : l'image est recadrée.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crop {
    pub zoom: f32,
    pub x: f32,
    pub y: f32,
}

impl Default for Crop {
    fn default() -> Self {
        Crop { zoom: 1.0, x: 0.5, y: 0.5 }
    }
}

/// Cadre (x, y, largeur, hauteur) à garder dans une image `width` × `height`.
pub fn crop_rect(width: u32, height: u32, kind: ProfileImage, crop: Crop) -> (u32, u32, u32, u32) {
    let (w, h) = (width.max(1) as f64, height.max(1) as f64);
    let ratio = kind.aspect();
    let (base_w, base_h) = if w / h > ratio { (h * ratio, h) } else { (w, w / ratio) };
    let zoom = (crop.zoom as f64).clamp(1.0, 8.0);
    let (cw, ch) = ((base_w / zoom).max(1.0).min(w), (base_h / zoom).max(1.0).min(h));
    let x = (w - cw) * (crop.x as f64).clamp(0.0, 1.0);
    let y = (h - ch) * (crop.y as f64).clamp(0.0, 1.0);
    (x.round() as u32, y.round() as u32, (cw.round() as u32).max(1), (ch.round() as u32).max(1))
}

/// Image choisie, gardée en mémoire (réduite) pour l'aperçu du recadrage.
pub struct CropSource {
    image: image::RgbaImage,
    /// GIF animé : il le restera après recadrage.
    pub animated: bool,
}

impl CropSource {
    pub fn open(path: &Path) -> Result<CropSource> {
        let bytes = fs::read(path).with_context(|| format!("« {} » est illisible", path.display()))?;
        if bytes.len() as u64 > MAX_SOURCE_BYTES {
            bail!("image trop lourde (40 Mo au plus)");
        }
        let format = image::guess_format(&bytes).map_err(|_| anyhow!("ce fichier n'est pas une image reconnue"))?;
        let image = image::load_from_memory(&bytes).context("image illisible")?;
        let image = if image.width().max(image.height()) > 1400 { image.thumbnail(1400, 1400) } else { image };
        Ok(CropSource { image: image.to_rgba8(), animated: format == image::ImageFormat::Gif })
    }

    /// Aperçu du résultat, `width` pixels de large.
    pub fn preview(&self, kind: ProfileImage, crop: Crop, width: u32) -> image::RgbaImage {
        let (x, y, w, h) = crop_rect(self.image.width(), self.image.height(), kind, crop);
        let part = image::imageops::crop_imm(&self.image, x, y, w, h).to_image();
        let height = ((width as f64) / kind.aspect()).round().max(1.0) as u32;
        image::imageops::resize(&part, width.max(1), height, image::imageops::FilterType::Triangle)
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

/// Tout ce qu'affiche le profil en grand.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileDetails {
    pub profile: ProfileView,
    pub device_name: String,
    pub pseudo: String,
    pub platform: String,
    pub fingerprint: String,
    pub online: bool,
    /// Associé (discussion à deux possible).
    pub trusted: bool,
    /// Groupes en commun : (conversation, nom).
    pub groups: Vec<(String, String)>,
    pub friend: crate::friends::FriendState,
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
    /// Contacts dont des images d'émojis manquent, et quand les redemander.
    emojis_wanted: HashMap<String, Instant>,
    emojis_fetching: HashSet<String>,
}

fn image_path(dir: &Path, kind: ProfileImage, info: &Option<ImageInfo>) -> Option<PathBuf> {
    let info = info.as_ref()?;
    let path = dir.join(format!("{}.{}", kind.stem(), info.ext));
    path.is_file().then_some(path)
}

fn emoji_path(profile_dir: &Path, emoji: &EmojiInfo) -> PathBuf {
    profile_dir.join(EMOJIS_DIR).join(format!("{}.{}", emoji.id, emoji.ext))
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
        let mut store = Self {
            dir: data_dir.to_path_buf(),
            mine,
            others,
            sent,
            syncing: HashSet::new(),
            retry: HashMap::new(),
            revision: 1,
            emojis_wanted: HashMap::new(),
            emojis_fetching: HashSet::new(),
        };
        let now = Instant::now();
        let wanted: Vec<String> = store.others.keys().filter(|id| !store.missing_emojis(id).is_empty()).cloned().collect();
        store.emojis_wanted.extend(wanted.into_iter().map(|id| (id, now)));
        store
    }

    /// Émojis d'un contact dont l'image n'est pas encore là.
    fn missing_emojis(&self, id: &str) -> Vec<EmojiInfo> {
        let dir = self.other_dir(id);
        self.others.get(id).map(|info| info.emojis.iter().filter(|e| !emoji_path(&dir, e).is_file()).cloned().collect()).unwrap_or_default()
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

    /// Notre profil et ses images (photo puis bannière), prêts à envoyer.
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

    /// Émojis personnalisés connus : les nôtres, puis ceux de nos contacts (`owner` : identifiant
    /// du contact, remplacé par son nom par l'appelant, hors verrou).
    fn custom_emojis(&self) -> Vec<CustomEmojiView> {
        let mut all = Vec::new();
        let mut add = |info: &ProfileInfo, dir: PathBuf, mine: bool, owner: String| {
            for emoji in &info.emojis {
                let path = emoji_path(&dir, emoji);
                if path.is_file() {
                    all.push(CustomEmojiView {
                        id: emoji.id.clone(),
                        name: emoji.name.clone(),
                        path,
                        animated: emoji.ext == "gif",
                        mine,
                        owner: owner.clone(),
                    });
                }
            }
        };
        add(&self.mine, self.mine_dir(), true, String::new());
        let mut others: Vec<_> = self.others.iter().collect();
        others.sort_by(|a, b| a.0.cmp(b.0));
        for (id, info) in others {
            add(info, self.other_dir(id), false, id.clone());
        }
        all
    }
}

// ------------------------------------------------------------------ images

/// Prépare une image choisie par l'utilisateur : recadrée (cadre `crop`) puis réduite (photo carrée
/// 256 px, bannière 960 × 384). Une animation (GIF ou PNG animé) est recadrée image par image et
/// reste animée, en GIF.
fn prepare_image(source: &Path, kind: ProfileImage, crop: Crop) -> Result<(Vec<u8>, &'static str)> {
    use image::{imageops::FilterType, ImageFormat, ImageReader};
    let bytes = fs::read(source).with_context(|| format!("« {} » est illisible", source.display()))?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        bail!("image trop lourde (40 Mo au plus)");
    }
    let reader = ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()?;
    let format = reader.format().ok_or_else(|| anyhow!("ce fichier n'est pas une image reconnue"))?;
    let gif_limit = if kind == ProfileImage::Avatar { MAX_GIF_AVATAR } else { MAX_GIF_BANNER };
    if let Some(frames) = crate::animation::frames(&bytes) {
        // Animation (GIF ou PNG animé) : gardée animée, en GIF, le format que tous les
        // appareils affichent. Un GIF déjà léger, pas trop grand et pas recadré est gardé tel quel.
        let (w, h) = (frames[0].image.width(), frames[0].image.height());
        let (target_w, target_h) = match kind {
            ProfileImage::Avatar => ANIMATED_AVATAR_SIZE,
            ProfileImage::Banner => ANIMATED_BANNER_SIZE,
        };
        if crop == Crop::default() && format == ImageFormat::Gif && bytes.len() <= gif_limit && w <= target_w * 2 && h <= target_h * 2 {
            return Ok((bytes, "gif"));
        }
        // Même cadre pour toutes les images de l'animation.
        let (x, y, cw, ch) = crop_rect(w, h, kind, crop);
        let cropped: Vec<crate::animation::AnimFrame> = frames
            .iter()
            .map(|frame| crate::animation::AnimFrame {
                image: image::imageops::crop_imm(&frame.image, x, y, cw, ch).to_image(),
                delay_ms: frame.delay_ms,
            })
            .collect();
        if let Some(gif) = shrink_animation(&cropped, (target_w, target_h), gif_limit) {
            return Ok((gif, "gif"));
        }
        // Vraiment trop lourde : première image seulement (ci-dessous).
    }
    let image = reader.decode().context("image illisible")?;
    let (target_w, target_h) = match kind {
        ProfileImage::Avatar => (AVATAR_SIZE, AVATAR_SIZE),
        ProfileImage::Banner => BANNER_SIZE,
    };
    let (x, y, w, h) = crop_rect(image.width(), image.height(), kind, crop);
    let cropped = image.crop_imm(x, y, w, h);
    let resized = if w > target_w { cropped.resize_exact(target_w, target_h, FilterType::Lanczos3) } else { cropped };
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

/// Réencode une animation en GIF de taille `size` (ou plus petit) qui tient dans `limit` octets :
/// on réduit d'abord le nombre d'images, puis la taille. `None` si rien ne tient.
fn shrink_animation(frames: &[crate::animation::AnimFrame], (w, h): (u32, u32), limit: usize) -> Option<Vec<u8>> {
    // Au plus ~120 images (une sur deux, trois… au-delà).
    let base_step = frames.len().div_ceil(120).max(1);
    for scale in [1.0, 0.75, 0.5] {
        let (sw, sh) = (((w as f64) * scale) as u32, ((h as f64) * scale) as u32);
        for step in [base_step, base_step * 2, base_step * 3] {
            let Ok(gif) = crate::animation::encode_gif(frames, sw.max(16), sh.max(16), step) else { return None };
            if gif.len() <= limit {
                return Some(gif);
            }
        }
    }
    None
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
        let _ = channel.finish();
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
        if info.emojis.len() > crate::emoji::MAX_CUSTOM
            || info.emojis.iter().any(|e| {
                e.size > crate::emoji::MAX_CUSTOM_BYTES as u64
                    || !crate::emoji::valid_name(&e.name)
                    || !crate::emoji::valid_id(&e.id)
                    || !matches!(e.ext.as_str(), "png" | "gif")
            })
        {
            bail!("émojis de profil invalides");
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
        let emojis_dir = dir.join(EMOJIS_DIR);
        // Retire les émojis qu'il n'a plus.
        if let Ok(entries) = fs::read_dir(&emojis_dir) {
            for entry in entries.flatten() {
                let keep = info.emojis.iter().any(|e| entry.file_name().to_string_lossy() == format!("{}.{}", e.id, e.ext));
                if !keep {
                    let _ = fs::remove_file(entry.path());
                }
            }
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
        if !profiles.missing_emojis(&sender.id).is_empty() {
            profiles.emojis_wanted.insert(sender.id.clone(), Instant::now());
        }
        drop(profiles);
        self.chat_changed();
        self.touch();
        Ok(())
    }

    fn fetch_emoji(&self, target: &Target, emoji: &EmojiInfo) -> Result<Vec<u8>> {
        use base64::Engine as _;
        let mut channel = self.connect(target)?;
        self.write_message(&mut channel, &Message::EmojiFetch { id: emoji.id.clone() }, &[])?;
        let _ = channel.finish();
        let ack = read_ack(&mut channel).ok_or_else(|| anyhow!("{} ne répond pas", target.name))?;
        if !ack.ok {
            bail!("{}", explain_refusal(&ack, &target.name));
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(ack.data.as_bytes()).context("réponse illisible")?;
        crate::emoji::check_received(emoji, &bytes)?;
        Ok(bytes)
    }

    /// Image d'un de nos émojis, pour un contact (appareil associé ou membre d'un de nos groupes).
    pub(crate) fn serve_emoji(&self, sender: &SenderInfo, id: &str) -> Result<String> {
        use base64::Engine as _;
        if !self.profile_targets().iter().any(|(contact, _)| *contact == sender.id) {
            bail!("émojis réservés aux contacts");
        }
        let profiles = lock(&self.profiles);
        let emoji = profiles.mine.emojis.iter().find(|e| e.id == id).ok_or_else(|| anyhow!("émoji inconnu"))?;
        let bytes = fs::read(emoji_path(&profiles.mine_dir(), emoji))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    fn profile_changed(&self) {
        lock(&self.profiles).retry.clear();
        self.chat_changed();
        self.touch();
    }
}

/// Demande à `owner` les images de ses émojis qui nous manquent ; redemande plus tard en cas
/// d'échec.
fn fetch_missing_emojis(inner: &Arc<Inner>, owner: &str) {
    let (dir, missing) = {
        let profiles = lock(&inner.profiles);
        (profiles.other_dir(owner), profiles.missing_emojis(owner))
    };
    let conversation = inner.profile_targets().into_iter().find(|(id, _)| id == owner).and_then(|(_, c)| c);
    let target = inner.resolve_profile_target(owner, conversation.as_deref());
    let mut fetched = false;
    let mut failed = target.is_none();
    if let Some(target) = target {
        let _ = fs::create_dir_all(dir.join(EMOJIS_DIR));
        for emoji in missing {
            let saved = inner.fetch_emoji(&target, &emoji).and_then(|bytes| {
                let path = emoji_path(&dir, &emoji);
                let temporary = path.with_extension("tmp");
                fs::write(&temporary, &bytes)?;
                fs::rename(&temporary, &path)?;
                Ok(())
            });
            match saved {
                Ok(()) => fetched = true,
                Err(error) => {
                    eprintln!("[profils] émoji « {} » de {} : {error}", emoji.name, target.name);
                    failed = true;
                }
            }
        }
    }
    let mut profiles = lock(&inner.profiles);
    profiles.emojis_fetching.remove(owner);
    if failed {
        profiles.emojis_wanted.insert(owner.to_string(), Instant::now() + RETRY_AFTER);
    }
    if fetched {
        profiles.revision += 1;
        drop(profiles);
        inner.chat_changed();
        inner.touch();
    }
}

pub(crate) fn spawn_profile_scheduler(inner: Arc<Inner>) {
    thread::spawn(move || {
        while !inner.shutdown.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_secs(2));
            // Images d'émojis de contacts à demander.
            let due: Vec<String> = {
                let mut profiles = lock(&inner.profiles);
                let now = Instant::now();
                let due: Vec<String> = profiles.emojis_wanted.iter().filter(|(_, at)| **at <= now).map(|(id, _)| id.clone()).collect();
                for id in &due {
                    profiles.emojis_wanted.remove(id);
                    profiles.emojis_fetching.insert(id.clone());
                }
                due
            };
            for owner in due {
                let worker = inner.clone();
                thread::spawn(move || fetch_missing_emojis(&worker, &owner));
            }
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

    /// Profil en grand : profil, appareil, groupes en commun, relation.
    pub fn profile_details(&self, id: &str) -> Option<ProfileDetails> {
        let inner = &self.inner;
        let profile = self.profile(id)?;
        let me = profile.me;
        let pseudo = if me {
            inner.identity.read().unwrap_or_else(|p| p.into_inner()).pseudo.clone()
        } else {
            lock(&inner.pseudos).get(id).cloned().unwrap_or_default()
        };
        let (device_name, platform, public_key, online) = if me {
            let identity = inner.identity.read().unwrap_or_else(|p| p.into_inner());
            (identity.name.clone(), identity.platform.clone(), identity.public_key.clone(), true)
        } else if let Some(peer) = lock(&inner.peers).get(id) {
            (peer.device.name.clone(), peer.device.platform.clone(), peer.device.public_key.clone(), peer.online())
        } else if let Some(peer) = lock(&inner.trusted).get(id) {
            (peer.name.clone(), peer.platform.clone(), peer.public_key.clone(), false)
        } else {
            let member = inner.any_member(id)?;
            (member.name, member.platform, member.public_key, false)
        };
        let trusted = !me && lock(&inner.trusted).get(id).is_some_and(|peer| peer.public_key == public_key);
        let member = crate::protocol::MemberInfo { id: id.to_string(), name: device_name.clone(), platform: platform.clone(), public_key: public_key.clone() };
        let friend = inner.friend_states()(&member, me);
        Some(ProfileDetails {
            profile,
            device_name,
            pseudo,
            platform,
            fingerprint: crate::crypto::fingerprint(&public_key),
            online,
            trusted,
            groups: if me { Vec::new() } else { inner.shared_groups(id) },
            friend,
        })
    }

    /// Nouvelle photo de profil ou bannière, à partir d'une image (PNG, JPEG, GIF, BMP),
    /// recadrée au centre.
    pub fn set_profile_image(&self, kind: ProfileImage, source: &Path) -> Result<()> {
        self.set_profile_image_cropped(kind, source, Crop::default())
    }

    /// Nouvelle photo de profil ou bannière, recadrée comme l'utilisateur l'a réglé. Un GIF animé
    /// peut prendre quelques secondes : à appeler hors du fil de l'interface.
    pub fn set_profile_image_cropped(&self, kind: ProfileImage, source: &Path, crop: Crop) -> Result<()> {
        let (bytes, ext) = prepare_image(source, kind, crop)?;
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

    // ------------------------------------------------------------ émojis

    /// Émojis personnalisés utilisables : les nôtres puis ceux de nos contacts.
    pub fn custom_emojis(&self) -> Vec<CustomEmojiView> {
        let mut all = lock(&self.inner.profiles).custom_emojis();
        for emoji in all.iter_mut().filter(|e| !e.mine) {
            emoji.owner = self.inner.display_name(&emoji.owner).unwrap_or_default();
        }
        all
    }

    /// Émoji personnalisé d'après son identifiant (le nôtre ou celui d'un contact).
    pub fn custom_emoji(&self, id: &str) -> Option<CustomEmojiView> {
        self.custom_emojis().into_iter().find(|e| e.id == id)
    }

    /// Nouvel émoji personnalisé à partir d'une image (fixe ou animée). Le nom est tiré du
    /// fichier s'il n'est pas donné, et rendu unique parmi les nôtres.
    pub fn add_custom_emoji(&self, source: &Path, name: Option<&str>) -> Result<CustomEmojiView> {
        let (bytes, ext) = crate::emoji::prepare(source)?;
        let id = crate::emoji::id_of(&bytes);
        let mut base = name.map(str::to_string).unwrap_or_else(|| crate::emoji::name_from_file(source));
        if !crate::emoji::valid_name(&base) {
            bail!("nom d'émoji invalide : 2 à 32 lettres sans accent, chiffres ou _");
        }
        {
            let mut profiles = lock(&self.inner.profiles);
            if profiles.mine.emojis.iter().any(|e| e.id == id) {
                bail!("cet émoji existe déjà");
            }
            if profiles.mine.emojis.len() >= crate::emoji::MAX_CUSTOM {
                bail!("{} émojis personnalisés au plus", crate::emoji::MAX_CUSTOM);
            }
            let mut n = 2;
            let original = base.clone();
            while profiles.mine.emojis.iter().any(|e| e.name == base) {
                base = format!("{}_{n}", &original[..original.len().min(29)]);
                n += 1;
            }
            let info = EmojiInfo { name: base, id, ext: ext.to_string(), size: bytes.len() as u64 };
            let path = emoji_path(&profiles.mine_dir(), &info);
            fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
            fs::write(&path, &bytes)?;
            profiles.mine.emojis.push(info);
            profiles.mine.revision = now_ms().max(profiles.mine.revision + 1);
            profiles.save_mine()?;
        }
        self.inner.profile_changed();
        let added = lock(&self.inner.profiles).mine.emojis.last().cloned();
        let added = added.ok_or_else(|| anyhow!("émoji introuvable"))?;
        self.custom_emoji(&added.id).ok_or_else(|| anyhow!("émoji introuvable"))
    }

    /// Retire un de nos émojis personnalisés (les messages qui l'utilisent montreront :nom:).
    pub fn remove_custom_emoji(&self, id: &str) -> Result<()> {
        {
            let mut profiles = lock(&self.inner.profiles);
            let Some(index) = profiles.mine.emojis.iter().position(|e| e.id == id) else { bail!("émoji inconnu") };
            let info = profiles.mine.emojis.remove(index);
            let _ = fs::remove_file(emoji_path(&profiles.mine_dir(), &info));
            profiles.mine.revision = now_ms().max(profiles.mine.revision + 1);
            profiles.save_mine()?;
        }
        self.inner.profile_changed();
        Ok(())
    }

    /// Émojis récents et favoris de cet appareil.
    pub fn emoji_prefs(&self) -> crate::emoji::EmojiPrefs {
        self.inner.store.load(EMOJI_PREFS).unwrap_or_default()
    }

    /// Un émoji vient d'être choisi (envoyé ou ajouté en réaction) : il passe en tête des récents.
    pub fn note_emoji_used(&self, emoji: &str) {
        let mut prefs = self.emoji_prefs();
        prefs.used(emoji);
        let _ = self.inner.store.save(EMOJI_PREFS, &prefs);
    }

    /// Ajoute aux favoris ou en retire ; vrai s'il en fait partie ensuite.
    pub fn toggle_favorite_emoji(&self, emoji: &str) -> bool {
        let mut prefs = self.emoji_prefs();
        let favorite = prefs.toggle_favorite(emoji);
        let _ = self.inner.store.save(EMOJI_PREFS, &prefs);
        favorite
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
        let (avatar, ext) = prepare_image(&source, ProfileImage::Avatar, Crop::default()).unwrap();
        assert_eq!(ext, "png");
        let decoded = image::load_from_memory(&avatar).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (256, 256));
        let (banner, ext) = prepare_image(&source, ProfileImage::Banner, Crop::default()).unwrap();
        assert_eq!(ext, "jpg");
        let decoded = image::load_from_memory(&banner).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (960, 384));
        fs::write(dir.join("faux.png"), b"pas une image").unwrap();
        assert!(prepare_image(&dir.join("faux.png"), ProfileImage::Avatar, Crop::default()).is_err());

        // Cadre : jamais hors de l'image, à la bonne proportion, plus petit avec le zoom.
        assert_eq!(crop_rect(1200, 600, ProfileImage::Avatar, Crop::default()), (300, 0, 600, 600));
        assert_eq!(crop_rect(1200, 600, ProfileImage::Avatar, Crop { zoom: 2.0, x: 0.0, y: 1.0 }), (0, 300, 300, 300));
        assert_eq!(crop_rect(1000, 1000, ProfileImage::Banner, Crop { zoom: 1.0, x: 0.5, y: 0.0 }), (0, 0, 1000, 400));
        let (_, _, w, h) = crop_rect(1000, 1000, ProfileImage::Banner, Crop { zoom: 20.0, x: 2.0, y: -1.0 });
        assert_eq!((w, h), (125, 50));

        // Aperçu.
        let preview = CropSource::open(&source).unwrap().preview(ProfileImage::Banner, Crop::default(), 300);
        assert_eq!((preview.width(), preview.height()), (300, 120));

        // GIF animé : recadré, toujours animé.
        let gif_path = dir.join("anime.gif");
        {
            use image::{codecs::gif::GifEncoder, Delay, Frame};
            let mut file = fs::File::create(&gif_path).unwrap();
            let mut encoder = GifEncoder::new(&mut file);
            for shade in [40_u8, 200] {
                let frame = image::RgbaImage::from_pixel(400, 300, image::Rgba([shade, 0, 0, 255]));
                encoder.encode_frame(Frame::from_parts(frame, 0, 0, Delay::from_numer_denom_ms(100, 1))).unwrap();
            }
        }
        let (gif, ext) = prepare_image(&gif_path, ProfileImage::Banner, Crop { zoom: 1.0, x: 0.0, y: 0.0 }).unwrap();
        assert_eq!(ext, "gif");
        assert!(crate::animation::frames(&gif).is_some_and(|frames| frames.len() > 1));
        let decoded = image::load_from_memory(&gif).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (600, 240));
        let _ = fs::remove_dir_all(&dir);
    }
}
