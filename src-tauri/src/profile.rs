//! Profile channels: `updateProfile`, `getProfileImageMetadata`,
//! `processProfileImage`, `cropProfileImage` (ports of `src/main/events/profile/`).

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::imageops::{self, FilterType};
use image::{AnimationDecoder, Frame, ImageFormat, RgbaImage};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::hydra_api;
use crate::library::now_ms;
use crate::store::Store;

/// MIME type from the file's magic bytes; an animated PNG reads as `image/apng` like file-type.
fn mime_of(bytes: &[u8]) -> Option<&'static str> {
    let format = image::guess_format(bytes).ok()?;
    if format == ImageFormat::Png && png_animated(bytes) == Some(true) {
        return Some("image/apng");
    }
    Some(format.to_mime_type())
}

fn skip_gif_sub_blocks(b: &[u8], mut offset: usize) -> Option<usize> {
    while offset < b.len() {
        let size = b[offset] as usize;
        offset += 1;
        if size == 0 {
            return Some(offset);
        }
        offset += size;
    }
    None
}

fn gif_animated(b: &[u8]) -> Option<bool> {
    if b.len() < 13 {
        return None;
    }
    let mut frames = 0;
    let mut offset = 13;
    if b[10] & 0x80 != 0 {
        offset += 3 << ((b[10] & 0x07) + 1);
    }
    while offset < b.len() {
        let marker = b[offset];
        offset += 1;
        match marker {
            0x2c => {
                frames += 1;
                if frames > 1 {
                    return Some(true);
                }
                if offset + 9 > b.len() {
                    return None;
                }
                let packed = b[offset + 8];
                offset += 9;
                if packed & 0x80 != 0 {
                    offset += 3 << ((packed & 0x07) + 1);
                }
                offset = skip_gif_sub_blocks(b, offset + 1)?;
            }
            0x21 => offset = skip_gif_sub_blocks(b, offset + 1)?,
            0x3b => return Some(false),
            _ => return None,
        }
    }
    None
}

fn webp_animated(b: &[u8]) -> Option<bool> {
    if b.len() < 21 {
        return None;
    }
    if &b[0..4] != b"RIFF" || &b[8..12] != b"WEBP" {
        return Some(false);
    }
    let mut offset = 12;
    while offset + 8 <= b.len() {
        let kind = &b[offset..offset + 4];
        let size = u32::from_le_bytes(b[offset + 4..offset + 8].try_into().ok()?) as usize;
        let data = offset + 8;
        if kind == b"VP8X" {
            return Some(data < b.len() && b[data] & 0x02 != 0);
        }
        if kind == b"ANIM" {
            return Some(true);
        }
        offset = data + size + size % 2;
    }
    None
}

fn png_animated(b: &[u8]) -> Option<bool> {
    const SIGNATURE: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    if b.len() < SIGNATURE.len() + 12 {
        return None;
    }
    if &b[..8] != SIGNATURE {
        return Some(false);
    }
    let mut offset = 8;
    while offset + 12 <= b.len() {
        let size = u32::from_be_bytes(b[offset..offset + 4].try_into().ok()?) as usize;
        match &b[offset + 4..offset + 8] {
            b"acTL" => return Some(true),
            b"IDAT" | b"IEND" => return Some(false),
            _ => offset += 12 + size,
        }
    }
    None
}

/// Electron falls back to "animated" when it cannot tell, so the editor keeps every frame.
fn is_animated(mime: Option<&str>, bytes: &[u8]) -> bool {
    match mime {
        Some("image/gif") => gif_animated(bytes),
        Some("image/webp") => webp_animated(bytes),
        Some("image/png" | "image/apng") => png_animated(bytes),
        _ => Some(false),
    }
    .unwrap_or(true)
}

/// `getProfileImageMetadata`.
pub fn image_metadata(path: &str) -> Value {
    let Ok(bytes) = fs::read(path) else { return json!({ "mimeType": null, "isAnimated": false }) };
    let mime = mime_of(&bytes);
    json!({ "mimeType": mime, "isAnimated": is_animated(mime, &bytes) })
}

fn temp_output(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hydra-temp-{}-{name}", now_ms()))
}

/// `processProfileImage`: static images pass through; animated ones become their first frame as WebP.
pub fn process_profile_image(path: &str) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|_| "Image file not found".to_string())?;
    let mime = mime_of(&bytes);
    if !is_animated(mime, &bytes) {
        return Ok(json!({ "imagePath": path, "mimeType": mime.unwrap_or("application/octet-stream") }));
    }
    let output = temp_output("profile.webp");
    let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    image.to_rgba8().save_with_format(&output, ImageFormat::WebP).map_err(|e| e.to_string())?;
    Ok(json!({ "imagePath": output.to_string_lossy(), "mimeType": "image/webp" }))
}

fn decode_frames(mime: Option<&str>, bytes: &[u8]) -> Result<Vec<Frame>, String> {
    let frames = match mime {
        Some("image/gif") => GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?.into_frames().collect_frames(),
        Some("image/webp") => WebPDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?.into_frames().collect_frames(),
        _ => PngDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?.apng().map_err(|e| e.to_string())?.into_frames().collect_frames(),
    };
    frames.map_err(|e| e.to_string())
}

struct Crop {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    out_w: u32,
    out_h: u32,
    rotation: u32,
}

impl Crop {
    /// Clamps the editor's rectangle like Electron, in the rotated frame's space.
    fn new(p: &Value, source_w: u32, source_h: u32) -> Self {
        let rotation = (((p["rotation"].as_f64().unwrap_or(0.0) as i64) % 360 + 360) % 360) as u32;
        let (fw, fh) = if rotation == 90 || rotation == 270 { (source_h, source_w) } else { (source_w, source_h) };
        let num = |k: &str| p[k].as_f64().unwrap_or(0.0).round() as i64;
        let left = num("left").clamp(0, fw as i64 - 1);
        let top = num("top").clamp(0, fh as i64 - 1);
        let width = num("width").clamp(1, fw as i64 - left);
        let height = num("height").clamp(1, fh as i64 - top);
        let out = |k: &str| (p[k].as_f64().unwrap_or(1.0).round() as u32).clamp(1, 4096);
        Crop { left: left as u32, top: top as u32, width: width as u32, height: height as u32, out_w: out("outputWidth"), out_h: out("outputHeight"), rotation }
    }

    fn apply(&self, frame: &RgbaImage) -> RgbaImage {
        let rotated = match self.rotation {
            90 => imageops::rotate90(frame),
            180 => imageops::rotate180(frame),
            270 => imageops::rotate270(frame),
            _ => frame.clone(),
        };
        let cropped = imageops::crop_imm(&rotated, self.left, self.top, self.width, self.height).to_image();
        imageops::resize(&cropped, self.out_w, self.out_h, FilterType::Triangle)
    }
}

/// `cropProfileImage`. Static images come out as WebP like Electron; animated ones as
/// an animated GIF, since the `image` crate cannot encode animated WebP.
pub fn crop_profile_image(path: &str, params: &Value) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let mime = mime_of(&bytes);
    if is_animated(mime, &bytes) {
        let frames = decode_frames(mime, &bytes)?;
        let first = frames.first().ok_or("Could not read source image dimensions")?;
        let crop = Crop::new(params, first.buffer().width(), first.buffer().height());
        let output = temp_output("profile-crop.gif");
        let file = fs::File::create(&output).map_err(|e| e.to_string())?;
        let mut encoder = GifEncoder::new_with_speed(file, 10);
        encoder.set_repeat(Repeat::Infinite).map_err(|e| e.to_string())?;
        let cropped = frames.iter().map(|f| Frame::from_parts(crop.apply(f.buffer()), 0, 0, f.delay()));
        encoder.encode_frames(cropped).map_err(|e| e.to_string())?;
        return Ok(json!({ "imagePath": output.to_string_lossy() }));
    }
    let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?.to_rgba8();
    let crop = Crop::new(params, image.width(), image.height());
    let output = temp_output("profile-crop.webp");
    crop.apply(&image).save_with_format(&output, ImageFormat::WebP).map_err(|e| e.to_string())?;
    Ok(json!({ "imagePath": output.to_string_lossy() }))
}

/// uploadImage: keeps the picked image under `local-profile/` and returns its `local:` URL.
fn keep_image(dir: &Path, kind: &str, source: &str) -> Result<String, String> {
    let bytes = fs::read(source).map_err(|e| e.to_string())?;
    let format = image::guess_format(&bytes).map_err(|_| "profile/not-an-image")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // A new name each time, so the renderer does not show a cached old picture.
    let target = dir.join(format!("{kind}-{}.{}", now_ms(), format.extensions_str()[0]));
    fs::write(&target, bytes).map_err(|e| e.to_string())?;
    Ok(format!("local:{}", target.display()))
}

/// `updateProfile`.
pub async fn update_profile(app: &AppHandle, request: &Value) -> Result<Value, String> {
    let mut payload = request.as_object().cloned().unwrap_or_default();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("local-profile");
    for (field, kind) in [("profileImageUrl", "profile-image"), ("backgroundImageUrl", "background-image")] {
        if let Some(Value::String(source)) = payload.get(field).cloned() {
            // Electron drops a failed copy (undefined), so the old picture stays.
            match keep_image(&dir, kind, &source) {
                Ok(url) => payload.insert(field.into(), json!(url)),
                Err(_) => payload.remove(field),
            };
        }
    }
    let version = app.package_info().version.to_string();
    let call = json!({ "method": "patch", "url": "/profile", "data": payload });
    let updated = hydra_api::call(&app.state::<Store>(), &version, &call).await?;
    let _ = app.emit("on-profile-updated", ());
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gif(frames: usize) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut out);
            let frame = || Frame::new(RgbaImage::from_pixel(4, 2, image::Rgba([255, 0, 0, 255])));
            encoder.encode_frames((0..frames).map(|_| frame())).unwrap();
        }
        out
    }

    #[test]
    fn detects_animation_from_headers() {
        assert_eq!(gif_animated(&gif(1)), Some(false));
        assert_eq!(gif_animated(&gif(3)), Some(true));
        let mut png = Vec::new();
        RgbaImage::new(2, 2).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        assert_eq!(png_animated(&png), Some(false));
        assert_eq!(mime_of(&png), Some("image/png"));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\x02\0\0\0\0\0\0\0\0\0".to_vec();
        assert_eq!(webp_animated(&webp), Some(true));
        webp[20] = 0;
        assert_eq!(webp_animated(&webp), Some(false));
        assert!(is_animated(Some("image/gif"), b"GIF89a"));
        assert!(!is_animated(Some("image/jpeg"), b""));
    }

    #[test]
    fn crop_clamps_in_rotated_space() {
        let c = Crop::new(&json!({ "left": -5, "top": 3.6, "width": 999, "height": 2, "outputWidth": 8, "outputHeight": 8, "rotation": -90 }), 10, 20);
        assert_eq!((c.rotation, c.left, c.top, c.width, c.height), (270, 0, 4, 20, 2));
        let src = RgbaImage::new(10, 20);
        assert_eq!(c.apply(&src).dimensions(), (8, 8));
    }

    #[test]
    fn crops_static_and_animated_images() {
        let dir = std::env::temp_dir().join(format!("chimaera-profile-test-{}", now_ms()));
        fs::create_dir_all(&dir).unwrap();
        let params = json!({ "left": 0, "top": 0, "width": 2, "height": 2, "outputWidth": 6, "outputHeight": 6, "rotation": 90 });

        let animated = dir.join("a.gif");
        fs::write(&animated, gif(3)).unwrap();
        let out = crop_profile_image(animated.to_str().unwrap(), &params).unwrap();
        let out = out["imagePath"].as_str().unwrap().to_string();
        let frames = decode_frames(Some("image/gif"), &fs::read(&out).unwrap()).unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].buffer().dimensions(), (6, 6));
        fs::remove_file(out).unwrap();

        let still = dir.join("s.png");
        RgbaImage::new(4, 2).save(&still).unwrap();
        let out = crop_profile_image(still.to_str().unwrap(), &params).unwrap();
        let out = out["imagePath"].as_str().unwrap().to_string();
        assert_eq!(image::open(&out).unwrap().width(), 6);
        assert_eq!(image_metadata(&out)["mimeType"], "image/webp");
        fs::remove_file(out).unwrap();

        let kept = keep_image(&dir.join("local-profile"), "profile-image", still.to_str().unwrap()).unwrap();
        assert!(kept.starts_with("local:") && kept.ends_with(".png"));
        assert!(keep_image(&dir, "x", animated.with_extension("missing").to_str().unwrap()).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
