//! `getProcessedImage`: a remote image downloaded, cover-cropped to the size the UI draws it at
//! and cached on disk (animated ones as GIF, others as WebP). Ported from
//! src/main/events/misc/get-processed-image.ts and processImage in native/hydra-native.

use std::fs::File;
use std::io::{BufWriter, Cursor};
use std::path::{Path, PathBuf};

use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::imageops::{crop_imm, resize, FilterType};
use image::{AnimationDecoder, Frame, ImageFormat, ImageReader, RgbaImage};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use crate::data_location::DataDir;

const MAX_DIMENSION: u64 = 2048;
const CACHE_VERSION: u32 = 2;

struct Options {
    width: u32,
    height: u32,
    preserve_animation: bool,
}

fn options(value: &Value) -> Option<Options> {
    let size = |key: &str| value[key].as_u64().filter(|n| (1..=MAX_DIMENSION).contains(n)).map(|n| n as u32);
    Some(Options { width: size("width")?, height: size("height")?, preserve_animation: value["preserveAnimation"].as_bool().unwrap_or(true) })
}

/// The same key as Electron (sha256 of the JSON it builds), so the names stay stable.
fn cache_key(url: &str, o: &Options) -> String {
    // Written out by hand: serde_json sorts object keys, JSON.stringify keeps insertion order.
    let text = format!(
        r#"{{"imageUrl":{},"version":{CACHE_VERSION},"width":{},"height":{},"preserveAnimation":{}}}"#,
        Value::from(url),
        o.width,
        o.height,
        o.preserve_animation
    );
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Returns the `local:` URL of the processed copy, or the input unchanged when it cannot be processed.
pub async fn get(app: &AppHandle, url: Option<&str>, raw_options: &Value) -> Value {
    let Some(url) = url else { return Value::Null };
    let Some(o) = options(raw_options).filter(|_| url.starts_with("http://") || url.starts_with("https://")) else { return json!(url) };
    match process(app, url, o).await {
        Ok(path) => json!(format!("local:{}", path.display())),
        Err(error) => {
            eprintln!("[images] could not process {url}: {error}");
            json!(url)
        }
    }
}

async fn process(app: &AppHandle, url: &str, o: Options) -> Result<PathBuf, String> {
    let dir = app.path().chimaera_data_dir().map_err(|e| e.to_string())?.join("image-cache").join("processed");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let base = dir.join(cache_key(url, &o));
    for ext in ["gif", "webp"] {
        let cached = base.with_extension(ext);
        if cached.exists() {
            return Ok(cached);
        }
    }
    let response = reqwest::get(url).await.and_then(|r| r.error_for_status()).map_err(|e| e.to_string())?;
    let content_type = response.headers().get("content-type").and_then(|v| v.to_str().ok()).map(|v| v.split(';').next().unwrap_or_default().to_string());
    if let Some(kind) = content_type.filter(|k| !k.starts_with("image/")) {
        return Err(format!("unexpected content type {kind}"));
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?.to_vec();
    // Two concurrent requests for one image write separate temp files; the last rename wins.
    let temp = dir.join(format!("{}-tmp", uuid::Uuid::new_v4()));
    let base_final = base.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let (made, ext) = convert(&bytes, &temp, &o)?;
        let target = base_final.with_extension(ext);
        std::fs::rename(&made, &target).map_err(|e| e.to_string())?;
        Ok(target)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Writes `<temp>.gif` for an animated input (when kept), else `<temp>.webp`.
fn convert(bytes: &[u8], temp: &Path, o: &Options) -> Result<(PathBuf, &'static str), String> {
    let format = image::guess_format(bytes).ok();
    if o.preserve_animation && is_animated(bytes, format) {
        let out = temp.with_extension("gif");
        let result = resize_animation(bytes, format, &out, o.width, o.height);
        if result.is_err() {
            let _ = std::fs::remove_file(&out);
        }
        return result.map(|_| (out, "gif"));
    }
    let image = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?.decode().map_err(|e| e.to_string())?.to_rgba8();
    let out = temp.with_extension("webp");
    cover(&image, o.width, o.height)?.save_with_format(&out, ImageFormat::WebP).map_err(|e| e.to_string())?;
    Ok((out, "webp"))
}

fn is_animated(bytes: &[u8], format: Option<ImageFormat>) -> bool {
    match format {
        Some(ImageFormat::Gif) => GifDecoder::new(Cursor::new(bytes)).is_ok_and(|d| d.into_frames().take(2).filter(Result::is_ok).count() == 2),
        Some(ImageFormat::WebP) => WebPDecoder::new(Cursor::new(bytes)).is_ok_and(|d| d.has_animation()),
        Some(ImageFormat::Png) => PngDecoder::new(Cursor::new(bytes)).ok().and_then(|d| d.is_apng().ok()).unwrap_or(false),
        _ => false,
    }
}

fn resize_animation(bytes: &[u8], format: Option<ImageFormat>, out: &Path, width: u32, height: u32) -> Result<(), String> {
    let frames: Vec<image::ImageResult<Frame>> = match format {
        Some(ImageFormat::Gif) => GifDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?.into_frames().collect(),
        Some(ImageFormat::WebP) => WebPDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?.into_frames().collect(),
        Some(ImageFormat::Png) => PngDecoder::new(Cursor::new(bytes)).map_err(|e| e.to_string())?.apng().map_err(|e| e.to_string())?.into_frames().collect(),
        _ => return Err("unsupported animated format".into()),
    };
    let mut encoder = GifEncoder::new(BufWriter::new(File::create(out).map_err(|e| e.to_string())?));
    encoder.set_repeat(Repeat::Infinite).map_err(|e| e.to_string())?;
    for frame in frames {
        let frame = frame.map_err(|e| e.to_string())?;
        let delay = frame.delay();
        let resized = cover(&frame.into_buffer(), width, height)?;
        encoder.encode_frame(Frame::from_parts(resized, 0, 0, delay)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// resize_cover_rgba: scale to cover the box, then crop the centre.
fn cover(image: &RgbaImage, width: u32, height: u32) -> Result<RgbaImage, String> {
    let (sw, sh) = image.dimensions();
    if sw == 0 || sh == 0 {
        return Err("could not read source image dimensions".into());
    }
    let scale = (width as f32 / sw as f32).max(height as f32 / sh as f32);
    let rw = ((sw as f32 * scale).ceil() as u32).max(width);
    let rh = ((sh as f32 * scale).ceil() as u32).max(height);
    let resized = resize(image, rw, rh, FilterType::Lanczos3);
    Ok(crop_imm(&resized, (rw - width) / 2, (rh - height) / 2, width, height).to_image())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn rejects_bad_sizes_and_matches_electrons_key() {
        assert!(options(&json!({ "width": 0, "height": 10 })).is_none());
        assert!(options(&json!({ "width": 4096, "height": 10 })).is_none());
        let o = options(&json!({ "width": 64, "height": 64 })).unwrap();
        assert!(o.preserve_animation);
        // sha256 of {"imageUrl":"https://x/a.png","version":2,"width":64,"height":64,"preserveAnimation":true}
        let text = r#"{"imageUrl":"https://x/a.png","version":2,"width":64,"height":64,"preserveAnimation":true}"#;
        let expected: String = Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(cache_key("https://x/a.png", &o), expected);
    }

    #[test]
    fn covers_and_converts_static_and_animated() {
        let dir = std::env::temp_dir().join(format!("chimaera-processed-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let o = Options { width: 32, height: 16, preserve_animation: true };

        let mut png = Vec::new();
        RgbaImage::from_pixel(100, 100, image::Rgba([255, 0, 0, 255])).write_to(&mut Cursor::new(&mut png), ImageFormat::Png).unwrap();
        let (out, ext) = convert(&png, &dir.join("a"), &o).unwrap();
        assert_eq!(ext, "webp");
        assert_eq!(image::open(&out).unwrap().to_rgba8().dimensions(), (32, 16));

        let mut gif = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut gif);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for shade in [0u8, 255] {
                encoder.encode_frame(Frame::new(RgbaImage::from_pixel(40, 40, image::Rgba([shade, shade, shade, 255])))).unwrap();
            }
        }
        let (out, ext) = convert(&gif, &dir.join("b"), &o).unwrap();
        assert_eq!(ext, "gif");
        let frames = GifDecoder::new(BufReader::new(File::open(&out).unwrap())).unwrap().into_frames().count();
        assert_eq!(frames, 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
