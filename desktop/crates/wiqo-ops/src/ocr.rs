//! Text on screen, read by the operating system's own recogniser (Apple Vision on macOS, Windows.Media.Ocr on Windows): the controls the accessibility tree does not expose
//! (games, canvases, remote-desktop windows, some Electron apps) still have words on them, and a word with its place is something to click.
//! Free and offline; on Windows the system's own Windows.Media.Ocr does the same. Other systems answer with nothing for now.

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
    // Fast, not Accurate: Accurate took 46 s on a whole screen, and every action waited for it.
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Fast);
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

/// Windows 10 and 11 carry their own recogniser (Windows.Media.Ocr), reached here through PowerShell's WinRT bridge: no install, offline, in the
/// languages of the person's Windows. It prints the picture's size and then one word per line with its box in pixels.
#[cfg(windows)]
pub(crate) fn read(png: &std::path::Path) -> Vec<Word> {
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Runtime.WindowsRuntime
$null = [Windows.Storage.StorageFile, Windows.Storage, ContentType = WindowsRuntime]
$null = [Windows.Media.Ocr.OcrEngine, Windows.Foundation, ContentType = WindowsRuntime]
$null = [Windows.Graphics.Imaging.BitmapDecoder, Windows.Graphics, ContentType = WindowsRuntime]
$asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' })[0]
function Await($operation, [Type]$type) { $task = $asTask.MakeGenericMethod($type).Invoke($null, @($operation)); $null = $task.Wait(-1); $task.Result }
$file = Await ([Windows.Storage.StorageFile]::GetFileFromPathAsync($env:WIQO_OCR_FILE)) ([Windows.Storage.StorageFile])
$stream = Await ($file.OpenAsync([Windows.Storage.FileAccessMode]::Read)) ([Windows.Storage.Streams.IRandomAccessStream])
$decoder = Await ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder])
$bitmap = Await ($decoder.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap])
$engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages()
if ($null -eq $engine) { exit 3 }
$result = Await ($engine.RecognizeAsync($bitmap)) ([Windows.Media.Ocr.OcrResult])
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
"{0}`t{1}" -f $bitmap.PixelWidth, $bitmap.PixelHeight
foreach ($line in $result.Lines) { foreach ($word in $line.Words) { $r = $word.BoundingRect; "{0}`t{1}`t{2}`t{3}`t{4}" -f $r.X, $r.Y, $r.Width, $r.Height, $word.Text } }
"#;
    let mut command = crate::process::command("powershell");
    command.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", SCRIPT]);
    command.env("WIQO_OCR_FILE", png);
    let Some(out) = crate::process::run_with_timeout(command, std::time::Duration::from_secs(20)) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    parse_windows(&String::from_utf8_lossy(&out.stdout))
}

/// The recogniser's lines: the picture's size first, then `x y width height word` in pixels, turned into fractions of the picture.
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_windows(text: &str) -> Vec<Word> {
    let mut lines = text.lines();
    let Some((width, height)) = lines.next().and_then(|first| {
        let mut size = first.trim().split('\t').map(|part| part.parse::<f64>().ok());
        Some((size.next()??, size.next()??))
    }) else {
        return Vec::new();
    };
    if width <= 0.0 || height <= 0.0 {
        return Vec::new();
    }
    lines
        .filter_map(|line| {
            let mut parts = line.splitn(5, '\t');
            let mut number = || parts.next()?.trim().replace(',', ".").parse::<f64>().ok();
            let (x, y, w, h) = (number()?, number()?, number()?, number()?);
            let text = parts.next()?.trim();
            (!text.is_empty()).then(|| Word {
                text: text.chars().take(100).collect(),
                frame: [x / width, y / height, w / width, h / height],
            })
        })
        .collect()
}

#[cfg(not(any(target_os = "macos", windows)))]
pub(crate) fn read(_png: &std::path::Path) -> Vec<Word> {
    Vec::new()
}

/// The words as text in reading order: words on about the same height make one line, left to right; lines go top to bottom.
pub(crate) fn as_text(words: &[Word]) -> String {
    let mut sorted: Vec<&Word> = words.iter().collect();
    sorted.sort_by(|a, b| a.frame[1].total_cmp(&b.frame[1]));
    let mut lines: Vec<Vec<&Word>> = Vec::new();
    for word in sorted {
        let middle = word.frame[1] + word.frame[3] / 2.0;
        match lines.last_mut() {
            Some(line)
                if (line[0].frame[1] + line[0].frame[3] / 2.0 - middle).abs()
                    < line[0].frame[3].max(0.004) / 2.0 =>
            {
                line.push(word)
            }
            _ => lines.push(vec![word]),
        }
    }
    lines
        .into_iter()
        .map(|mut line| {
            line.sort_by(|a, b| a.frame[0].total_cmp(&b.frame[0]));
            line.iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod reading_order {
    use super::*;

    #[test]
    fn windows_words_become_fractions_of_the_picture() {
        let words = parse_windows("1000\t500\n100\t50\t80\t20\tKaydet\n0\t0\t10\t10\t \n");
        assert_eq!(words, vec![Word { text: "Kaydet".into(), frame: [0.1, 0.1, 0.08, 0.04] }]);
        assert!(parse_windows("").is_empty());
    }

    #[test]
    fn words_read_line_by_line_left_to_right() {
        let word = |text: &str, x: f64, y: f64| Word {
            text: text.into(),
            frame: [x, y, 0.05, 0.02],
        };
        let words = [
            word("dünya", 0.3, 0.101),
            word("Kaydet", 0.1, 0.5),
            word("Merhaba", 0.1, 0.1),
        ];
        assert_eq!(as_text(&words), "Merhaba dünya\nKaydet");
        assert_eq!(as_text(&[]), "");
    }
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
