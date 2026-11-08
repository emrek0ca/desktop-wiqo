//! Pictures: a smaller copy of an image.

use super::*;

// ----- images -----

pub(super) fn compress_image(request: &Request) -> Result<Done> {
    let source = existing(&request.path)?;
    let edge: u32 = request
        .mode
        .as_deref()
        .and_then(|mode| mode.parse().ok())
        .unwrap_or(1600u32)
        .clamp(200, 8000);
    let parent = source.parent().map(Path::to_path_buf).unwrap_or_default();
    let output = target(
        &request.destination,
        &parent.join(format!("{}-küçük.jpg", stem_of(&source))),
    )?;
    let failed = || {
        fail(
            "COMPRESS_FAILED",
            "Görsel küçültülemedi; bir görsel dosyası mı?",
        )
    };
    let encoded = image::open(&source).ok().and_then(|picture| {
        let picture = if picture.width().max(picture.height()) > edge {
            picture.resize(edge, edge, image::imageops::FilterType::Lanczos3)
        } else {
            picture
        };
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 75)
            .encode_image(&picture.to_rgb8())
            .ok()?;
        Some(bytes)
    });
    match encoded {
        Some(bytes) => crate::files::write_atomic(&output, &bytes).map_err(|_| failed())?,
        // HEIC and the other formats the image library cannot read: the system's own converter.
        None if cfg!(target_os = "macos") => {
            let edge = edge.to_string();
            let out = run(
                "/usr/bin/sips",
                &[
                    "-Z",
                    &edge,
                    "-s",
                    "format",
                    "jpeg",
                    "-s",
                    "formatOptions",
                    "75",
                    &source.to_string_lossy(),
                    "--out",
                    &output.to_string_lossy(),
                ],
                60,
            );
            if !succeeded(&out) || !output.exists() {
                return Err(failed());
            }
        }
        None => return Err(failed()),
    }
    Ok(Done::new(
        format!(
            "{}: {} → {}.",
            name_of(&output),
            human(size(&source)),
            human(size(&output))
        ),
        Some(listing(&[output])),
    ))
}
