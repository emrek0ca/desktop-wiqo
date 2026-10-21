//! `executeCode`: a Python, JavaScript or shell snippet in an isolated scratch folder. On macOS the sandbox denies everything but reading the system
//! and the toolchains and writing the scratch folder (network only when asked); work on the person's own files goes through terminal sessions instead.

use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};
use wiqo_core::engine::{Context, Lane, Operation, Outcome};
use wiqo_core::wire::WireError;

use super::shell::{self, Control};
use crate::op::{error, payload, permission, tool_failure};
use crate::Op;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(ExecuteCode)]
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Request {
    language: String,
    code: String,
    arguments: Vec<String>,
    timeout_seconds: u64,
    allow_network: bool,
    scope: String,
}

impl Default for Request {
    fn default() -> Self {
        Request {
            language: String::new(),
            code: String::new(),
            arguments: Vec::new(),
            timeout_seconds: 30,
            allow_network: false,
            scope: "sandbox".into(),
        }
    }
}

struct ExecuteCode;

#[async_trait::async_trait]
impl Operation for ExecuteCode {
    fn name(&self) -> &'static str {
        "executeCode"
    }

    fn lane(&self) -> Lane {
        Lane::SideEffect
    }

    async fn run(&self, _ctx: &Context, value: &Value) -> Outcome {
        let value = value.clone();
        match tokio::task::spawn_blocking(move || execute(&value)).await {
            Ok((data, error)) => Outcome {
                message: error
                    .as_ref()
                    .map(|error| error.message.clone())
                    .unwrap_or_else(|| "Kod çalıştı.".into()),
                data,
                error,
            },
            Err(_) => Outcome::failed(tool_failure(
                "OUTCOME_UNKNOWN",
                "Kod beklenmedik şekilde durdu.",
            )),
        }
    }
}

fn quoted(path: &Path) -> String {
    format!(
        "\"{}\"",
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

/// Reads only the system, the toolchains and the run's own folder; writes only that folder.
fn sandbox_profile(directory: &Path, network: bool) -> String {
    let folder = quoted(directory);
    format!(
        "(version 1) (deny default) (allow process*) (allow sysctl-read) (allow mach-lookup) (allow iokit-open) (allow signal) (allow file-read-metadata) \
         (allow file-read-data (literal \"/\") (subpath \"/System\") (subpath \"/Library\") (subpath \"/Applications\") (subpath \"/usr\") (subpath \"/bin\") \
         (subpath \"/opt/homebrew\") (subpath \"/usr/local\") (subpath \"/private/etc\") (subpath \"/private/var/db\") (subpath {folder})) \
         (allow file-write* (subpath {folder})) {}",
        if network { "(allow network*)" } else { "(deny network*)" }
    )
}

fn execute(value: &Value) -> (Option<Value>, Option<WireError>) {
    let request: Request = match payload(value, "codeExecution", "kod") {
        Ok(request) => request,
        Err(error) => return (None, Some(error)),
    };
    if request.scope.eq_ignore_ascii_case("computer") {
        return (None, Some(permission("TERMINAL_GRANT_REQUIRED", "Bu bilgisayarda terminal komutu için onaylı klasör izni gerekiyor. Görevi terminal aracıyla yeniden isteyin.")));
    }
    let (extension, interpreter) = match request.language.to_lowercase().as_str() {
        "python" => ("py", "python3"),
        "javascript" => ("js", "node"),
        "shell" => ("sh", "/bin/sh"),
        _ => {
            return (
                None,
                Some(tool_failure(
                    "UNSUPPORTED_LANGUAGE",
                    "Yalnızca Python, JavaScript veya shell çalıştırılabilir.",
                )),
            )
        }
    };
    if request.code.len() > 200_000 {
        return (
            None,
            Some(error("INVALID_PAYLOAD", "Kod çok uzun.", "validation")),
        );
    }
    let scratch = std::env::temp_dir().join(format!("rova-code-{}", uuid::Uuid::new_v4()));
    if std::fs::create_dir_all(&scratch).is_err() {
        return (
            None,
            Some(tool_failure(
                "CODE_SANDBOX_FAILED",
                "Kod için geçici klasör açılamadı.",
            )),
        );
    }
    // The sandbox matches the filesystem's real path (/private/var/folders, not /var/folders).
    let directory = dunce::canonicalize(&scratch).unwrap_or(scratch.clone());
    let script = directory.join(format!("main.{extension}"));
    if std::fs::write(&script, &request.code).is_err() {
        let _ = std::fs::remove_dir_all(&scratch);
        return (
            None,
            Some(tool_failure(
                "CODE_SANDBOX_FAILED",
                "Kod izole ortamda başlatılamadı.",
            )),
        );
    }
    let seconds = request.timeout_seconds.clamp(1, 110);
    let mut argv: Vec<String> = if interpreter.starts_with('/') {
        vec![interpreter.into()]
    } else {
        vec!["/usr/bin/env".into(), interpreter.into()]
    };
    argv.push(script.to_string_lossy().to_string());
    argv.extend(request.arguments.iter().cloned());
    let mut command = if cfg!(target_os = "macos") {
        let mut sandboxed = crate::process::command("/usr/bin/sandbox-exec");
        // The CPU-time limit is applied by a fixed wrapper that execs the separated argv: the code never becomes part of this shell text.
        sandboxed
            .args([
                "-p",
                &sandbox_profile(&directory, request.allow_network),
                "/bin/sh",
                "-c",
                &format!("ulimit -t {seconds}; exec \"$@\""),
                "rova-code",
            ])
            .args(&argv);
        sandboxed
    } else {
        let mut plain = crate::process::command(&argv[0]);
        plain.args(&argv[1..]);
        plain
    };
    let path = shell::interactive_path()
        .unwrap_or_else(|| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into());
    command
        .current_dir(&directory)
        .env_clear()
        .envs([("PATH", path.as_str()), ("LANG", "en_US.UTF-8")])
        .env("HOME", super::policy::home())
        .env("TMPDIR", format!("{}/", directory.to_string_lossy()))
        .env("PWD", &directory);
    let snapshot = shell::run(command, seconds, &Control::default());
    let _ = std::fs::remove_dir_all(&scratch);
    let output = snapshot.output(&request.language);
    let data = Some(json!({ "kind": "processOutput", "value": output }));
    if snapshot.timed_out {
        return (
            data,
            Some(WireError::new(
                "CODE_TIMEOUT",
                "Kod çalıştırma süresi doldu.",
                "timeout",
            )),
        );
    }
    if snapshot.exit_code != 0 {
        let detail: String = snapshot
            .stderr
            .lines()
            .filter(|line| !line.starts_with("shell-init: error retrieving current directory"))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .chars()
            .take(300)
            .collect();
        let message = if detail.is_empty() {
            format!("Kod hata ile sonlandı (çıkış kodu {}).", snapshot.exit_code)
        } else {
            format!("Kod hata ile sonlandı: {detail}")
        };
        return (data, Some(tool_failure("CODE_FAILED", message)));
    }
    (data, None)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn code_runs_in_its_sandbox_and_cannot_write_outside() {
        let run = |code: &str| {
            execute(
                &json!({ "kind": "codeExecution", "value": { "language": "shell", "code": code, "timeoutSeconds": 10 } }),
            )
        };
        let (data, error) = run("echo $((6 * 7)); echo ok > inside.txt && cat inside.txt");
        assert!(error.is_none(), "{error:?}");
        assert_eq!(data.unwrap()["value"]["stdout"], "42\nok\n");
        let target =
            crate::fsx::home().join(format!("wiqo-sandbox-escape-{}", uuid::Uuid::new_v4()));
        let (_, error) = run(&format!("echo x > '{}'", target.display()));
        assert_eq!(error.unwrap().code, "CODE_FAILED");
        assert!(!target.exists());
    }
}
