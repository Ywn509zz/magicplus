//! Photos et bannières de profil pour Slint : décodées une fois, réduites et gardées en mémoire
//! tant que le fichier ne change pas. Une photo ou bannière animée (GIF) donne aussi ses images.

use magicplus_core::animation;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Mémoire maximale des images d'une animation ; au-delà, une image sur deux (puis trois…).
const ANIMATION_BUDGET_BYTES: usize = 16 * 1024 * 1024;

/// Image fixe et, si elle est animée, ses images et la durée de chacune.
#[derive(Clone)]
pub struct Picture {
    pub still: Image,
    pub frames: Vec<Image>,
    pub frame_ms: i32,
}

/// (fichier, taille) → (date de modification, image).
type Cache = HashMap<(PathBuf, u32), (Option<SystemTime>, Picture)>;

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(HashMap::new());
    /// Une photo animée a été préparée : l'horloge des animations doit tourner.
    static ANIMATED: Cell<bool> = const { Cell::new(false) };
}

/// Vrai si au moins une photo ou bannière animée a été préparée pour l'affichage.
pub fn any_animated() -> bool {
    ANIMATED.with(Cell::get)
}

fn to_image(rgba: &image::RgbaImage) -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height()))
}

fn shrink(image: &image::RgbaImage, max: u32) -> image::RgbaImage {
    if image.width() <= max && image.height() <= max {
        return image.clone();
    }
    let scale = (max as f32 / image.width() as f32).min(max as f32 / image.height() as f32);
    let (w, h) = (((image.width() as f32) * scale).round().max(1.0) as u32, ((image.height() as f32) * scale).round().max(1.0) as u32);
    image::imageops::resize(image, w, h, image::imageops::FilterType::Triangle)
}

/// Image d'au plus `max` pixels de côté, avec ses images si elle est animée.
pub fn picture(path: &Path, max: u32) -> Option<Picture> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let key = (path.to_path_buf(), max);
    if let Some(picture) = CACHE.with(|cache| cache.borrow().get(&key).filter(|(when, _)| *when == modified).map(|(_, p)| p.clone())) {
        return Some(picture);
    }
    let bytes = std::fs::read(path).ok()?;
    let picture = match animation::frames(&bytes) {
        Some(frames) => {
            let first = shrink(&frames[0].image, max);
            let per_frame = (first.width() * first.height() * 4) as usize;
            let step = (per_frame * frames.len()).div_ceil(ANIMATION_BUDGET_BYTES).max(1);
            let images: Vec<Image> = frames.iter().step_by(step).map(|f| to_image(&shrink(&f.image, max))).collect();
            ANIMATED.with(|a| a.set(true));
            Picture {
                still: to_image(&first),
                frame_ms: animation::average_delay_ms(&frames, images.len()) as i32,
                frames: images,
            }
        }
        None => {
            let decoded = image::load_from_memory(&bytes).ok()?;
            let decoded = if decoded.width() > max || decoded.height() > max { decoded.thumbnail(max, max) } else { decoded };
            Picture { still: to_image(&decoded.to_rgba8()), frames: Vec::new(), frame_ms: 0 }
        }
    };
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 200 {
            cache.clear();
        }
        cache.insert(key, (modified, picture.clone()));
    });
    Some(picture)
}

/// Image fixe d'au plus `max` pixels de côté (première image d'une animation).
pub fn still(path: &Path, max: u32) -> Option<Image> {
    picture(path, max).map(|p| p.still)
}
