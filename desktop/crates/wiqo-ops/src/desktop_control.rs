//! Driving another app's controls (`computerDesktop`): read what is on screen, then click, type, scroll or press keys on a control by its ref.
//!
//! The work is done by the `agent-desktop` helper program (Apache-2.0, the operating system's accessibility tree, no pictures of the screen). This module only
//! asks it, and turns its answers into the contract's words: the same targets, the same error codes, and a plain `verified` that is true only when the effect
//! was read back. One rule decides whether a failed step may be repeated: the helper's own `delivery` (did the input reach the app), never the wording.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use wiqo_core::engine::{Context, Lane, Operation, Outcome};

use crate::op::{data, outcome, payload, permission, tool_failure, Done, Result};
use crate::Op;

/// An observation older than this is not acted on; the screen has probably changed.
const OBSERVATION_LIFETIME: Duration = Duration::from_secs(30);
const MAX_TARGETS: usize = 120;
const MAX_STEPS: usize = 12;
const CALL_TIMEOUT: Duration = Duration::from_secs(25);

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(ComputerDesktop {
        engine: Arc::new(Helper::locate()),
    })]
}

/// `ComputerDesktopPayload` (and its steps, which carry the same fields).
#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
struct Request {
    task_id: String,
    action: String,
    snapshot_id: Option<String>,
    target_ref: Option<String>,
    source_ref: Option<String>,
    destination_ref: Option<String>,
    direction: Option<String>,
    amount: Option<u32>,
    text: Option<String>,
    keys: Option<Vec<String>>,
    steps: Option<Vec<Request>>,
}

/// What the helper program answers: its envelope, already split into success data or a failure.
#[derive(Debug, Clone)]
pub(crate) struct Answer {
    pub ok: bool,
    pub data: Value,
    pub code: String,
    pub message: String,
    /// `not_delivered`, `delivery_uncertain`, `delivered_unverified`, `delivered_verified` (absent for pure reads).
    pub delivery: Option<String>,
}

/// Something that can run one helper command. The real one starts the program; tests answer from a script.
pub(crate) trait Engine: Send + Sync {
    fn call(&self, args: &[String]) -> std::result::Result<Answer, String>;
}

struct Helper {
    path: Option<PathBuf>,
}

impl Helper {
    /// `WIQO_AGENT_DESKTOP`, else the program shipped next to Wiqo.
    fn locate() -> Helper {
        let name = if cfg!(windows) { "agent-desktop.exe" } else { "agent-desktop" };
        let beside = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(name)));
        let path = std::env::var_os("WIQO_AGENT_DESKTOP")
            .map(PathBuf::from)
            .into_iter()
            .chain(beside)
            .find(|candidate| candidate.is_file());
        Helper { path }
    }
}

impl Engine for Helper {
    fn call(&self, args: &[String]) -> std::result::Result<Answer, String> {
        let path = self.path.as_ref().ok_or("helper missing")?;
        let mut child = Command::new(path)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| source.to_string())?;
        let mut stdout = child.stdout.take().ok_or("no output")?;
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stdout.read_to_string(&mut text);
            text
        });
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if started.elapsed() < CALL_TIMEOUT => std::thread::sleep(Duration::from_millis(25)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("timeout".into());
                }
            }
        }
        let text = reader.join().map_err(|_| "no output")?;
        parse_envelope(&text)
    }
}

fn parse_envelope(text: &str) -> std::result::Result<Answer, String> {
    let envelope: Value = serde_json::from_str(text.trim()).map_err(|_| "unreadable answer".to_string())?;
    let ok = envelope["ok"].as_bool().unwrap_or(false);
    let delivery_of = |value: &Value| value["disposition"]["delivery"].as_str().map(str::to_string);
    if ok {
        let data = envelope["data"].clone();
        let delivery = delivery_of(&data);
        Ok(Answer { ok, data, code: String::new(), message: String::new(), delivery })
    } else {
        let error = &envelope["error"];
        Ok(Answer {
            ok,
            data: error["details"].clone(),
            code: error["code"].as_str().unwrap_or("INTERNAL").to_string(),
            message: error["message"].as_str().unwrap_or_default().to_string(),
            delivery: delivery_of(error),
        })
    }
}

/// The helper's failure, in the contract's words. The code names the failure; the helper's `delivery` decides whether the app may have been touched.
fn contract_error(answer: &Answer) -> wiqo_core::wire::WireError {
    let untouched = matches!(answer.delivery.as_deref(), Some("not_delivered"));
    let (code, category) = match answer.code.as_str() {
        "PERM_DENIED" => ("ACCESSIBILITY_PERMISSION_REQUIRED", "permission"),
        "ELEMENT_NOT_FOUND" | "AMBIGUOUS_TARGET" => ("DESKTOP_TARGET_NOT_FOUND", "tool_failure"),
        "STALE_REF" | "SNAPSHOT_NOT_FOUND" => ("DESKTOP_STALE_OBSERVATION", "tool_failure"),
        "APP_NOT_FOUND" | "WINDOW_NOT_FOUND" => ("DESKTOP_APP_UNAVAILABLE", "tool_failure"),
        "ACTION_NOT_SUPPORTED" | "PLATFORM_NOT_SUPPORTED" | "POLICY_DENIED" => ("DESKTOP_ACTION_UNSUPPORTED", "tool_failure"),
        "INVALID_ARGS" => ("DESKTOP_ACTION_INVALID", "validation"),
        _ if untouched => ("DESKTOP_UNAVAILABLE", "tool_failure"),
        "TIMEOUT" | "APP_UNRESPONSIVE" => ("COMMAND_TIMEOUT", "timeout"),
        _ => ("OUTCOME_UNKNOWN", "tool_failure"),
    };
    // A refusal that says the keys are dangerous is the contract's own hotkey rule.
    let code = if answer.code == "ACTION_FAILED" && answer.message.to_lowercase().contains("dangerous") { "DESKTOP_HOTKEY_BLOCKED" } else { code };
    let message = match code {
        "ACCESSIBILITY_PERMISSION_REQUIRED" => "Masaüstü kontrolü için Erişilebilirlik izni gerekiyor; izin verip yeniden dene.".to_string(),
        "DESKTOP_STALE_OBSERVATION" => "Masaüstü görünümü değişti; işlemden önce yeniden gözlemleyin.".to_string(),
        "DESKTOP_TARGET_NOT_FOUND" => "Hedef güncel masaüstü görünümünde bulunamadı veya birden fazla eşleşti.".to_string(),
        "DESKTOP_APP_UNAVAILABLE" => "Uygulama açık değil ya da penceresi yok.".to_string(),
        "DESKTOP_ACTION_UNSUPPORTED" => "Bu denetim bu işlemi desteklemiyor.".to_string(),
        "DESKTOP_HOTKEY_BLOCKED" => "Bu kısayol güvenlik nedeniyle engellendi.".to_string(),
        "COMMAND_TIMEOUT" | "OUTCOME_UNKNOWN" => "Uygulamanın işlemi yapıp yapmadığı doğrulanamadı; ekranı yeniden gözlemleyin, aynı işlemi tekrarlamayın.".to_string(),
        _ => "Masaüstü işlemi yapılamadı.".to_string(),
    };
    wiqo_core::wire::WireError::new(code, message, category)
}

// ---------- the observation: refs the model can name ----------

#[derive(Debug, Clone, PartialEq)]
struct Target {
    reference: String,
    role: String,
    name: String,
    frame: [f64; 4],
    actions: Vec<String>,
    enabled: bool,
    secure: bool,
}

impl Target {
    fn json(&self) -> Value {
        json!({
            "ref": self.reference, "role": self.role, "name": self.name,
            "frame": { "x": self.frame[0], "y": self.frame[1], "width": self.frame[2], "height": self.frame[3] },
            "actions": self.actions, "enabled": self.enabled, "secure": self.secure,
        })
    }
}

struct Observation {
    snapshot_id: String,
    application: String,
    window_title: String,
    targets: Vec<Target>,
}

fn collect(node: &Value, screen: [f64; 2], out: &mut Vec<Target>) {
    if let Some(reference) = node["ref_id"].as_str() {
        let states: Vec<&str> = node["states"].as_array().map(|list| list.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let bounds = &node["bounds"];
        let (x, y, w, h) = (
            bounds["x"].as_f64().unwrap_or(0.0),
            bounds["y"].as_f64().unwrap_or(0.0),
            bounds["width"].as_f64().unwrap_or(0.0),
            bounds["height"].as_f64().unwrap_or(0.0),
        );
        let actions: Vec<String> = node["available_actions"].as_array().map(|list| list.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        let name = node["name"].as_str().or_else(|| node["value"].as_str()).unwrap_or_default();
        let secure = states.contains(&"secure");
        // Anonymous, action-less, invisible rows are scaffolding, not targets.
        if !actions.is_empty() && w > 1.0 && h > 1.0 && !states.contains(&"offscreen") && out.len() < MAX_TARGETS {
            let round = |value: f64| (value * 100_000.0).round() / 100_000.0;
            out.push(Target {
                reference: reference.to_string(),
                role: node["role"].as_str().unwrap_or("unknown").to_string(),
                // A secure field's content is never read back to the model.
                name: if secure { String::new() } else { name.chars().take(100).collect() },
                frame: [round(x / screen[0]), round(y / screen[1]), round(w / screen[0]), round(h / screen[1])],
                actions,
                enabled: !states.contains(&"disabled"),
                secure,
            });
        }
    }
    for child in node["children"].as_array().into_iter().flatten() {
        collect(child, screen, out);
    }
}

fn observe(engine: &dyn Engine, application: Option<&str>) -> Result<Observation> {
    let screen = engine
        .call(&["list-displays".into()])
        .ok()
        .and_then(|answer| answer.ok.then_some(answer.data))
        .and_then(|displays| {
            let list = displays.as_array()?;
            let first = list.iter().find(|display| display["is_primary"].as_bool() == Some(true)).or_else(|| list.first())?;
            Some([first["bounds"]["width"].as_f64()?.max(1.0), first["bounds"]["height"].as_f64()?.max(1.0)])
        })
        .unwrap_or([1920.0, 1080.0]);
    let mut args: Vec<String> = vec!["snapshot".into(), "-i".into(), "--compact".into(), "--include-bounds".into()];
    if let Some(application) = application.filter(|name| !name.trim().is_empty()) {
        args.extend(["--app".into(), application.trim().to_string()]);
    }
    let answer = engine.call(&args).map_err(unavailable)?;
    if !answer.ok {
        return Err(contract_error(&answer));
    }
    let mut targets = Vec::new();
    collect(&answer.data["tree"], screen, &mut targets);
    Ok(Observation {
        snapshot_id: answer.data["snapshot_id"].as_str().unwrap_or_default().to_string(),
        application: answer.data["app"].as_str().unwrap_or_default().to_string(),
        window_title: answer.data["window"]["title"].as_str().unwrap_or_default().to_string(),
        targets,
    })
}

fn unavailable(reason: String) -> wiqo_core::wire::WireError {
    if reason == "helper missing" {
        tool_failure("DESKTOP_ADAPTER_UNAVAILABLE", "Masaüstü kontrol bileşeni bu Wiqo sürümünde yok.")
    } else if reason == "timeout" {
        wiqo_core::wire::WireError::new("COMMAND_TIMEOUT", "Masaüstü işlemi zamanında bitmedi; sonucu doğrulanamadı.", "timeout")
    } else {
        tool_failure("DESKTOP_UNAVAILABLE", "Masaüstü kontrol bileşeni çalıştırılamadı.")
    }
}

// ---------- what a task last saw ----------

struct Seen {
    snapshot_id: String,
    at: Instant,
    targets: Vec<Target>,
    application: String,
}

fn seen() -> &'static Mutex<HashMap<String, Seen>> {
    static SEEN: OnceLock<Mutex<HashMap<String, Seen>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}

fn remember(task: &str, observation: &Observation) {
    if let Ok(mut map) = seen().lock() {
        map.insert(task.to_string(), Seen {
            snapshot_id: observation.snapshot_id.clone(),
            at: Instant::now(),
            targets: observation.targets.clone(),
            application: observation.application.clone(),
        });
    }
}

// ---------- the operation ----------

struct ComputerDesktop {
    engine: Arc<dyn Engine>,
}

#[async_trait]
impl Operation for ComputerDesktop {
    fn name(&self) -> &'static str {
        "computerDesktop"
    }

    fn lane(&self) -> Lane {
        Lane::SideEffect
    }

    async fn run(&self, _ctx: &Context, value: &Value) -> Outcome {
        let engine = Arc::clone(&self.engine);
        let value = value.clone();
        let result = tokio::task::spawn_blocking(move || control(engine.as_ref(), &value)).await;
        outcome(result.unwrap_or_else(|_| Err(tool_failure("DESKTOP_UNAVAILABLE", "Masaüstü işlemi tamamlanamadı."))))
    }
}

fn control(engine: &dyn Engine, value: &Value) -> Result<Done> {
    let request: Request = payload(value, "computerDesktop", "masaüstü")?;
    if request.task_id.trim().is_empty() {
        return Err(tool_failure("DESKTOP_TASK_INVALID", "Masaüstü görevi geçersiz."));
    }
    match request.action.as_str() {
        "cancel" => {
            if let Ok(mut map) = seen().lock() {
                map.remove(&request.task_id);
            }
            Ok(Done::new("Masaüstü görevi durduruldu.", Some(content(&request, None, &[], true, false, Vec::new()))))
        }
        "observe" => {
            let observation = observe(engine, request.text.as_deref())?;
            remember(&request.task_id, &observation);
            Ok(Done::new(
                format!("{} uygulamasında {} denetim görüldü.", observation.application, observation.targets.len()),
                Some(content(&request, Some(&observation), &[], true, false, Vec::new())),
            ))
        }
        "runSteps" => {
            let steps = request.steps.clone().unwrap_or_default();
            if steps.is_empty() || steps.len() > MAX_STEPS {
                return Err(tool_failure("DESKTOP_STEPS_INVALID", "Adım listesi geçersiz."));
            }
            run_steps(engine, &request, steps)
        }
        _ => run_steps(engine, &request, vec![request.clone()]),
    }
}

/// Runs the steps in order, and stops at the first one that fails or leaves the result in doubt. After every step the screen is read again, so what the
/// model is told is what the app shows now, not what the helper hoped for.
fn run_steps(engine: &dyn Engine, request: &Request, steps: Vec<Request>) -> Result<Done> {
    let before = guard_observation(request)?;
    let mut previous = before;
    let mut evidence = Vec::new();
    let mut latest: Option<Observation> = None;
    let mut failure: Option<wiqo_core::wire::WireError> = None;
    for (index, step) in steps.iter().enumerate() {
        let target = step.target_ref.as_deref().and_then(|reference| previous.iter().find(|target| target.reference == reference));
        let verdict = match perform(engine, step, target) {
            Ok(answer) => answer,
            Err(error) => {
                evidence.push(json!({ "index": index, "action": step.action, "targetRef": step.target_ref, "ok": false, "changed": false, "verified": false, "errorCode": error.code }));
                failure = Some(error);
                break;
            }
        };
        let delivery = verdict.delivery.clone().unwrap_or_default();
        // Read the screen again: a step is verified when the helper read its effect back, or when the controls on screen visibly changed.
        let after = observe(engine, None).ok();
        let changed = after.as_ref().is_some_and(|after| differs(&previous, &after.targets));
        let verified = delivery == "delivered_verified" || (delivery != "not_delivered" && changed);
        evidence.push(json!({ "index": index, "action": step.action, "targetRef": step.target_ref, "ok": true, "changed": changed, "verified": verified }));
        if let Some(after) = after {
            remember(&request.task_id, &after);
            previous = after.targets.clone();
            latest = Some(after);
        }
        // An input that may have landed and shows no effect is not repeated by the next step: the model looks first.
        if !verified && delivery != "not_delivered" && index + 1 < steps.len() {
            failure = Some(wiqo_core::wire::WireError::new("DESKTOP_TYPE_UNVERIFIED", "Adımın etkisi görülemedi; sonraki adımlar çalıştırılmadı.", "tool_failure"));
            break;
        }
    }
    let all_verified = failure.is_none() && !evidence.is_empty() && evidence.iter().all(|entry| entry["verified"] == true);
    let any_changed = evidence.iter().any(|entry| entry["changed"] == true);
    let body = content(request, latest.as_ref(), &[], true, any_changed, evidence);
    let body = with_verified(body, all_verified);
    match failure {
        Some(error) => Ok(Done::partial(error, body)),
        None => Ok(Done::new(
            if all_verified { "Masaüstü işlemi yapıldı ve ekranda görüldü." } else { "Masaüstü işlemi gönderildi; etkisi ekranda doğrulanamadı." },
            Some(body),
        )),
    }
}

fn with_verified(mut body: Value, verified: bool) -> Value {
    body["value"]["verified"] = json!(verified);
    body
}

/// The step's own rules before anything is sent: an observation exists, is recent, and is the one the model named.
fn guard_observation(request: &Request) -> Result<Vec<Target>> {
    let map = seen().lock().map_err(|_| tool_failure("DESKTOP_UNAVAILABLE", "Masaüstü durumu okunamadı."))?;
    let prior = map.get(&request.task_id).ok_or_else(|| tool_failure("DESKTOP_OBSERVATION_REQUIRED", "Önce masaüstünü gözlemleyip güncel hedefleri alın."))?;
    if prior.at.elapsed() > OBSERVATION_LIFETIME {
        return Err(tool_failure("DESKTOP_STALE_OBSERVATION", "Masaüstü gözlemi eskidi; işlemden önce yeniden gözlemleyin."));
    }
    if request.snapshot_id.as_deref().is_some_and(|named| named != prior.snapshot_id) {
        return Err(tool_failure("DESKTOP_STALE_OBSERVATION", "Masaüstü görünümü değişti; işlemden önce yeniden gözlemleyin."));
    }
    let _ = &prior.application;
    Ok(prior.targets.clone())
}

fn differs(before: &[Target], after: &[Target]) -> bool {
    before.len() != after.len() || before.iter().zip(after).any(|(a, b)| a.role != b.role || a.name != b.name || a.enabled != b.enabled)
}

/// One step as helper commands. A ref action is idempotent where the helper offers it, and physical input is never used unless the step is a drag or a double click.
fn perform(engine: &dyn Engine, step: &Request, target: Option<&Target>) -> Result<Answer> {
    let reference = step.target_ref.clone();
    let need_ref = || reference.clone().ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Bu işlem için bir hedef gerekiyor."));
    let known = |reference: &str| -> Result<()> {
        // A ref the model never saw is refused here; the helper would resolve it, but the model must have looked first.
        match target {
            Some(_) => Ok(()),
            None => Err(tool_failure("DESKTOP_TARGET_NOT_FOUND", format!("{reference} hedefi güncel görünümde yok."))),
        }
    };
    let args: Vec<String> = match step.action.as_str() {
        "click" => { let r = need_ref()?; known(&r)?; refuse_secure(target)?; vec!["click".into(), r] }
        "rightClick" => { let r = need_ref()?; known(&r)?; refuse_secure(target)?; vec!["right-click".into(), r] }
        "doubleClick" => { let r = need_ref()?; known(&r)?; refuse_secure(target)?; vec!["double-click".into(), r, "--headed".into()] }
        "type" => {
            let r = need_ref()?;
            known(&r)?;
            if target.is_some_and(|target| target.secure) {
                return Err(tool_failure("DESKTOP_SECURE_FIELD_BLOCKED", "Parola alanına yazılmaz; kullanıcı kendisi girmeli."));
            }
            let text = step.text.clone().filter(|text| !text.is_empty()).ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Yazılacak metin yok."))?;
            vec!["type".into(), r, text]
        }
        "scroll" => {
            let r = need_ref()?;
            known(&r)?;
            let direction = step.direction.clone().unwrap_or_else(|| "down".into());
            if !["up", "down", "left", "right"].contains(&direction.as_str()) {
                return Err(tool_failure("DESKTOP_ACTION_INVALID", "Kaydırma yönü geçersiz."));
            }
            vec!["scroll".into(), r, "--direction".into(), direction, "--amount".into(), step.amount.unwrap_or(3).clamp(1, 20).to_string()]
        }
        "hotkey" => {
            let keys = step.keys.clone().filter(|keys| !keys.is_empty()).ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Kısayol tuşları yok."))?;
            vec!["press".into(), keys.join("+")]
        }
        "dragDrop" => {
            let (from, to) = (step.source_ref.clone(), step.destination_ref.clone());
            let (Some(from), Some(to)) = (from, to) else { return Err(tool_failure("DESKTOP_ACTION_INVALID", "Sürükleme için kaynak ve hedef gerekiyor.")); };
            vec!["drag".into(), "--from".into(), from, "--to".into(), to, "--headed".into()]
        }
        "wait" => vec!["wait".into(), step.amount.unwrap_or(500).clamp(50, 5000).to_string()],
        _ => return Err(tool_failure("DESKTOP_ACTION_UNSUPPORTED", "Bu masaüstü işlemi desteklenmiyor.")),
    };
    let answer = engine.call(&args).map_err(unavailable)?;
    if answer.ok { Ok(answer) } else { Err(contract_error(&answer)) }
}

fn refuse_secure(target: Option<&Target>) -> Result<()> {
    if target.is_some_and(|target| target.secure) {
        return Err(permission("DESKTOP_SECURE_FIELD_BLOCKED", "Parola alanıyla etkileşilmez; kullanıcı kendisi yapmalı."));
    }
    Ok(())
}

fn content(request: &Request, observation: Option<&Observation>, removed: &[String], access: bool, changed: bool, evidence: Vec<Value>) -> Value {
    let targets: Vec<Value> = observation.map(|observation| observation.targets.iter().map(Target::json).collect()).unwrap_or_default();
    data("computerDesktop", json!({
        "taskId": request.task_id,
        "action": request.action,
        "snapshotId": observation.map(|observation| observation.snapshot_id.clone()),
        "applicationName": observation.map(|observation| observation.application.clone()),
        "windowTitle": observation.map(|observation| observation.window_title.clone()),
        "targets": targets,
        "addedTargets": [],
        "changedTargets": [],
        "removedTargetRefs": removed,
        "accessGranted": access,
        "changed": changed,
        "verified": true,
        "evidence": evidence,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers helper commands from a script: the first rule whose words all appear in the command wins.
    struct Script(Mutex<Vec<(Vec<&'static str>, Value)>>);

    impl Script {
        fn new(rules: Vec<(Vec<&'static str>, Value)>) -> Arc<Script> {
            Arc::new(Script(Mutex::new(rules)))
        }
    }

    impl Engine for Script {
        fn call(&self, args: &[String]) -> std::result::Result<Answer, String> {
            let line = args.join(" ");
            let mut rules = self.0.lock().unwrap();
            let position = rules.iter().position(|(words, _)| words.iter().all(|word| line.contains(word))).ok_or_else(|| format!("no rule for {line}"))?;
            let envelope = if rules.iter().filter(|(words, _)| words.iter().all(|word| line.contains(word))).count() > 1 {
                rules.remove(position).1
            } else {
                rules[position].1.clone()
            };
            parse_envelope(&envelope.to_string())
        }
    }

    fn screen(snapshot: &str, button_name: &str) -> Value {
        json!({ "ok": true, "data": { "app": "Hesap", "snapshot_id": snapshot, "window": { "title": "Hesap" }, "tree": {
            "role": "window", "children": [
                { "ref_id": format!("@{snapshot}:e1"), "role": "button", "name": button_name, "available_actions": ["Click"], "bounds": { "x": 100.0, "y": 50.0, "width": 40.0, "height": 20.0 } },
                { "ref_id": format!("@{snapshot}:e2"), "role": "textfield", "name": "Parola", "states": ["secure"], "available_actions": ["SetValue"], "bounds": { "x": 0.0, "y": 0.0, "width": 80.0, "height": 20.0 } },
                { "ref_id": format!("@{snapshot}:e3"), "role": "group", "name": "kap", "bounds": { "x": 0.0, "y": 0.0, "width": 80.0, "height": 20.0 } }
            ] } } })
    }

    fn displays() -> (Vec<&'static str>, Value) {
        (vec!["list-displays"], json!({ "ok": true, "data": [{ "is_primary": true, "bounds": { "width": 1000.0, "height": 500.0 } }] }))
    }

    fn request(task: &str, action: &str, extra: Value) -> Value {
        let mut value = json!({ "taskId": task, "action": action });
        for (key, item) in extra.as_object().unwrap() {
            value[key] = item.clone();
        }
        json!({ "kind": "computerDesktop", "value": value })
    }

    fn result(done: Result<Done>) -> (Value, Option<String>) {
        match done {
            Ok(done) => (done.data.unwrap_or(Value::Null), done.error.map(|error| error.code)),
            Err(error) => (Value::Null, Some(error.code)),
        }
    }

    #[test]
    fn observing_lists_only_real_controls_with_normalised_frames_and_hides_secure_names() {
        let engine = Script::new(vec![displays(), (vec!["snapshot"], screen("s1", "7"))]);
        let (data, code) = result(control(engine.as_ref(), &request("t-obs", "observe", json!({}))));
        assert_eq!(code, None);
        let targets = data["value"]["targets"].as_array().unwrap();
        assert_eq!(targets.len(), 2, "the anonymous group is scaffolding");
        assert_eq!(targets[0]["frame"]["x"], 0.1);
        assert_eq!(targets[0]["frame"]["width"], 0.04);
        assert_eq!(targets[1]["secure"], true);
        assert_eq!(targets[1]["name"], "");
    }

    #[test]
    fn a_click_needs_a_recent_observation_that_the_model_named() {
        let engine = Script::new(vec![displays(), (vec!["snapshot"], screen("s2", "7"))]);
        let none = result(control(engine.as_ref(), &request("t-none", "click", json!({ "targetRef": "@s2:e1" }))));
        assert_eq!(none.1.as_deref(), Some("DESKTOP_OBSERVATION_REQUIRED"));
        control(engine.as_ref(), &request("t-snap", "observe", json!({}))).unwrap();
        let wrong = result(control(engine.as_ref(), &request("t-snap", "click", json!({ "targetRef": "@s2:e1", "snapshotId": "other" }))));
        assert_eq!(wrong.1.as_deref(), Some("DESKTOP_STALE_OBSERVATION"));
        let unseen = result(control(engine.as_ref(), &request("t-snap", "click", json!({ "targetRef": "@s2:e99" }))));
        assert_eq!(unseen.1.as_deref(), Some("DESKTOP_TARGET_NOT_FOUND"));
    }

    #[test]
    fn a_click_is_verified_by_reading_the_screen_again() {
        let engine = Script::new(vec![
            displays(),
            (vec!["snapshot"], screen("s3", "7")),
            (vec!["click"], json!({ "ok": true, "data": { "action": "click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } })),
            (vec!["snapshot"], screen("s4", "8")),
        ]);
        control(engine.as_ref(), &request("t-click", "observe", json!({}))).unwrap();
        let (data, code) = result(control(engine.as_ref(), &request("t-click", "click", json!({ "targetRef": "@s3:e1" }))));
        assert_eq!(code, None);
        assert_eq!(data["value"]["verified"], true, "the button's name changed on screen");
        assert_eq!(data["value"]["changed"], true);
        assert_eq!(data["value"]["snapshotId"], "s4");
    }

    #[test]
    fn a_click_whose_effect_cannot_be_seen_is_sent_but_not_called_verified() {
        let engine = Script::new(vec![
            displays(),
            (vec!["snapshot"], screen("s5", "7")),
            (vec!["click"], json!({ "ok": true, "data": { "action": "click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } })),
            (vec!["snapshot"], screen("s5", "7")),
        ]);
        control(engine.as_ref(), &request("t-quiet", "observe", json!({}))).unwrap();
        let (data, code) = result(control(engine.as_ref(), &request("t-quiet", "click", json!({ "targetRef": "@s5:e1" }))));
        assert_eq!(code, None);
        assert_eq!(data["value"]["verified"], false);
    }

    #[test]
    fn the_helpers_delivery_decides_whether_a_failure_may_have_changed_anything() {
        let untouched = parse_envelope(r#"{"ok":false,"error":{"code":"TIMEOUT","message":"x","disposition":{"delivery":"not_delivered","retry":"safe"}}}"#).unwrap();
        assert_eq!(contract_error(&untouched).code, "DESKTOP_UNAVAILABLE");
        let uncertain = parse_envelope(r#"{"ok":false,"error":{"code":"TIMEOUT","message":"x","disposition":{"delivery":"unknown","retry":"unknown"}}}"#).unwrap();
        assert_eq!(contract_error(&uncertain).code, "COMMAND_TIMEOUT");
        let stale = parse_envelope(r#"{"ok":false,"error":{"code":"STALE_REF","message":"x","disposition":{"delivery":"not_delivered","retry":"safe"}}}"#).unwrap();
        assert_eq!(contract_error(&stale).code, "DESKTOP_STALE_OBSERVATION");
        let permission = parse_envelope(r#"{"ok":false,"error":{"code":"PERM_DENIED","message":"x"}}"#).unwrap();
        assert_eq!(contract_error(&permission).code, "ACCESSIBILITY_PERMISSION_REQUIRED");
    }

    #[test]
    fn a_password_field_is_never_typed_into_or_clicked() {
        let engine = Script::new(vec![displays(), (vec!["snapshot"], screen("s6", "7"))]);
        control(engine.as_ref(), &request("t-secure", "observe", json!({}))).unwrap();
        let typed = result(control(engine.as_ref(), &request("t-secure", "type", json!({ "targetRef": "@s6:e2", "text": "x" }))));
        assert_eq!(typed.1.as_deref(), Some("DESKTOP_SECURE_FIELD_BLOCKED"));
    }

    #[test]
    fn a_missing_helper_is_reported_as_unavailable_not_as_a_failed_click() {
        struct Missing;
        impl Engine for Missing {
            fn call(&self, _: &[String]) -> std::result::Result<Answer, String> {
                Err("helper missing".into())
            }
        }
        let (_, code) = result(control(&Missing, &request("t-missing", "observe", json!({}))));
        assert_eq!(code.as_deref(), Some("DESKTOP_ADAPTER_UNAVAILABLE"));
    }

    #[test]
    fn steps_stop_when_one_cannot_be_seen_to_work() {
        let engine = Script::new(vec![
            displays(),
            (vec!["snapshot"], screen("s7", "7")),
            (vec!["click"], json!({ "ok": true, "data": { "action": "click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } })),
            (vec!["snapshot"], screen("s7", "7")),
        ]);
        control(engine.as_ref(), &request("t-steps", "observe", json!({}))).unwrap();
        let steps = json!({ "steps": [{ "action": "click", "targetRef": "@s7:e1" }, { "action": "click", "targetRef": "@s7:e1" }] });
        let (data, code) = result(control(engine.as_ref(), &request("t-steps", "runSteps", steps)));
        assert_eq!(code.as_deref(), Some("DESKTOP_TYPE_UNVERIFIED"));
        assert_eq!(data["value"]["evidence"].as_array().unwrap().len(), 1, "the second click was not sent");
    }

    /// Against the real helper on this computer: `WIQO_AGENT_DESKTOP=<path> WIQO_LIVE_APP=<app> cargo test live -- --ignored`.
    #[test]
    #[ignore]
    fn live_observation_names_real_controls() {
        let (Some(app), Some(_)) = (std::env::var_os("WIQO_LIVE_APP"), std::env::var_os("WIQO_AGENT_DESKTOP")) else { return };
        let engine = Helper::locate();
        let (data, code) = result(control(&engine, &request("t-live", "observe", json!({ "text": app.to_string_lossy() }))));
        assert_eq!(code, None, "{data}");
        let targets = data["value"]["targets"].as_array().unwrap();
        assert!(!targets.is_empty());
        println!("{} targets, first: {}", targets.len(), targets[0]);
    }
}
