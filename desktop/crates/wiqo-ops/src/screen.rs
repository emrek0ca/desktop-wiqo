//! A picture of the screen, small enough to travel: scaled to the width asked for and JPEG-compressed until it fits.

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage};
use serde::Deserialize;
use serde_json::{json, Value};
use wiqo_core::engine::{Context, Lane, Operation, Outcome};

#[cfg(target_os = "macos")]
use crate::op::permission;
use crate::op::{data, outcome, payload, tool_failure, Done, Result};
use crate::Op;

const MAX_BYTES: usize = 700 * 1024;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(CaptureScreen)]
}

/// `ScreenCapturePayload`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capture {
    #[serde(default)]
    display_id: Option<u32>,
    #[serde(default = "default_width")]
    max_width: u32,
    #[serde(default)]
    target_application: Option<String>,
    /// Also save the picture here ("~/Desktop", a folder or a file path).
    #[serde(default)]
    destination: Option<String>,
}

fn default_width() -> u32 {
    1600
}

struct CaptureScreen;

#[async_trait]
impl Operation for CaptureScreen {
    fn name(&self) -> &'static str {
        "captureScreen"
    }

    fn lane(&self) -> Lane {
        Lane::ReadOnly
    }

    async fn run(&self, _ctx: &Context, value: &Value) -> Outcome {
        outcome(capture(value).await)
    }
}

async fn capture(value: &Value) -> Result<Done> {
    let request: Capture = payload(value, "screenCapture", "ekran")?;
    if !(100..=4000).contains(&request.max_width) {
        return Err(tool_failure("INVALID_PAYLOAD", "Ekran ayarları geçersiz."));
    }
    ensure_permission()?;
    if let Some(target) = request
        .target_application
        .as_deref()
        .map(str::trim)
        .filter(|target| !target.is_empty())
    {
        crate::apps::activate_verified(target).await?;
    }
    let (width_limit, display) = (request.max_width, request.display_id);
    let destination = request
        .destination
        .clone()
        .filter(|text| !text.trim().is_empty());
    tokio::task::spawn_blocking(move || {
        let picture = grab(display)?;
        let saved = destination
            .as_deref()
            .map(|target| save(&picture, target))
            .transpose()?;
        let mut done = encode(picture, width_limit)?;
        if let Some(path) = saved {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            let folder = path
                .parent()
                .map(crate::fsx::abbreviate)
                .unwrap_or_default();
            done.message = format!("Ekran görüntüsü {folder} klasörüne kaydedildi: {name}");
        }
        Ok(done)
    })
    .await
    .map_err(|_| tool_failure("SCREEN_CAPTURE_FAILED", "Ekran görüntüsü alınamadı."))?
}

/// Saves the full-size picture as PNG: into a folder under a dated name (a taken name gets " 2"), or at the file path given.
fn save(picture: &DynamicImage, target: &str) -> Result<std::path::PathBuf> {
    let invalid = || {
        tool_failure(
            "INVALID_PATH",
            "Ekran görüntüsünün kaydedileceği yer bulunamadı.",
        )
    };
    let resolved = crate::fsx::resolve(target, true).ok_or_else(invalid)?;
    let path = if crate::fsx::is_dir(&resolved) {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H.%M.%S");
        crate::utility::free_path(&resolved.join(format!("Ekran görüntüsü {stamp}.png")))
    } else {
        crate::utility::free_path(&resolved.with_extension("png"))
    };
    if wiqo_contract::FilePolicy::get().is_sensitive(&path.to_string_lossy()) {
        return Err(crate::fsx::sensitive_blocked("dosyalar"));
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    picture
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|_| tool_failure("SCREEN_CAPTURE_FAILED", "Ekran görüntüsü kaydedilemedi."))?;
    crate::files::write_atomic(&path, bytes.get_ref()).map_err(|source| {
        crate::fsx::local_file_error(
            "FILE_WRITE_FAILED",
            "Ekran görüntüsü kaydedilemedi.",
            &source,
        )
    })?;
    Ok(path)
}

/// The screenshot as JPEG bytes that fit the transfer limit, with its final size.
fn encode(picture: DynamicImage, max_width: u32) -> Result<Done> {
    let resized = if picture.width() > max_width {
        picture.resize(max_width, u32::MAX, FilterType::Triangle)
    } else {
        picture
    };
    let rgb = DynamicImage::ImageRgb8(resized.to_rgb8());
    let mut jpeg: Option<Vec<u8>> = None;
    for quality in [72u8, 58, 42] {
        let mut bytes = Vec::new();
        if JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode_image(&rgb)
            .is_err()
        {
            return Err(tool_failure(
                "SCREEN_CAPTURE_FAILED",
                "Ekran görüntüsü hazırlanamadı.",
            ));
        }
        if bytes.len() <= MAX_BYTES {
            jpeg = Some(bytes);
            break;
        }
    }
    let bytes = jpeg.ok_or_else(|| {
        tool_failure(
            "SCREEN_TOO_LARGE",
            "Ekran görüntüsü güvenli aktarım sınırına sığmadı.",
        )
    })?;
    let content = json!({
        "filename": format!("rova-screen-{}.jpg", uuid::Uuid::new_v4()),
        "mimeType": "image/jpeg",
        "dataBase64": base64::engine::general_purpose::STANDARD.encode(&bytes),
        "width": rgb.width(),
        "height": rgb.height(),
    });
    Ok(Done::new(
        "Ekran görüntüsü hazır.",
        Some(data("screenshot", content)),
    ))
}

#[cfg(target_os = "macos")]
fn ensure_permission() -> Result<()> {
    // Without the Screen Recording permission the system hands back only the wallpaper, so the permission is checked first.
    let allowed = unsafe {
        libloading::Library::new("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics")
            .ok()
            .and_then(|library| {
                library
                    .get::<unsafe extern "C" fn() -> bool>(b"CGPreflightScreenCaptureAccess\0")
                    .ok()
                    .map(|check| check())
            })
            .unwrap_or(false)
    };
    if allowed {
        Ok(())
    } else {
        Err(permission("SCREEN_RECORDING_PERMISSION_REQUIRED", "Ekran görüntüsü için Ekran Kaydı izni gerekiyor: bilgisayarda izin penceresi açıldı; izin verip Wiqo'yu yeniden başlatınca tekrar dene."))
    }
}

#[cfg(not(target_os = "macos"))]
fn ensure_permission() -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn grab(display: Option<u32>) -> Result<DynamicImage> {
    let file = std::env::temp_dir().join(format!("wiqo-screen-{}.png", uuid::Uuid::new_v4()));
    let mut command = crate::process::command("/usr/sbin/screencapture");
    command.args(["-x", "-t", "png"]);
    if let Some(display) = display {
        command.arg(format!("-D{display}"));
    }
    command.arg(&file);
    let done = crate::process::run_with_timeout(command, std::time::Duration::from_secs(10))
        .is_some_and(|output| output.status.success());
    let picture = if done { image::open(&file).ok() } else { None };
    let _ = std::fs::remove_file(&file);
    picture.ok_or_else(|| {
        permission(
            "SCREEN_CAPTURE_FAILED",
            "Ekran görüntüsü alınamadı. Ekran Kaydı iznini kontrol edin.",
        )
    })
}

#[cfg(not(target_os = "macos"))]
fn grab(display: Option<u32>) -> Result<DynamicImage> {
    let screens = xcap::Monitor::all()
        .map_err(|_| tool_failure("SCREEN_CAPTURE_FAILED", "Ekran görüntüsü alınamadı."))?;
    let screen = display
        .and_then(|id| screens.iter().find(|screen| screen.id().ok() == Some(id)))
        .or_else(|| screens.first())
        .ok_or_else(|| tool_failure("DISPLAY_NOT_FOUND", "Ekran bulunamadı."))?;
    let captured = screen
        .capture_image()
        .map_err(|_| tool_failure("SCREEN_CAPTURE_FAILED", "Ekran görüntüsü alınamadı."))?;
    Ok(DynamicImage::ImageRgba8(captured))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_big_picture_is_scaled_to_the_width_and_fits_the_limit() {
        let picture = DynamicImage::ImageRgb8(image::RgbImage::from_fn(3200, 1800, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        }));
        let done = encode(picture, 1600).unwrap();
        let value = &done.data.unwrap()["value"];
        assert_eq!(value["width"], 1600);
        assert_eq!(value["height"], 900);
        assert_eq!(value["mimeType"], "image/jpeg");
        assert!(value["dataBase64"].as_str().unwrap().len() < MAX_BYTES * 2);
    }
}
