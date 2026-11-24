//! Images animées : GIF et PNG animé (APNG) décodés image par image, et
//! réencodage en GIF animé (le seul format animé que tous les appareils affichent : le Mac
//! l'anime nativement et l'interface Slint le lit avec `frames`).

use anyhow::{anyhow, Result};
use image::{
    codecs::{gif::GifDecoder, png::PngDecoder},
    imageops::FilterType,
    AnimationDecoder, Frames, ImageFormat, RgbaImage,
};
use std::io::Cursor;

/// Au-delà, les images en trop sont ignorées (mémoire et temps de décodage bornés).
pub const MAX_FRAMES: usize = 500;

/// Une image d'une animation et sa durée d'affichage.
pub struct AnimFrame {
    pub image: RgbaImage,
    pub delay_ms: u32,
}

/// Les images d'une animation, ou `None` si `bytes` n'est pas animé (une seule image, format
/// sans animation, fichier illisible).
pub fn frames(bytes: &[u8]) -> Option<Vec<AnimFrame>> {
    let format = image::guess_format(bytes).ok()?;
    let decoded: Frames = match format {
        ImageFormat::Gif => GifDecoder::new(Cursor::new(bytes)).ok()?.into_frames(),
        ImageFormat::Png => {
            let decoder = PngDecoder::new(Cursor::new(bytes)).ok()?;
            if !decoder.is_apng().ok()? {
                return None;
            }
            decoder.apng().ok()?.into_frames()
        }
        _ => return None,
    };
    let mut out = Vec::new();
    for next in decoded.take(MAX_FRAMES) {
        let Ok(frame) = next else { break };
        let (numer, denom) = frame.delay().numer_denom_ms();
        // Les navigateurs affichent 100 ms quand le fichier annonce moins de 20 ms.
        let delay_ms = numer.checked_div(denom).filter(|ms| *ms >= 20).unwrap_or(100);
        out.push(AnimFrame { image: frame.into_buffer(), delay_ms });
    }
    (out.len() > 1).then_some(out)
}

/// Durée moyenne d'une image, pour un affichage à cadence fixe.
pub fn average_delay_ms(frames: &[AnimFrame], kept: usize) -> u32 {
    let total: u64 = frames.iter().map(|f| f.delay_ms as u64).sum();
    (total / kept.max(1) as u64).clamp(20, 2000) as u32
}

/// Recadre au centre à la proportion `w × h` puis réduit à cette taille.
pub fn cover(image: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let (iw, ih) = (image.width().max(1), image.height().max(1));
    let ratio = w as f64 / h as f64;
    let (cw, ch) = if iw as f64 / ih as f64 > ratio { (((ih as f64) * ratio) as u32, ih) } else { (iw, ((iw as f64) / ratio) as u32) };
    let (cw, ch) = (cw.clamp(1, iw), ch.clamp(1, ih));
    let cropped = image::imageops::crop_imm(image, (iw - cw) / 2, (ih - ch) / 2, cw, ch).to_image();
    if cw == w && ch == h {
        cropped
    } else {
        image::imageops::resize(&cropped, w, h, FilterType::Triangle)
    }
}

/// GIF animé (en boucle) de taille `w × h` à partir de `frames`, recadrées au centre. Garde une
/// image sur `step` (durées cumulées) pour alléger les longues animations.
pub fn encode_gif(frames: &[AnimFrame], w: u32, h: u32, step: usize) -> Result<Vec<u8>> {
    use image::codecs::gif::{GifEncoder, Repeat};
    let step = step.max(1);
    let mut out = Vec::new();
    {
        let mut encoder = GifEncoder::new_with_speed(&mut out, 10);
        encoder.set_repeat(Repeat::Infinite)?;
        for chunk in frames.chunks(step) {
            let delay: u32 = chunk.iter().map(|f| f.delay_ms).sum();
            let image = cover(&chunk[0].image, w, h);
            let frame = image::Frame::from_parts(image, 0, 0, image::Delay::from_numer_denom_ms(delay, 1));
            encoder.encode_frame(frame)?;
        }
    }
    if out.is_empty() {
        return Err(anyhow!("animation vide"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_gif(count: usize) -> Vec<u8> {
        let frames: Vec<AnimFrame> = (0..count)
            .map(|i| AnimFrame { image: RgbaImage::from_pixel(40, 20, image::Rgba([(i * 40) as u8, 0, 0, 255])), delay_ms: 50 })
            .collect();
        encode_gif(&frames, 40, 20, 1).unwrap()
    }

    #[test]
    fn gif_round_trip_keeps_frames_and_delays() {
        let decoded = frames(&sample_gif(5)).unwrap();
        assert_eq!(decoded.len(), 5);
        assert!(decoded.iter().all(|f| f.delay_ms == 50));
        assert_eq!(decoded[0].image.dimensions(), (40, 20));
    }

    #[test]
    fn still_images_are_not_animations() {
        assert!(frames(&sample_gif(1)).is_none());
        let mut png = Vec::new();
        RgbaImage::new(4, 4).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        assert!(frames(&png).is_none());
    }

    #[test]
    fn step_merges_delays_and_cover_crops() {
        let source: Vec<AnimFrame> = frames(&sample_gif(6)).unwrap();
        let gif = encode_gif(&source, 10, 10, 2).unwrap();
        let decoded = frames(&gif).unwrap();
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].delay_ms, 100);
        assert_eq!(decoded[0].image.dimensions(), (10, 10));
    }
}
