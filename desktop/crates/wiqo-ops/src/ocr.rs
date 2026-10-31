//! Text on screen, read by the operating system's own recogniser (Apple Vision on macOS): the controls the accessibility tree does not expose
//! (games, canvases, remote-desktop windows, some Electron apps) still have words on them, and a word with its place is something to click.
//! Free and offline. Other systems answer with nothing for now.

/// One piece of text and where it is, as fractions of the picture (0-1, origin top-left).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Word {
    pub text: String,
    pub frame: [f64; 4],
}

#[cfg(target_os = "macos")]
pub(crate) fn read(png: &std::path::Path) -> Vec<Word> {
    use objc2::AnyThread;
    use objc2_foundation::{NSArray, NSDictionary, NSString, NSURL};
    use objc2_vision::{
        VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
    };

    let Some(url) = NSURL::from_file_path(png) else {
        return Vec::new();
    };
    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setUsesLanguageCorrection(true);
    request.setRecognitionLanguages(&NSArray::from_retained_slice(&[
        NSString::from_str("tr-TR"),
        NSString::from_str("en-US"),
    ]));
    let handler = unsafe {
        VNImageRequestHandler::initWithURL_options(
            VNImageRequestHandler::alloc(),
            &url,
            &NSDictionary::new(),
        )
    };
    let requests: objc2::rc::Retained<NSArray<VNRequest>> =
        NSArray::from_slice(&[request.as_ref()]);
    if handler.performRequests_error(&requests).is_err() {
        return Vec::new();
    }
    let Some(results) = request.results() else {
        return Vec::new();
    };
    results
        .iter()
        .filter_map(|observation| {
            let best = observation.topCandidates(1).firstObject()?;
            let text = best.string().to_string();
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            // Vision's box is normalised with its origin at the bottom left.
            let rect = unsafe { observation.boundingBox() };
            let (x, y, w, h) = (
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                rect.size.height,
            );
            Some(Word {
                text: text.chars().take(100).collect(),
                frame: [x, 1.0 - y - h, w, h],
            })
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn read(_png: &std::path::Path) -> Vec<Word> {
    Vec::new()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    /// `WIQO_OCR_SAMPLE=<png with "Kaydet" top-left> cargo test -p wiqo-ops ocr -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn reads_words_with_their_place() {
        let Some(path) = std::env::var_os("WIQO_OCR_SAMPLE") else {
            return;
        };
        let words = super::read(std::path::Path::new(&path));
        println!("{words:?}");
        let save = words
            .iter()
            .find(|word| word.text.contains("Kaydet"))
            .expect("Kaydet read");
        assert!(
            save.frame[0] < 0.3 && save.frame[1] < 0.5,
            "top-left: {:?}",
            save.frame
        );
    }
}
