//! Files between the computer and the phone: `saveArtifact` downloads a file the server made onto the Desktop, `sendFileToPhone` uploads one of the person's files
//! so the phone can show it. Both talk to the paired server with the desktop's own token, and both check size and SHA-256 before they report success.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use wiqo_contract::{is_valid_file_name, FilePolicy};
use wiqo_core::engine::{Context, Lane, Operation, Outcome};

use crate::fsx::{self, is_regular_file, is_symlink};
use crate::op::{data, error, outcome, payload, permission, tool_failure, Done, Result};
use crate::{undo, Op};

const MAX_ARTIFACT_BYTES: u64 = 25 * 1024 * 1024;
const MAX_PHONE_TRANSFER_BYTES: u64 = 25 * 1024 * 1024;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(SaveArtifact), Arc::new(SendFileToPhone)]
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn server_token(ctx: &Context) -> Result<(String, String)> {
    let credentials = ctx
        .services
        .credentials
        .get()
        .filter(|credentials| !credentials.access_token.is_empty());
    credentials
        .map(|credentials| (credentials.api_base, credentials.access_token))
        .ok_or_else(|| {
            tool_failure(
                "BACKEND_AUTH_REQUIRED",
                "Sunucu oturumu bulunamadı; bu bilgisayarı yeniden eşleştir.",
            )
        })
}

fn client(seconds: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(seconds))
        .user_agent(concat!("wiqo-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

// ----- saveArtifact -----

/// `ArtifactFilePayload`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Artifact {
    filename: String,
    download_url: String,
    #[serde(default)]
    overwrite: bool,
    #[serde(default)]
    byte_count: Option<u64>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    destination_directory: Option<String>,
}

/// The download address must be the paired server's own artifact path, so a model-made URL cannot turn this into a general downloader.
fn is_artifact_url(raw: &str, api_base: &str) -> bool {
    let (Ok(url), Ok(base)) = (url::Url::parse(raw), url::Url::parse(api_base)) else {
        return false;
    };
    if url.scheme() != base.scheme()
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
    {
        return false;
    }
    url.path()
        .strip_prefix("/copilot/artifacts/")
        .is_some_and(|id| {
            (20..=128).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        })
}

fn valid_directory(directory: &Option<String>) -> bool {
    match directory.as_deref() {
        None | Some("") => true,
        Some(path) => {
            path.chars().count() <= 500
                && !path.starts_with('~')
                && path
                    .split('/')
                    .all(|part| is_valid_file_name(part) && part != "." && part != "..")
        }
    }
}

struct SaveArtifact;

#[async_trait]
impl Operation for SaveArtifact {
    fn name(&self) -> &'static str {
        "saveArtifact"
    }

    async fn run(&self, ctx: &Context, value: &Value) -> Outcome {
        outcome(save_artifact(ctx, value).await)
    }
}

async fn save_artifact(ctx: &Context, value: &Value) -> Result<Done> {
    let artifact: Artifact = payload(value, "artifactFile", "artifact")?;
    let (api_base, token) = server_token(ctx)?;
    if !is_artifact_url(&artifact.download_url, &api_base) {
        return Err(tool_failure(
            "INVALID_ARTIFACT_URL",
            "Artifact indirme adresi güvenli değil.",
        ));
    }
    let response = client(45)
        .get(&artifact.download_url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| tool_failure("ARTIFACT_DOWNLOAD_FAILED", "Artifact indirilemedi."))?;
    if !response.status().is_success() {
        return Err(tool_failure(
            "ARTIFACT_DOWNLOAD_FAILED",
            "Artifact backend'den indirilemedi.",
        ));
    }
    if response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|kind| kind.to_lowercase().starts_with("application/json"))
    {
        return Err(tool_failure(
            "ARTIFACT_DOWNLOAD_FAILED",
            "Artifact backend'den geçerli bir dosya döndürmedi.",
        ));
    }
    let too_large = || {
        tool_failure(
            "ARTIFACT_TOO_LARGE",
            "İndirilen dosya desteklenen boyut sınırını aşıyor.",
        )
    };
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARTIFACT_BYTES)
    {
        return Err(too_large());
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| tool_failure("ARTIFACT_DOWNLOAD_FAILED", "Artifact indirilemedi."))?
    {
        if (bytes.len() + chunk.len()) as u64 > MAX_ARTIFACT_BYTES {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    let integrity = || {
        tool_failure(
            "ARTIFACT_INTEGRITY_FAILED",
            "İndirilen artifact bütünlük doğrulamasından geçmedi.",
        )
    };
    if artifact
        .byte_count
        .is_some_and(|expected| expected != bytes.len() as u64)
    {
        return Err(tool_failure(
            "ARTIFACT_INTEGRITY_FAILED",
            "İndirilen artifact boyut doğrulamasından geçmedi.",
        ));
    }
    if artifact
        .sha256
        .as_deref()
        .is_some_and(|expected| expected.to_lowercase() != sha(&bytes))
    {
        return Err(integrity());
    }
    let (ctx, filename, overwrite, directory) = (
        ctx.clone(),
        artifact.filename,
        artifact.overwrite,
        artifact.destination_directory,
    );
    tokio::task::spawn_blocking(move || {
        commit_artifact(&ctx, &bytes, &filename, overwrite, &directory)
    })
    .await
    .map_err(|_| {
        error(
            "OUTCOME_UNKNOWN",
            "İşlem beklenmedik şekilde durdu; sonucu bilinmiyor.",
            "tool_failure",
        )
    })?
}

/// Writes the file onto the Desktop (inside a sub-folder when one is named), never through a link, never over a file unless asked, and reads it back.
fn commit_artifact(
    ctx: &Context,
    bytes: &[u8],
    filename: &str,
    overwrite: bool,
    directory: &Option<String>,
) -> Result<Done> {
    let invalid = || {
        tool_failure(
            "INVALID_ARTIFACT_TARGET",
            "Dosya veya hedef klasör geçersiz.",
        )
    };
    if bytes.is_empty()
        || bytes.len() as u64 > MAX_ARTIFACT_BYTES
        || !is_valid_file_name(filename)
        || !valid_directory(directory)
    {
        return Err(invalid());
    }
    let desktop = fsx::desktop_dir()
        .ok_or_else(|| tool_failure("DESKTOP_UNAVAILABLE", "Masaüstüne erişilemedi."))?;
    let mut folder = dunce::canonicalize(&desktop)
        .map_err(|_| tool_failure("DESKTOP_UNAVAILABLE", "Masaüstüne erişilemedi."))?;
    for part in directory
        .as_deref()
        .unwrap_or("")
        .split('/')
        .filter(|part| !part.is_empty())
    {
        folder.push(part);
        match fs::symlink_metadata(&folder) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err(tool_failure(
                    "UNSAFE_ARTIFACT_TARGET",
                    "Hedef klasör bir bağlantı veya erişilemeyen bir konum.",
                ))
            }
            Ok(_) => {}
            Err(_) => fs::create_dir(&folder).map_err(|_| {
                tool_failure("DIRECTORY_CREATE_FAILED", "Hedef klasör oluşturulamadı.")
            })?,
        }
    }
    let target = folder.join(filename.trim());
    let existed = fs::symlink_metadata(&target).is_ok();
    if existed {
        if !overwrite {
            return Err(permission(
                "FILE_EXISTS",
                "Aynı adlı dosya zaten var; üzerine yazılmadı.",
            ));
        }
        if is_symlink(&target) || !is_regular_file(&target) {
            return Err(tool_failure(
                "UNSAFE_ARTIFACT_TARGET",
                "Hedef normal bir dosya değil.",
            ));
        }
        crate::files::back_up(ctx, &target).map_err(|_| {
            tool_failure(
                "BACKUP_FAILED",
                "Yedek saklanamadı; işlem tamamlandı sayılmadı.",
            )
        })?;
    }
    crate::files::write_atomic(&target, bytes)
        .map_err(|_| tool_failure("FILE_WRITE_FAILED", "Dosya bütünüyle yazılamadı."))?;
    if fs::read(&target)
        .map(|written| written != bytes)
        .unwrap_or(true)
    {
        return Err(tool_failure(
            "FILE_VERIFICATION_FAILED",
            "Kaydedilen içerik doğrulanamadı.",
        ));
    }
    let mut item = fsx::entry(&target).ok_or_else(|| {
        tool_failure(
            "FILE_VERIFICATION_FAILED",
            "Dosya tekrar okunamadı; otomatik tekrar yapılmadı.",
        )
    })?;
    if !existed {
        item.undo_id = undo::register(ctx, &target.to_string_lossy(), None, None);
    }
    let message = format!(
        "Doğrulandı: {} · {} bayt · SHA-256: {}",
        item.path,
        bytes.len(),
        sha(bytes)
    );
    Ok(Done::new(message, Some(data("files", vec![item]))))
}

// ----- sendFileToPhone -----

#[derive(Deserialize)]
struct SendRequest {
    path: String,
}

struct SendFileToPhone;

#[async_trait]
impl Operation for SendFileToPhone {
    fn name(&self) -> &'static str {
        "sendFileToPhone"
    }

    fn lane(&self) -> Lane {
        Lane::ReadOnly
    }

    async fn run(&self, ctx: &Context, value: &Value) -> Outcome {
        outcome(send_file(ctx, value).await)
    }
}

fn prepare_send(path: &str) -> Result<(PathBuf, Vec<u8>)> {
    let policy = FilePolicy::get();
    let blocked = || {
        permission(
            "SENSITIVE_FILE_BLOCKED",
            "Kimlik bilgisi içerebilecek dosyalar Wiqo tarafından aktarılamaz.",
        )
    };
    if policy.is_sensitive(path) {
        return Err(blocked());
    }
    let file = fsx::resolve(path, false)
        .filter(|file| file.exists())
        .ok_or_else(|| fsx::missing_path_error(path, "Gönderilecek dosya bulunamadı."))?;
    if policy.is_sensitive(&file.to_string_lossy()) {
        return Err(blocked());
    }
    if !is_regular_file(&file) || is_symlink(&file) {
        return Err(tool_failure(
            "INVALID_FILE",
            "Yalnızca normal dosyalar gönderilebilir; klasör veya sembolik bağlantı seçilemez.",
        ));
    }
    let size = fs::metadata(&file)
        .map_err(|_| {
            tool_failure(
                "FILE_READ_FAILED",
                "Dosya bilgisi okunamadı. Dosyaya erişim iznini kontrol et.",
            )
        })?
        .len();
    if size == 0 {
        return Err(tool_failure(
            "INVALID_FILE",
            "Dosya boş; aktarılacak içerik bulunamadı.",
        ));
    }
    if size > MAX_PHONE_TRANSFER_BYTES {
        return Err(tool_failure(
            "FILE_TOO_LARGE",
            "Yalnızca 25 MB veya daha küçük normal dosyalar gönderilebilir.",
        ));
    }
    let bytes = fs::read(&file).map_err(|source| {
        fsx::local_file_error(
            "FILE_READ_FAILED",
            "Dosya bilgisi okunamadı. Dosyaya erişim iznini kontrol et.",
            &source,
        )
    })?;
    Ok((file, bytes))
}

pub(crate) async fn send_file(ctx: &Context, value: &Value) -> Result<Done> {
    let request: SendRequest = payload(value, "fileOperation", "dosya")?;
    let (api_base, token) = server_token(ctx)
        .map_err(|_| tool_failure("TRANSFER_UNAVAILABLE", "PC aktarım bağlantısı hazır değil."))?;
    let path = request.path;
    let (file, bytes) = tokio::task::spawn_blocking(move || prepare_send(&path))
        .await
        .map_err(|_| {
            error(
                "OUTCOME_UNKNOWN",
                "İşlem beklenmedik şekilde durdu; sonucu bilinmiyor.",
                "tool_failure",
            )
        })??;
    let digest = sha(&bytes);
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "dosya".into());
    let encoded: String = name
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    let failed = || tool_failure("TRANSFER_FAILED", "Dosya telefona iletilemedi.");
    let size = bytes.len();
    let response = client(120)
        .put(format!(
            "{}/file-transfers/{}",
            api_base.trim_end_matches('/'),
            ctx.command_id
        ))
        .bearer_auth(token)
        .header("Content-Type", "application/octet-stream")
        .header("X-File-Name", encoded)
        .header("X-File-SHA256", &digest)
        .body(bytes)
        .send()
        .await
        .map_err(|_| failed())?;
    if response.status().as_u16() != 201 {
        return Err(failed());
    }
    let body: Value = response.json().await.map_err(|_| failed())?;
    let artifact_id = body["artifactId"].as_str().ok_or_else(failed)?.to_string();
    if body["byteCount"].as_u64() != Some(size as u64)
        || body["sha256"].as_str() != Some(digest.as_str())
    {
        return Err(tool_failure(
            "TRANSFER_UNVERIFIED",
            "Aktarılan dosyanın boyutu veya bütünlüğü doğrulanamadı.",
        ));
    }
    Ok(Done::new(
        "Dosya telefona gönderildi.",
        Some(json!({ "kind": "fileTransfer", "value": artifact_id })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_paired_servers_artifact_paths_may_be_downloaded() {
        let base = "https://api.wiqoai.com";
        let id = "0b8e7d2a-1111-4c3b-8d9e-aaaaaaaaaaaa";
        assert!(is_artifact_url(
            &format!("https://api.wiqoai.com/copilot/artifacts/{id}"),
            base
        ));
        assert!(!is_artifact_url(
            &format!("https://evil.test/copilot/artifacts/{id}"),
            base
        ));
        assert!(!is_artifact_url(
            &format!("http://api.wiqoai.com/copilot/artifacts/{id}"),
            base
        ));
        assert!(!is_artifact_url(
            &format!("https://api.wiqoai.com/copilot/artifacts/{id}?x=1"),
            base
        ));
        assert!(!is_artifact_url(
            "https://api.wiqoai.com/copilot/artifacts/kisa",
            base
        ));
        assert!(!is_artifact_url(
            &format!("https://api.wiqoai.com/other/{id}"),
            base
        ));
    }

    #[test]
    fn an_artifact_lands_in_its_folder_never_over_a_file_unless_asked_and_with_a_backup() {
        let ctx = Context::for_tests();
        let desktop = fsx::desktop_dir().unwrap();
        // A fresh CI machine has no Desktop folder yet.
        fs::create_dir_all(&desktop).unwrap();
        let folder = format!("wiqo-artifact-test-{}", uuid::Uuid::new_v4());
        let directory = Some(folder.clone());
        let done = commit_artifact(&ctx, b"bir", "rapor.txt", false, &directory).unwrap();
        let file = desktop.join(&folder).join("rapor.txt");
        assert_eq!(fs::read(&file).unwrap(), b"bir");
        assert!(done.message.contains("SHA-256"));
        assert_eq!(
            commit_artifact(&ctx, b"iki", "rapor.txt", false, &directory)
                .unwrap_err()
                .code,
            "FILE_EXISTS"
        );
        commit_artifact(&ctx, b"iki", "rapor.txt", true, &directory).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"iki");
        assert_eq!(
            commit_artifact(&ctx, b"x", "../kacis.txt", false, &None)
                .unwrap_err()
                .code,
            "INVALID_ARTIFACT_TARGET"
        );
        assert_eq!(
            commit_artifact(&ctx, b"x", "a.txt", false, &Some("../disari".into()))
                .unwrap_err()
                .code,
            "INVALID_ARTIFACT_TARGET"
        );
        assert_eq!(
            commit_artifact(&ctx, b"", "a.txt", false, &None)
                .unwrap_err()
                .code,
            "INVALID_ARTIFACT_TARGET"
        );
        let _ = fs::remove_dir_all(desktop.join(&folder));
    }

    #[test]
    fn a_file_for_the_phone_must_be_a_real_small_non_sensitive_file() {
        let dir = std::env::temp_dir().join(format!("wiqo-send-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let dir = dunce::canonicalize(dir).unwrap();
        fs::write(dir.join("rapor.pdf"), "x").unwrap();
        fs::write(dir.join("bos.txt"), "").unwrap();
        fs::write(dir.join("id_rsa"), "x").unwrap();
        assert!(prepare_send(&dir.join("rapor.pdf").to_string_lossy()).is_ok());
        assert_eq!(
            prepare_send(&dir.join("bos.txt").to_string_lossy())
                .unwrap_err()
                .code,
            "INVALID_FILE"
        );
        assert_eq!(
            prepare_send(&dir.join("id_rsa").to_string_lossy())
                .unwrap_err()
                .code,
            "SENSITIVE_FILE_BLOCKED"
        );
        assert_eq!(
            prepare_send(&dir.to_string_lossy()).unwrap_err().code,
            "INVALID_FILE"
        );
        assert_eq!(
            prepare_send(&dir.join("yok.pdf").to_string_lossy())
                .unwrap_err()
                .code,
            "FILE_NOT_FOUND"
        );
    }
}
