//! The desktop's own agent: PC work runs here, the way a coding agent runs on the developer's machine. The loop is local (model call → this computer's
//! tools → result → model call), the tools are the terminal session's own (rova-free: the same grants, policy, undo and full-access switch), and the only
//! thing that leaves the computer is each model call, through the server's gateway (`/agent/model`) with the run id the chat turn handed over.
//! Nothing waits on a round trip per step, and nothing depends on the chat's stream staying open.

use std::path::{Path, PathBuf};
use std::time::Instant;

use super::*;

/// How many model turns, and for how long, one request may run.
const MAX_TURNS: usize = 150;
const MAX_MINUTES: u64 = 180;
/// What one tool result may give the model, and what the whole working context may hold before old results are shortened.
const OUTPUT_CHARS: usize = 12_000;
const CONTEXT_CHARS: usize = 900_000;
const KEEP_FULL_RESULTS: usize = 8;
const MODEL_TRIES: usize = 3;

fn tools() -> Value {
    json!([
        { "type": "function", "function": { "name": "shell", "description": "Bu bilgisayarda bir kabuk komutu çalıştırır (macOS zsh, Windows PowerShell). workdir vermezsen ev klasöründe ya da son klasörde çalışır. Kurulum, derleme, indirme gibi uzun komutlar kendiliğinden arka plan işi olur ve job_id döner.",
            "parameters": { "type": "object", "properties": {
                "command": { "type": "string" },
                "workdir": { "type": "string", "description": "Tam klasör yolu, ör. ~/Desktop/proje" },
                "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 50 }
            }, "required": ["command"] } } },
        { "type": "function", "function": { "name": "job", "description": "Arka plan işi: wait (bitene ya da süre dolana kadar bekle ve çıktıyı oku), status, input (çalışan işe girdi yaz), cancel.",
            "parameters": { "type": "object", "properties": {
                "action": { "type": "string", "enum": ["wait", "status", "input", "cancel"] },
                "job_id": { "type": "string" },
                "text": { "type": "string" },
                "wait_seconds": { "type": "integer", "minimum": 5, "maximum": 45 }
            }, "required": ["action", "job_id"] } } },
        { "type": "function", "function": { "name": "read_file", "description": "Bir metin dosyasını okur (isteğe bağlı satır aralığı).",
            "parameters": { "type": "object", "properties": {
                "path": { "type": "string" }, "start_line": { "type": "integer", "minimum": 1 }, "end_line": { "type": "integer", "minimum": 1 }
            }, "required": ["path"] } } },
        { "type": "function", "function": { "name": "write_file", "description": "Dosyayı tam içeriğiyle yazar; klasörü yoksa oluşturur. Var olan dosyayı baştan yazar.",
            "parameters": { "type": "object", "properties": { "path": { "type": "string" }, "content": { "type": "string" } }, "required": ["path", "content"] } } },
        { "type": "function", "function": { "name": "edit_file", "description": "Var olan dosyada bir metni başka bir metinle değiştirir; find dosyada tam ve tek olmalı (yoksa replace_all).",
            "parameters": { "type": "object", "properties": {
                "path": { "type": "string" }, "find": { "type": "string" }, "replace": { "type": "string" }, "replace_all": { "type": "boolean" }
            }, "required": ["path", "find", "replace"] } } },
        { "type": "function", "function": { "name": "search", "description": "Bir klasörde dosya içeriğinde metin ya da regex arar.",
            "parameters": { "type": "object", "properties": {
                "pattern": { "type": "string" }, "path": { "type": "string", "description": "Aranacak klasör; yoksa ev klasörü" }, "glob": { "type": "string" }
            }, "required": ["pattern"] } } }
    ])
}

/// "~/x" and absolute paths as a real path; a relative one stays relative.
fn expand(path: &str) -> PathBuf {
    let path = path.trim();
    if path == "~" {
        return dirs::home_dir().unwrap_or_default();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return dirs::home_dir().unwrap_or_default().join(rest);
    }
    PathBuf::from(path)
}

/// A file named by its full path is opened in its own folder: the session's grant covers that folder.
fn file_and_folder(path: &str) -> (String, Option<String>) {
    let full = expand(path);
    if full.is_absolute() {
        let folder = full.parent().map(|parent| parent.to_string_lossy().to_string());
        let name = full.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
        return (name, folder);
    }
    (path.to_string(), None)
}

fn environment() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macOS (zsh)",
        "windows" => "Windows (PowerShell)",
        other => other,
    };
    let home = dirs::home_dir().map(|home| home.to_string_lossy().to_string()).unwrap_or_default();
    let access = if wiqo_ops::full_access() {
        "Tam yetki açık: komutlar ve dosya değişiklikleri sorulmadan çalışır."
    } else {
        "Sorarak modu: değiştiren her komut ve dosya yazımı kullanıcıya sorulur; reddedilirse başka yol öner."
    };
    format!(
        "ORTAM\n- İşletim sistemi: {os}\n- Ev klasörü: {home}\n- Şimdi: {}\n- {access}",
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    )
}

fn text_of(value: &Value) -> String {
    value.as_str().unwrap_or("").to_string()
}

fn clip(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        return text.to_string();
    }
    let head: String = text.chars().take(most / 3).collect();
    let tail: String = text.chars().rev().take(most - most / 3).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n…(arası kısaltıldı)…\n{tail}")
}

/// Old tool results are shortened once the context grows past its size; the last few stay whole.
fn compact(messages: &mut [Value]) {
    let total: usize = messages.iter().map(|message| message.to_string().len()).sum();
    if total <= CONTEXT_CHARS {
        return;
    }
    let tool_indices: Vec<usize> = messages.iter().enumerate().filter(|(_, message)| message["role"] == "tool").map(|(index, _)| index).collect();
    let keep_from = tool_indices.len().saturating_sub(KEEP_FULL_RESULTS);
    for index in &tool_indices[..keep_from] {
        let content = text_of(&messages[*index]["content"]);
        if content.len() > 600 {
            messages[*index]["content"] = json!(format!("{}… (eski sonuç kısaltıldı)", content.chars().take(500).collect::<String>()));
        }
    }
}

/// What the person reads for a tool call while it runs.
fn label(name: &str, args: &Value) -> String {
    let one = |key: &str| text_of(&args[key]).chars().take(120).collect::<String>();
    match name {
        "shell" => format!("Komut: {}", one("command")),
        "job" => format!("Arka plan işi: {}", one("action")),
        "read_file" => format!("Okuyor: {}", one("path")),
        "write_file" => format!("Yazıyor: {}", one("path")),
        "edit_file" => format!("Düzenliyor: {}", one("path")),
        "search" => format!("Arıyor: {}", one("pattern")),
        other => other.to_string(),
    }
}

impl Chat {
    /// Runs one handed-over request to its end on this computer. `handoff` is the chat turn's `localAgent`: run id, instructions, goal.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn local_agent(
        &self,
        engine: &Engine,
        handoff: &Value,
        history: &Value,
        text: &str,
        token: &str,
        approve: &Approve,
        stop: &Stop,
        emit: &impl Fn(Value),
    ) -> Result<()> {
        let run_id = handoff["runId"].as_str().unwrap_or("").to_string();
        if Uuid::parse_str(&run_id).is_err() {
            bail!("Yerel çalışma başlatılamadı.");
        }
        let goal = handoff["goal"].as_str().filter(|goal| !goal.trim().is_empty()).unwrap_or(text);
        let mut messages = vec![json!({ "role": "system", "content": format!("{}\n\n{}", text_of(&handoff["instructions"]), environment()) })];
        for item in history.as_array().map(|items| &items[items.len().saturating_sub(10)..]).unwrap_or(&[]) {
            let role = item["role"].as_str().unwrap_or("");
            let content = text_of(&item["content"]);
            if matches!(role, "user" | "assistant") && !content.trim().is_empty() {
                messages.push(json!({ "role": role, "content": clip(&content, 4_000) }));
            }
        }
        messages.push(json!({ "role": "user", "content": if goal.trim() == text.trim() { text.to_string() } else { format!("{text}\n\n{goal}") } }));
        emit(json!({ "type": "reset" }));
        let started = Instant::now();
        let mut reply = String::new();
        let mut thinking = 0usize;
        for turn in 0..MAX_TURNS {
            if stop.is_asked() {
                reply = "Durduruldu; o ana kadar yapılanlar yerinde.".into();
                break;
            }
            if started.elapsed() > Duration::from_secs(MAX_MINUTES * 60) {
                reply = format!("{MAX_MINUTES} dakikayı aştı; yapılanlar yerinde. Devam etmemi istersen yaz.");
                break;
            }
            compact(&mut messages);
            thinking += 1;
            let think_id = format!("local-think-{thinking}");
            emit(json!({ "type": "activity", "activity": { "id": think_id, "phase": "plan", "label": if turn == 0 { "Düşünüyor" } else { "Sonuca bakıyor" }, "state": "running" } }));
            let answer = match self.model_call(token, &run_id, &messages).await {
                Ok(answer) => answer,
                Err(error) => {
                    emit(json!({ "type": "activity", "activity": { "id": think_id, "phase": "plan", "label": "Model yanıt vermedi", "state": "failed" } }));
                    reply = format!("Model şu an yanıt vermiyor ({error}). Yapılanlar yerinde; biraz sonra devam etmemi isteyebilirsin.");
                    break;
                }
            };
            emit(json!({ "type": "activity", "activity": { "id": think_id, "phase": "plan", "label": if turn == 0 { "Düşünüyor" } else { "Sonuca bakıyor" }, "state": "done" } }));
            let content = text_of(&answer["content"]);
            let calls = answer["tool_calls"].as_array().cloned().unwrap_or_default();
            if calls.is_empty() {
                reply = content;
                break;
            }
            messages.push(json!({ "role": "assistant", "content": content, "tool_calls": calls }));
            for call in &calls {
                let id = text_of(&call["id"]);
                let name = text_of(&call["function"]["name"]);
                let args: Value = serde_json::from_str(call["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
                let words = label(&name, &args);
                let step_id = format!("local-{}", Uuid::new_v4());
                emit(json!({ "type": "activity", "activity": { "id": step_id, "phase": "tool", "label": words, "state": "running" } }));
                let (ok, output) = if stop.is_asked() {
                    (false, json!({ "status": "denied", "message": "Kullanıcı durdurdu; çalıştırılmadı." }).to_string())
                } else {
                    self.local_tool(engine, &name, &args, approve).await
                };
                emit(json!({ "type": "activity", "activity": { "id": step_id, "phase": "tool", "label": words, "state": if ok { "done" } else { "failed" } } }));
                messages.push(json!({ "role": "tool", "tool_call_id": id, "content": output }));
            }
        }
        if reply.trim().is_empty() {
            reply = "İş bitti ama model bir özet yazmadı; adımlar yukarıda.".into();
        }
        emit(json!({ "type": "done", "reply": reply }));
        let _ = self
            .call(reqwest::Method::POST, "agent/finish", Some(json!({ "runId": run_id, "reply": reply })), Some(token))
            .await;
        Ok(())
    }

    /// One model call through the server's gateway, tried again on a dropped connection or a busy model.
    async fn model_call(&self, token: &str, run_id: &str, messages: &[Value]) -> Result<Value> {
        let body = json!({ "runId": run_id, "messages": messages, "tools": tools() });
        let mut last = anyhow!("Model yanıt vermedi.");
        for attempt in 0..MODEL_TRIES {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
            }
            let sent = self
                .http
                .post(format!("{}/agent/model", self.server))
                .bearer_auth(token)
                .timeout(Duration::from_secs(150))
                .json(&body)
                .send()
                .await;
            let response = match sent {
                Ok(response) => response,
                Err(_) => {
                    last = anyhow!("sunucuya ulaşılamadı");
                    continue;
                }
            };
            let status = response.status();
            let value: Value = response.json().await.unwrap_or(Value::Null);
            if status.is_success() {
                return Ok(value["message"].clone());
            }
            let message = value["error"]["message"].as_str().unwrap_or("model hatası").to_string();
            // The run is over or out of calls: trying again changes nothing.
            if matches!(status.as_u16(), 400 | 401 | 403 | 404 | 429) {
                bail!(message);
            }
            last = anyhow!(message);
        }
        Err(last)
    }

    /// One tool call, run by this computer's terminal session; what goes back to the model is its status, its message and its output.
    async fn local_tool(&self, engine: &Engine, name: &str, args: &Value, approve: &Approve) -> (bool, String) {
        let full = wiqo_ops::full_access();
        let refuse = |message: &str| (false, json!({ "status": "failed", "message": message }).to_string());
        let (value, changes, question) = match name {
            "shell" => {
                let command = text_of(&args["command"]);
                if command.trim().is_empty() {
                    return refuse("command gerekli.");
                }
                let reads = wiqo_ops::is_read_only(&command);
                let mut value = json!({ "action": "run", "command": command, "mode": if full || !reads { "full" } else { "read" } });
                if let Some(workdir) = args["workdir"].as_str().filter(|dir| !dir.trim().is_empty()) {
                    value["workingDirectory"] = json!(workdir);
                }
                if let Some(seconds) = args["timeout_seconds"].as_u64() {
                    value["timeoutSeconds"] = json!(seconds.clamp(1, 50));
                }
                (value, !reads, format!("Komut çalıştırılsın mı?\n{command}"))
            }
            "job" => {
                let job = text_of(&args["job_id"]);
                let value = match args["action"].as_str().unwrap_or("") {
                    "wait" => json!({ "action": "wait_job", "jobId": job, "waitSeconds": args["wait_seconds"].as_u64().unwrap_or(45).clamp(5, 45) }),
                    "status" => json!({ "action": "get_job", "jobId": job }),
                    "input" => json!({ "action": "send_input", "jobId": job, "input": text_of(&args["text"]) }),
                    "cancel" => json!({ "action": "cancel_job", "jobId": job }),
                    _ => return refuse("action wait, status, input ya da cancel olmalı."),
                };
                let writes = args["action"] == "input";
                (value, writes, format!("Çalışan işe girdi yazılsın mı?\n{}", text_of(&args["text"])))
            }
            "read_file" => {
                let (file, folder) = file_and_folder(&text_of(&args["path"]));
                let mut value = json!({ "action": "read_file", "filePath": file, "mode": if full { "full" } else { "read" } });
                if let Some(folder) = folder {
                    value["workingDirectory"] = json!(folder);
                }
                for (from, to) in [("start_line", "startLine"), ("end_line", "endLine")] {
                    if let Some(line) = args[from].as_u64() {
                        value[to] = json!(line);
                    }
                }
                (value, false, String::new())
            }
            "write_file" => {
                let path = text_of(&args["path"]);
                let (file, folder) = file_and_folder(&path);
                if let Some(folder) = &folder {
                    let _ = std::fs::create_dir_all(Path::new(folder));
                }
                let mut value = json!({ "action": "write_file", "filePath": file, "content": text_of(&args["content"]), "mode": "full" });
                if let Some(folder) = folder {
                    value["workingDirectory"] = json!(folder);
                }
                (value, true, format!("Dosya yazılsın mı?\n{path}"))
            }
            "edit_file" => {
                let path = text_of(&args["path"]);
                let (file, folder) = file_and_folder(&path);
                let mut value = json!({ "action": "edit_file", "mode": "full", "replacements": [{
                    "filePath": file, "find": text_of(&args["find"]), "replace": text_of(&args["replace"]), "replaceAll": args["replace_all"] == true
                }] });
                if let Some(folder) = folder {
                    value["workingDirectory"] = json!(folder);
                }
                (value, true, format!("Dosya düzenlensin mi?\n{path}"))
            }
            "search" => {
                let folder = args["path"].as_str().filter(|path| !path.trim().is_empty()).unwrap_or("~");
                let mut value = json!({ "action": "search", "pattern": text_of(&args["pattern"]), "workingDirectory": folder, "mode": if full { "full" } else { "read" } });
                if let Some(glob) = args["glob"].as_str() {
                    value["glob"] = json!(glob);
                }
                (value, false, String::new())
            }
            other => return refuse(&format!("{other} diye bir araç yok.")),
        };
        // Sorarak: a change is asked on this computer, with the exact command or file; under full access it runs.
        if changes && !full {
            match approve("terminal".into(), question).await {
                Decision::Yes => {}
                Decision::No => return refuse("Kullanıcı onay vermedi; çalıştırılmadı. Başka bir yol öner ya da ne yapmak istediğini sor."),
                Decision::TimedOut => return refuse("Onay zamanında gelmedi; çalıştırılmadı."),
            }
        }
        let step = json!({ "id": Uuid::new_v4().to_string(), "type": "terminal", "summary": label(name, args), "payload": { "kind": "terminal", "value": value } });
        let result = run_step_as(engine, &step, false, false).await;
        let ok = result["status"] == "completed";
        let mut shown = json!({ "status": result["status"], "message": result["message"] });
        if let Some(code) = result["errorCode"].as_str() {
            shown["errorCode"] = json!(code);
            shown["error"] = result["error"].clone();
        }
        if let Some(data) = result["dataText"].as_str() {
            shown["output"] = json!(clip(data, OUTPUT_CHARS));
        }
        (ok, shown.to_string())
    }
}
