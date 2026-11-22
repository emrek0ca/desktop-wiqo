//! `browser_check`: open a page in a real (headless) browser, use it, and say what went wrong.
//!
//! A syntax check says a script parses; it does not say the page works. A person who has written a web page opens it, looks at the console, clicks what they built
//! and sees whether the result is right. The agent had no browser: a site whose script threw on load was handed over as "verified" because a server answered
//! 200 for its title. This opens the page in the Chrome or Edge that is installed (headless, a throwaway profile), through the DevTools protocol, and reports
//! the errors the console and the network showed, answers any `alert`/`prompt` the page raises (and tells what they said), and runs the steps the model gives
//! (click, type, wait, expect text, evaluate): the result is exact, short and the same on every run. Nothing else on this computer is touched.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_tungstenite::tungstenite::Message;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const LOAD_WAIT: Duration = Duration::from_secs(20);
const MOST_STEPS: usize = 25;
const MOST_LISTED: usize = 8;

struct Cdp {
    socket: Socket,
    next: u64,
    /// Events that arrived while a reply was awaited, in order.
    events: Vec<Value>,
}

impl Cdp {
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        let text = json!({ "id": id, "method": method, "params": params }).to_string();
        self.socket
            .send(Message::text(text))
            .await
            .map_err(|error| format!("tarayıcıya yazılamadı: {error}"))?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let message = tokio::time::timeout(left, self.socket.next())
                .await
                .map_err(|_| format!("{method} yanıtsız kaldı"))?;
            let Some(Ok(Message::Text(text))) = message else {
                return Err("tarayıcı bağlantısı koptu".into());
            };
            let Ok(value) = serde_json::from_str::<Value>(text.as_str()) else {
                continue;
            };
            if value["id"] == id {
                return if value["error"].is_null() {
                    Ok(value["result"].clone())
                } else {
                    Err(format!(
                        "{method}: {}",
                        value["error"]["message"].as_str().unwrap_or("hata")
                    ))
                };
            }
            self.keep(value).await;
        }
    }

    /// A dialog (alert, confirm, prompt) blocks the page until it is answered: it is answered at once, and what it said is kept.
    async fn keep(&mut self, event: Value) {
        if event["method"] == "Page.javascriptDialogOpening" {
            self.next += 1;
            let reply = json!({ "id": self.next, "method": "Page.handleJavaScriptDialog", "params": { "accept": true, "promptText": event["params"]["defaultPrompt"].as_str().unwrap_or("1") } });
            let _ = self.socket.send(Message::text(reply.to_string())).await;
        }
        self.events.push(event);
    }

    /// Reads events for `duration`, answering dialogs as they come.
    async fn settle(&mut self, duration: Duration) {
        let end = Instant::now() + duration;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            match tokio::time::timeout(left, self.socket.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(value) = serde_json::from_str::<Value>(text.as_str()) {
                        if value["id"].is_null() {
                            self.keep(value).await;
                        }
                    }
                }
                Ok(Some(Ok(_))) => {}
                _ => return,
            }
        }
    }

    async fn eval(&mut self, expression: &str) -> Result<Value, String> {
        let reply = self
            .call(
                "Runtime.evaluate",
                json!({ "expression": expression, "returnByValue": true, "awaitPromise": true }),
            )
            .await?;
        if reply["exceptionDetails"].is_object() {
            return Err(reply["exceptionDetails"]["exception"]["description"]
                .as_str()
                .or(reply["exceptionDetails"]["text"].as_str())
                .unwrap_or("betik hatası")
                .lines()
                .next()
                .unwrap_or("")
                .to_string());
        }
        Ok(reply["result"]["value"].clone())
    }
}

fn js(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// One step of the page's use: what it did and whether it held.
async fn run_step(cdp: &mut Cdp, step: &Value) -> (String, bool, String) {
    if let Some(selector) = step["click"].as_str() {
        let result = cdp.eval(&format!("(() => {{ const e = document.querySelector({}); if (!e) return 'no element'; e.click(); return 'ok'; }})()", js(selector))).await;
        return match result {
            Ok(value) if value == "ok" => (format!("click {selector}"), true, String::new()),
            Ok(value) => (
                format!("click {selector}"),
                false,
                value.as_str().unwrap_or("olmadı").to_string(),
            ),
            Err(error) => (format!("click {selector}"), false, error),
        };
    }
    if let Some(selector) = step["type"].as_str() {
        let text = step["text"].as_str().unwrap_or("");
        let result = cdp
            .eval(&format!("(() => {{ const e = document.querySelector({}); if (!e) return 'no element'; e.focus(); e.value = {}; e.dispatchEvent(new Event('input', {{ bubbles: true }})); e.dispatchEvent(new Event('change', {{ bubbles: true }})); return 'ok'; }})()", js(selector), js(text)))
            .await;
        return match result {
            Ok(value) if value == "ok" => (format!("type {selector}"), true, String::new()),
            Ok(value) => (
                format!("type {selector}"),
                false,
                value.as_str().unwrap_or("olmadı").to_string(),
            ),
            Err(error) => (format!("type {selector}"), false, error),
        };
    }
    if let Some(ms) = step["wait_ms"].as_u64() {
        cdp.settle(Duration::from_millis(ms.min(10_000))).await;
        return (format!("wait {ms} ms"), true, String::new());
    }
    if let Some(wanted) = step["expect_text"].as_str() {
        let found = cdp
            .eval(&format!("document.body.innerText.includes({})", js(wanted)))
            .await;
        return match found {
            Ok(value) if value == true => (format!("expect_text {wanted:?}"), true, String::new()),
            Ok(_) => {
                let body = cdp
                    .eval("document.body.innerText.slice(0, 300)")
                    .await
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_default();
                (
                    format!("expect_text {wanted:?}"),
                    false,
                    format!("sayfada yok; sayfa metni: {}", body.replace('\n', " ⏎ ")),
                )
            }
            Err(error) => (format!("expect_text {wanted:?}"), false, error),
        };
    }
    if let Some(selector) = step["expect_selector"].as_str() {
        let found = cdp
            .eval(&format!("!!document.querySelector({})", js(selector)))
            .await;
        return match found {
            Ok(value) if value == true => {
                (format!("expect_selector {selector}"), true, String::new())
            }
            Ok(_) => (
                format!("expect_selector {selector}"),
                false,
                "öğe yok".into(),
            ),
            Err(error) => (format!("expect_selector {selector}"), false, error),
        };
    }
    if let Some(code) = step["eval"].as_str() {
        return match cdp.eval(code).await {
            Ok(value) if value.is_null() || value == false || value == "" || value == 0 => (
                format!("eval {}", code.chars().take(60).collect::<String>()),
                false,
                format!("sonuç boş/yanlış: {value}"),
            ),
            Ok(value) => (
                format!("eval {}", code.chars().take(60).collect::<String>()),
                true,
                value.to_string().chars().take(200).collect(),
            ),
            Err(error) => (
                format!("eval {}", code.chars().take(60).collect::<String>()),
                false,
                error,
            ),
        };
    }
    (
        "bilinmeyen adım".into(),
        false,
        "click, type, wait_ms, expect_text, expect_selector ya da eval ver".into(),
    )
}

/// What the console and the network showed, from the events kept.
fn problems(events: &[Value]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut errors, mut requests, mut dialogs) = (Vec::new(), Vec::new(), Vec::new());
    for event in events {
        let params = &event["params"];
        match event["method"].as_str().unwrap_or("") {
            "Runtime.exceptionThrown" => {
                let details = &params["exceptionDetails"];
                let what = details["exception"]["description"]
                    .as_str()
                    .or(details["text"].as_str())
                    .unwrap_or("hata")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let place = details["url"]
                    .as_str()
                    .map(|url| {
                        format!(
                            " ({}:{})",
                            url.rsplit('/').next().unwrap_or(url),
                            details["lineNumber"].as_i64().unwrap_or(0) + 1
                        )
                    })
                    .unwrap_or_default();
                errors.push(format!("{what}{place}"));
            }
            "Runtime.consoleAPICalled"
                if matches!(params["type"].as_str(), Some("error") | Some("assert")) =>
            {
                let text: Vec<String> = params["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|arg| {
                        arg["value"]
                            .as_str()
                            .map(str::to_string)
                            .or_else(|| arg["description"].as_str().map(str::to_string))
                            .unwrap_or_default()
                    })
                    .collect();
                errors.push(format!(
                    "console.error: {}",
                    text.join(" ").chars().take(200).collect::<String>()
                ));
            }
            "Network.responseReceived" => {
                let status = params["response"]["status"].as_i64().unwrap_or(0);
                let url = params["response"]["url"].as_str().unwrap_or("");
                if status >= 400 && !url.ends_with("favicon.ico") {
                    requests.push(format!(
                        "{status} {}",
                        url.rsplit('/').next().unwrap_or(url)
                    ));
                }
            }
            "Network.loadingFailed" if params["canceled"] != true => {
                requests.push(format!(
                    "yüklenemedi: {}",
                    params["errorText"].as_str().unwrap_or("")
                ));
            }
            "Page.javascriptDialogOpening" => {
                dialogs.push(format!(
                    "{}: {}",
                    params["type"].as_str().unwrap_or("alert"),
                    params["message"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(160)
                        .collect::<String>()
                ));
            }
            _ => {}
        }
    }
    errors.dedup();
    requests.dedup();
    (errors, requests, dialogs)
}

async fn browser_ready(child: &mut tokio::process::Child) -> Result<u16, String> {
    let stderr = child.stderr.take().ok_or("tarayıcı çıktısı yok")?;
    let mut lines = BufReader::new(stderr).lines();
    let waited = tokio::time::timeout(Duration::from_secs(15), async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(rest) = line.split("ws://127.0.0.1:").nth(1) {
                if let Some(port) = rest
                    .split('/')
                    .next()
                    .and_then(|port| port.parse::<u16>().ok())
                {
                    return Some(port);
                }
            }
        }
        None
    })
    .await;
    // Keeps reading what the browser prints, so its pipe never fills.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    waited
        .ok()
        .flatten()
        .ok_or_else(|| "tarayıcı 15 sn içinde hazır olmadı".into())
}

/// Opens `args.url`, runs `args.steps`, and returns the report (`ok` is true when nothing went wrong).
pub async fn check(args: &Value) -> Result<Value, String> {
    let url = args["url"].as_str().unwrap_or("").trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with("file://")) {
        return Err("url http://, https:// ya da file:// ile başlamalı (örn. http://localhost:8000 ya da file:///Users/…/index.html).".into());
    }
    let browser: PathBuf = wiqo_ops::installed_browsers()
        .into_iter()
        .next()
        .ok_or("Chrome ya da Edge kurulu değil; sayfa denenemedi.")?;
    let profile = std::env::temp_dir().join(format!("wiqo-page-{}", uuid::Uuid::new_v4().simple()));
    let mut child = Command::new(&browser)
        .args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-gpu",
            "--mute-audio",
            "--window-size=1280,900",
            "--allow-file-access-from-files",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("tarayıcı başlamadı: {error}"))?;
    let outcome = drive(&mut child, &url, args).await;
    let _ = child.kill().await;
    let _ = std::fs::remove_dir_all(&profile);
    outcome
}

async fn drive(
    child: &mut tokio::process::Child,
    url: &str,
    args: &Value,
) -> Result<Value, String> {
    let port = browser_ready(child).await?;
    let list = reqwest::get(format!("http://127.0.0.1:{port}/json/list"))
        .await
        .map_err(|error| format!("tarayıcıya ulaşılamadı: {error}"))?
        .json::<Value>()
        .await
        .map_err(|error| error.to_string())?;
    let target = list
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| item["type"] == "page")
        .and_then(|item| item["webSocketDebuggerUrl"].as_str())
        .ok_or("açık sayfa bulunamadı")?
        .to_string();
    let (socket, _) = tokio_tungstenite::connect_async(target.as_str())
        .await
        .map_err(|error| format!("tarayıcıya bağlanılamadı: {error}"))?;
    let mut cdp = Cdp {
        socket,
        next: 0,
        events: Vec::new(),
    };
    for domain in [
        "Page.enable",
        "Runtime.enable",
        "Network.enable",
        "Log.enable",
    ] {
        cdp.call(domain, json!({})).await?;
    }
    cdp.call("Page.navigate", json!({ "url": url })).await?;
    // The page is loaded when the browser says so (or after the wait); a script that runs late gets its moment.
    let loaded = Instant::now() + LOAD_WAIT;
    while Instant::now() < loaded
        && !cdp
            .events
            .iter()
            .any(|event| event["method"] == "Page.loadEventFired")
    {
        cdp.settle(Duration::from_millis(250)).await;
    }
    cdp.settle(Duration::from_millis(
        args["wait_ms"].as_u64().unwrap_or(800).min(10_000),
    ))
    .await;

    let mut steps: Vec<Value> = Vec::new();
    let mut all_held = true;
    for step in args["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MOST_STEPS)
    {
        let (what, held, detail) = run_step(&mut cdp, step).await;
        cdp.settle(Duration::from_millis(250)).await;
        all_held &= held;
        steps.push(json!({ "step": what, "ok": held, "detail": detail }));
        if !held {
            break;
        }
    }

    let title = cdp
        .eval("document.title")
        .await
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    let text = cdp
        .eval("document.body ? document.body.innerText.slice(0, 500) : ''")
        .await
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    let shot = args["screenshot"].as_str().filter(|path| !path.is_empty());
    let mut saved = None;
    if let Some(path) = shot {
        if let Ok(reply) = cdp
            .call("Page.captureScreenshot", json!({ "format": "png" }))
            .await
        {
            use base64::Engine;
            if let Some(bytes) = reply["data"]
                .as_str()
                .and_then(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
            {
                let target = std::path::PathBuf::from(path.replacen(
                    '~',
                    &dirs::home_dir().unwrap_or_default().to_string_lossy(),
                    1,
                ));
                if target.is_absolute() && std::fs::write(&target, bytes).is_ok() {
                    saved = Some(target.display().to_string());
                }
            }
        }
    }
    let (errors, requests, dialogs) = problems(&cdp.events);
    let ok = errors.is_empty() && requests.is_empty() && all_held;
    let list =
        |items: &[String]| -> Vec<String> { items.iter().take(MOST_LISTED).cloned().collect() };
    Ok(json!({
        "ok": ok, "url": url, "title": title, "consoleErrors": list(&errors), "failedRequests": list(&requests), "dialogs": list(&dialogs),
        "steps": steps, "pageText": text, "screenshot": saved,
        "summary": if ok { "Sayfa hatasız açıldı ve adımlar tuttu.".to_string() } else { format!("{} konsol hatası, {} başarısız istek{}.", errors.len(), requests.len(), if all_held { "" } else { "; bir adım tutmadı" }) }
    }))
}

#[cfg(test)]
mod tests {
    use super::problems;
    use serde_json::json;

    #[test]
    fn the_console_and_the_network_are_read_for_what_went_wrong() {
        let events = vec![
            json!({ "method": "Runtime.exceptionThrown", "params": { "exceptionDetails": { "text": "Uncaught", "url": "http://localhost:8000/script.js", "lineNumber": 115, "exception": { "description": "SyntaxError: Identifier 'matrix' has already been declared\n  at x" } } } }),
            json!({ "method": "Network.responseReceived", "params": { "response": { "status": 404, "url": "http://localhost:8000/app.css" } } }),
            json!({ "method": "Network.responseReceived", "params": { "response": { "status": 404, "url": "http://localhost:8000/favicon.ico" } } }),
            json!({ "method": "Page.javascriptDialogOpening", "params": { "type": "alert", "message": "Matris yok." } }),
        ];
        let (errors, requests, dialogs) = problems(&events);
        assert_eq!(
            errors,
            vec!["SyntaxError: Identifier 'matrix' has already been declared (script.js:116)"]
        );
        assert_eq!(
            requests,
            vec!["404 app.css"],
            "the favicon is not the page's fault"
        );
        assert_eq!(dialogs, vec!["alert: Matris yok."]);
    }
}
