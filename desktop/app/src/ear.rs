//! The on-device ear: speech recognition by whisper.cpp (Metal on Apple Silicon), so the person's voice is never sent anywhere and nothing is paid for.
//! The model is a one-time download made on the first launch (resumable, checked against the hash the host publishes); until it is there the window
//! falls back to the Mac's built-in recognition.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};

// Apple Silicon runs the large turbo model on its GPU; elsewhere the smaller model keeps dictation quick on an ordinary processor.
#[cfg(target_os = "macos")]
const MODEL_NAME: &str = "ggml-large-v3-turbo-q5_0.bin";
#[cfg(not(target_os = "macos"))]
const MODEL_NAME: &str = "ggml-small-q5_1.bin";
#[cfg(target_os = "macos")]
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin";
#[cfg(not(target_os = "macos"))]
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    /// missing, downloading, ready or failed
    pub state: String,
    pub progress: f64,
    pub message: Option<String>,
}

pub struct Ear {
    dir: PathBuf,
    status: Mutex<ModelStatus>,
    #[cfg(any(
        target_os = "macos",
        all(target_os = "windows", target_arch = "x86_64")
    ))]
    engine: Mutex<Option<whisper_rs::WhisperState>>,
    downloading: Mutex<bool>,
}

impl Ear {
    pub fn new(dir: PathBuf) -> Arc<Ear> {
        let ready = dir.join(MODEL_NAME).exists();
        Arc::new(Ear {
            dir,
            status: Mutex::new(ModelStatus {
                state: if ready { "ready" } else { "missing" }.into(),
                progress: if ready { 1.0 } else { 0.0 },
                message: None,
            }),
            #[cfg(any(
                target_os = "macos",
                all(target_os = "windows", target_arch = "x86_64")
            ))]
            engine: Mutex::new(None),
            downloading: Mutex::new(false),
        })
    }

    pub fn status(&self) -> ModelStatus {
        self.status.lock().expect("ear lock").clone()
    }

    fn set(&self, handle: &AppHandle, state: &str, progress: f64, message: Option<String>) {
        let status = ModelStatus {
            state: state.into(),
            progress,
            message,
        };
        *self.status.lock().expect("ear lock") = status.clone();
        let _ = handle.emit("voice-model", status);
    }

    /// Fetches the model once, in the background; a second call while it runs does nothing. Resumes a partial file.
    pub fn ensure_model(self: &Arc<Self>, handle: AppHandle) {
        if self.dir.join(MODEL_NAME).exists() {
            return;
        }
        {
            let mut running = self.downloading.lock().expect("ear lock");
            if *running {
                return;
            }
            *running = true;
        }
        let ear = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = ear.download(&handle).await;
            *ear.downloading.lock().expect("ear lock") = false;
            match result {
                Ok(()) => {
                    ear.set(&handle, "ready", 1.0, None);
                    ear.warm();
                }
                Err(message) => ear.set(&handle, "failed", 0.0, Some(message)),
            }
        });
    }

    async fn download(&self, handle: &AppHandle) -> Result<(), String> {
        use std::io::Write as _;
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        let partial = self.dir.join(format!("{MODEL_NAME}.part"));
        let client = reqwest::Client::builder()
            .build()
            .map_err(|error| error.to_string())?;
        // The host publishes the file's SHA-256 as the LFS etag: what is downloaded is checked against it.
        let head = client
            .head(MODEL_URL)
            .send()
            .await
            .map_err(|_| "Ses paketi sunucusuna ulaşılamadı.".to_string())?;
        let expected = head
            .headers()
            .get("x-linked-etag")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim_matches('"').to_string());
        let have = std::fs::metadata(&partial)
            .map(|meta| meta.len())
            .unwrap_or(0);
        let mut request = client.get(MODEL_URL);
        if have > 0 {
            request = request.header("Range", format!("bytes={have}-"));
        }
        let response = request
            .send()
            .await
            .map_err(|_| "Ses paketi indirilemedi; internet bağlantını kontrol et.".to_string())?;
        let resumed = response.status().as_u16() == 206;
        if !response.status().is_success() {
            return Err("Ses paketi indirilemedi.".into());
        }
        let total = response.content_length().unwrap_or(0) + if resumed { have } else { 0 };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&partial)
            .map_err(|error| error.to_string())?;
        let mut done = if resumed { have } else { 0 };
        let mut last = 0.0;
        self.set(handle, "downloading", 0.0, None);
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| {
                "İndirme yarıda kesildi; uygulamayı yeniden açınca devam eder.".to_string()
            })?;
            file.write_all(&chunk).map_err(|error| error.to_string())?;
            done += chunk.len() as u64;
            if total > 0 {
                let progress = done as f64 / total as f64;
                if progress - last >= 0.01 {
                    last = progress;
                    self.set(handle, "downloading", progress.min(0.99), None);
                }
            }
        }
        drop(file);
        if let Some(expected) = expected.filter(|value| value.len() == 64) {
            let bytes = std::fs::read(&partial).map_err(|error| error.to_string())?;
            if hex::encode(Sha256::digest(&bytes)) != expected {
                let _ = std::fs::remove_file(&partial);
                return Err("Ses paketi doğrulanamadı; yeniden denenecek.".into());
            }
        }
        std::fs::rename(&partial, self.dir.join(MODEL_NAME)).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// The words in 16 kHz mono samples. The model is loaded once and kept (with its working memory), so only the first call pays for the load.
    #[cfg(any(
        target_os = "macos",
        all(target_os = "windows", target_arch = "x86_64")
    ))]
    pub fn transcribe(&self, samples: &[f32], language: &str) -> Result<String, String> {
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
        let path = self.dir.join(MODEL_NAME);
        if !path.exists() {
            return Err("unsupported".into());
        }
        let mut slot = self.engine.lock().expect("ear lock");
        if slot.is_none() {
            let mut options = WhisperContextParameters::default();
            options.flash_attn(true);
            let context = WhisperContext::new_with_params(&path.to_string_lossy(), options)
                .map_err(|error| format!("Ses paketi yüklenemedi: {error}"))?;
            *slot = Some(context.create_state().map_err(|error| error.to_string())?);
        }
        let state = slot.as_mut().expect("loaded");
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(if language.is_empty() { "tr" } else { language }));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_no_timestamps(true);
        params.set_single_segment(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_no_speech_thold(0.6);
        params.set_n_threads(4);
        // The encoder looks at 30 seconds by default; a dictated phrase is a few, so it only looks at what was said (plus a margin).
        let seconds = samples.len() as f32 / 16_000.0;
        params.set_audio_ctx((((seconds * 50.0) * 1.25) as i32 + 64).clamp(256, 1500));
        params.set_initial_prompt("Wiqo, masaüstü, dosya, klasör.");
        state
            .full(params, samples)
            .map_err(|error| format!("Ses tanıma çalışmadı: {error}"))?;
        let count = state.full_n_segments().map_err(|error| error.to_string())?;
        let mut text = String::new();
        for index in 0..count {
            text.push_str(
                &state
                    .full_get_segment_text_lossy(index)
                    .map_err(|error| error.to_string())?,
            );
        }
        Ok(fix_names(text.trim()))
    }

    /// Loads the model and runs it once on silence, so the first real dictation is as quick as the next ones.
    pub fn warm(self: &Arc<Self>) {
        if self.status().state != "ready" {
            return;
        }
        let ear = self.clone();
        std::thread::spawn(move || {
            let _ = ear.transcribe(&[0.0; 16_000], "tr");
        });
    }

    #[cfg(not(any(
        target_os = "macos",
        all(target_os = "windows", target_arch = "x86_64")
    )))]
    pub fn transcribe(&self, _samples: &[f32], _language: &str) -> Result<String, String> {
        Err("unsupported".into())
    }
}

/// The product's own name is the one word a general model keeps mishearing.
fn fix_names(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let core = word.trim_matches(|c: char| !c.is_alphanumeric());
            if matches!(
                core.to_lowercase().as_str(),
                "iko" | "viko" | "wiko" | "vico" | "wico" | "viqo" | "uiko" | "vıko"
            ) {
                word.replacen(core, "Wiqo", 1)
            } else {
                word.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real recording through the real model: `WIQO_TEST_WAV=/path/16k-mono.wav cargo test -p wiqo-desktop dictation -- --ignored --nocapture`.
    #[test]
    #[ignore]
    #[allow(clippy::chunks_exact_to_as_chunks)]
    fn dictation_reads_a_recording() {
        let Ok(path) = std::env::var("WIQO_TEST_WAV") else {
            return;
        };
        let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
            .join("Library/Application Support/Wiqo Desktop/voice");
        let ear = Ear::new(dir);
        let wav = std::fs::read(path).unwrap();
        let samples: Vec<f32> = wav[44..]
            .chunks_exact(2)
            .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
            .collect();
        let started = std::time::Instant::now();
        let first = ear.transcribe(&samples, "tr").unwrap();
        let loaded = started.elapsed();
        let again = std::time::Instant::now();
        let second = ear.transcribe(&samples, "tr").unwrap();
        println!(
            "first ({:?} with loading): {first}\nsecond ({:?}): {second}",
            loaded,
            again.elapsed()
        );
        assert!(!first.is_empty());
    }
}
