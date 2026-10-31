//! Wiqo's own visible browser (`computerBrowser`, `endBrowserTask`): the Playwright worker in `desktop/browser-worker` (the same one the Mac app
//! ran), spoken to in JSON lines over its stdin and stdout. Each task gets a fresh browser context; no personal profile or cookie is touched.
//! The worker installs its pinned Playwright and Chromium on first use (Node.js 20 or newer must be on the computer).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use wiqo_core::engine::{Context, Lane, Operation, Outcome};
use wiqo_core::wire::WireError;

use crate::op::{data, outcome, payload, tool_failure, Done, Result};
use crate::Op;

/// First use may install Playwright and Chromium (the worker bounds that at 105 s).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(115);

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(Browse), Arc::new(EndTask)]
}

struct Worker {
    child: Child,
    input: ChildStdin,
    pending: Arc<Mutex<HashMap<String, Sender<Value>>>>,
}

fn worker() -> &'static Mutex<Option<Worker>> {
    static WORKER: OnceLock<Mutex<Option<Worker>>> = OnceLock::new();
    WORKER.get_or_init(Default::default)
}

/// `WIQO_BROWSER_WORKER`, else the copy shipped with Wiqo (macOS: Contents/Resources/browser-worker, others: next to the executable).
fn worker_script() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    std::env::var_os("WIQO_BROWSER_WORKER")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            dir.join("../Resources/browser-worker/index.mjs"),
            dir.join("browser-worker/index.mjs"),
        ])
        .find(|path| path.is_file())
}

fn node() -> Option<PathBuf> {
    let name = if cfg!(windows) { "node.exe" } else { "node" };
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::var_os("WIQO_NODE_EXECUTABLE")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::split_paths(&path).map(|dir| dir.join(name)))
        .chain(
            [
                "/opt/homebrew/bin/node",
                "/usr/local/bin/node",
                "/usr/bin/node",
            ]
            .map(PathBuf::from),
        )
        .find(|candidate| candidate.is_file())
}

fn start() -> Result<Worker> {
    let script = worker_script().ok_or_else(|| {
        tool_failure(
            "BROWSER_RUNTIME_MISSING",
            "Tarayıcı bileşeni bu Wiqo sürümünde yok.",
        )
    })?;
    let node = node().ok_or_else(|| {
        tool_failure(
            "BROWSER_RUNTIME_MISSING",
            "Tarayıcı için bilgisayarda Node.js 20 veya üzeri gerekiyor (nodejs.org).",
        )
    })?;
    let support = dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("Wiqo Desktop")
        .join("browser-worker");
    let _ = std::fs::create_dir_all(&support);
    let mut child = Command::new(node)
        .arg(&script)
        .env("WIQO_BROWSER_WORKER_PACKAGE_ROOT", &support)
        .env("PLAYWRIGHT_BROWSERS_PATH", support.join(".browsers"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            tool_failure(
                "BROWSER_RUNTIME_MISSING",
                "Tarayıcı bileşeni başlatılamadı.",
            )
        })?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| tool_failure("BROWSER_FAILED", "Tarayıcı bileşeni başlatılamadı."))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| tool_failure("BROWSER_FAILED", "Tarayıcı bileşeni başlatılamadı."))?;
    let pending: Arc<Mutex<HashMap<String, Sender<Value>>>> = Arc::default();
    let readers = Arc::clone(&pending);
    std::thread::spawn(move || {
        for line in BufReader::new(output)
            .lines()
            .map_while(std::result::Result::ok)
        {
            let Ok(envelope) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(id) = envelope["id"].as_str() else {
                continue;
            };
            if let Some(sender) = readers.lock().expect("pending lock").remove(id) {
                let _ = sender.send(envelope);
            }
        }
        // The worker is gone: every waiting request hears it now instead of at its timeout.
        readers.lock().expect("pending lock").clear();
    });
    Ok(Worker {
        child,
        input,
        pending,
    })
}

/// One request to the worker and its answer envelope (`{id, ok, result?, error?}`).
fn ask(mut message: Value) -> Result<Value> {
    let id = uuid::Uuid::new_v4().to_string();
    message["id"] = json!(id);
    let (sender, answer) = channel();
    {
        let mut slot = worker()
            .lock()
            .map_err(|_| tool_failure("BROWSER_FAILED", "Tarayıcı bileşeni kullanılamıyor."))?;
        let alive = slot
            .as_mut()
            .is_some_and(|worker| matches!(worker.child.try_wait(), Ok(None)));
        if !alive {
            *slot = Some(start()?);
        }
        let worker = slot.as_mut().expect("worker started");
        worker
            .pending
            .lock()
            .expect("pending lock")
            .insert(id.clone(), sender);
        let line = format!("{message}\n");
        if worker
            .input
            .write_all(line.as_bytes())
            .and_then(|()| worker.input.flush())
            .is_err()
        {
            *slot = None;
            return Err(tool_failure(
                "BROWSER_FAILED",
                "Tarayıcı bileşeni beklenmedik biçimde durdu.",
            ));
        }
    }
    answer.recv_timeout(REQUEST_TIMEOUT).map_err(|_| {
        WireError::new(
            "BROWSER_TIMEOUT",
            "Tarayıcı işlemi zamanında bitmedi; sonucu doğrulanamadı.",
            "timeout",
        )
    })
}

fn worker_error(error: &Value) -> WireError {
    let code = error["code"].as_str().unwrap_or("BROWSER_FAILED");
    let message = error["message"]
        .as_str()
        .unwrap_or("Tarayıcı işlemi tamamlanamadı.");
    let category = if code.contains("TIMEOUT") {
        "timeout"
    } else if code.contains("BLOCKED") || code.contains("UNSAFE") {
        "permission"
    } else {
        "tool_failure"
    };
    WireError::new(code, message, category)
}

struct Browse;

#[async_trait]
impl Operation for Browse {
    fn name(&self) -> &'static str {
        "computerBrowser"
    }

    fn lane(&self) -> Lane {
        Lane::SideEffect
    }

    async fn run(&self, _ctx: &Context, value: &Value) -> Outcome {
        let value = value.clone();
        let result = tokio::task::spawn_blocking(move || browse(&value)).await;
        outcome(result.unwrap_or_else(|_| {
            Err(tool_failure(
                "BROWSER_FAILED",
                "Tarayıcı işlemi tamamlanamadı.",
            ))
        }))
    }
}

/// The payload goes to the worker as it is (the worker validates every field); its result is the contract's `BrowserContent`.
fn browse(value: &Value) -> Result<Done> {
    let request: Value = payload(value, "computerBrowser", "tarayıcı")?;
    if !request["taskId"].is_string() || !request["action"].is_string() {
        return Err(tool_failure("INVALID_PAYLOAD", "Tarayıcı işlemi geçersiz."));
    }
    let envelope = ask(request)?;
    let result = &envelope["result"];
    let has_result = result.is_object() && result["taskId"].is_string();
    match (
        envelope["ok"].as_bool() == Some(true),
        has_result,
        envelope["error"].is_object(),
    ) {
        (_, true, true) => Ok(Done::partial(
            worker_error(&envelope["error"]),
            data("computerBrowser", result),
        )),
        (true, true, false) => Ok(Done::new(
            "Tarayıcı işlemi tamamlandı ve sonucu doğrulandı.",
            Some(data("computerBrowser", result)),
        )),
        (_, _, true) => Err(worker_error(&envelope["error"])),
        _ => Err(tool_failure(
            "BROWSER_FAILED",
            "Tarayıcıdan güvenli bir sonuç alınamadı.",
        )),
    }
}

struct EndTask;

#[async_trait]
impl Operation for EndTask {
    fn name(&self) -> &'static str {
        "endBrowserTask"
    }

    async fn run(&self, _ctx: &Context, value: &Value) -> Outcome {
        let value = value.clone();
        let result = tokio::task::spawn_blocking(move || end_task(&value)).await;
        outcome(
            result
                .unwrap_or_else(|_| Err(tool_failure("BROWSER_FAILED", "Tarayıcı kapatılamadı."))),
        )
    }
}

fn end_task(value: &Value) -> Result<Done> {
    let request: Value = payload(value, "browserTask", "tarayıcı görevi")?;
    let task = request["taskId"]
        .as_str()
        .ok_or_else(|| tool_failure("INVALID_PAYLOAD", "Tarayıcı görevi geçersiz."))?;
    let envelope = ask(json!({ "action": "closeBrowser", "taskId": task }))?;
    if envelope["ok"].as_bool() != Some(true) {
        return Err(worker_error(&envelope["error"]));
    }
    Ok(Done::new(
        "Göreve özel görünür tarayıcı kapatıldı.",
        Some(data(
            "browserTask",
            json!({ "taskId": task, "closed": envelope["result"]["closed"].as_bool().unwrap_or(true) }),
        )),
    ))
}

#[cfg(test)]
mod tests {
    /// The real worker opening a page: `WIQO_BROWSER_WORKER=<desktop/browser-worker/index.mjs> cargo test -p wiqo-ops live_browser -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_browser_opens_and_reads_a_page() {
        if std::env::var_os("WIQO_BROWSER_WORKER").is_none() {
            return;
        }
        let task = uuid::Uuid::new_v4().to_string();
        let open = serde_json::json!({ "kind": "computerBrowser", "value": { "taskId": task, "action": "navigate", "url": "https://example.com" } });
        let done = super::browse(&open).expect("navigate");
        println!("{} {}", done.message, done.data.unwrap_or_default());
        let _ = super::end_task(
            &serde_json::json!({ "kind": "browserTask", "value": { "taskId": task } }),
        );
    }
}
