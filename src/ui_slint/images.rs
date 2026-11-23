//! Photos et bannières de profil pour Slint : décodées une fois (première image d'un GIF),
//! réduites et gardées en mémoire tant que le fichier ne change pas.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use std::{cell::RefCell, collections::HashMap, path::{Path, PathBuf}, time::SystemTime};

/// (fichier, taille) → (date de modification, image).
type Cache = HashMap<(PathBuf, u32), (Option<SystemTime>, Image)>;

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(HashMap::new());
}

/// Image fixe d'au plus `max` pixels de côté.
pub fn still(path: &Path, max: u32) -> Option<Image> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let key = (path.to_path_buf(), max);
    if let Some(image) = CACHE.with(|cache| cache.borrow().get(&key).filter(|(when, _)| *when == modified).map(|(_, image)| image.clone())) {
        return Some(image);
    }
    let decoded = image::open(path).ok()?;
    let decoded = if decoded.width() > max || decoded.height() > max { decoded.thumbnail(max, max) } else { decoded };
    let rgba = decoded.to_rgba8();
    let image = Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height()));
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 200 {
            cache.clear();
        }
        cache.insert(key, (modified, image.clone()));
    });
    Some(image)
}
