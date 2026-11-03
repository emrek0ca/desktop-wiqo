//! Driving another app's controls (`computerDesktop`): read what is on screen, then click, type, scroll or press keys on a control by its ref.
//!
//! The work is done by the `agent-desktop` helper program (Apache-2.0, the operating system's accessibility tree, no pictures of the screen). This module only
//! asks it, and turns its answers into the contract's words: the same targets, the same error codes, and a plain `verified` that is true only when the effect
//! was read back. One rule decides whether a failed step may be repeated: the helper's own `delivery` (did the input reach the app), never the wording.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wiqo_core::engine::{Context, Lane, Operation, Outcome};

use crate::op::{data, outcome, payload, permission, tool_failure, Done, Result};
use crate::Op;

/// An observation older than this is not acted on; the screen has probably changed.
const OBSERVATION_LIFETIME: Duration = Duration::from_secs(30);
const MAX_TARGETS: usize = 120;
const MAX_STEPS: usize = 12;
const CALL_TIMEOUT: Duration = Duration::from_secs(25);
/// How long a step waits to see its effect. A deadline, not a number of looks: one look takes seconds on a heavy window, and seven of them held a task for 47 s.
const SETTLE_FOR: Duration = Duration::from_millis(1500);

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
    normalized_x: Option<f64>,
    normalized_y: Option<f64>,
    direction: Option<String>,
    amount: Option<u32>,
    text: Option<String>,
    keys: Option<Vec<String>>,
    steps: Option<Vec<Request>>,
    /// Where a drag ends when it does not end on a control.
    to_normalized_x: Option<f64>,
    to_normalized_y: Option<f64>,
    /// The person's own finger on the phone's live screen. Honoured only on a phone's signature: the relay removes it from anything the server sends.
    person: bool,
    /// The person said yes on a card and the relay checked their signature. The relay alone sets this.
    card_approved: bool,
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

    /// Brings the app to the front and says whether the system confirmed it.
    fn bring_to_front(&self, application: &str) -> bool {
        crate::apps::bring_to_front(application)
    }

    /// The app in front, when the system can say.
    fn frontmost(&self) -> Option<String> {
        None
    }

    /// Seconds since the keyboard or mouse was last touched; `None` where the system cannot say.
    fn seconds_since_input(&self) -> Option<f64> {
        None
    }
}

struct Helper {
    path: Option<PathBuf>,
}

impl Helper {
    /// `WIQO_AGENT_DESKTOP`, else the program shipped next to Wiqo.
    fn locate() -> Helper {
        let name = if cfg!(windows) {
            "agent-desktop.exe"
        } else {
            "agent-desktop"
        };
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
                Ok(None) if started.elapsed() < CALL_TIMEOUT => {
                    std::thread::sleep(Duration::from_millis(25))
                }
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

    fn seconds_since_input(&self) -> Option<f64> {
        seconds_since_input()
    }

    fn frontmost(&self) -> Option<String> {
        crate::apps::frontmost()
    }
}

#[cfg(target_os = "macos")]
fn seconds_since_input() -> Option<f64> {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event: u32) -> f64;
    }
    const HID_SYSTEM_STATE: i32 = 1;
    const ANY_INPUT_EVENT: u32 = u32::MAX;
    Some(unsafe { CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, ANY_INPUT_EVENT) })
}

#[cfg(windows)]
fn seconds_since_input() -> Option<f64> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return None;
    }
    Some(unsafe { GetTickCount() }.wrapping_sub(info.dwTime) as f64 / 1000.0)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn seconds_since_input() -> Option<f64> {
    None
}

fn parse_envelope(text: &str) -> std::result::Result<Answer, String> {
    let envelope: Value =
        serde_json::from_str(text.trim()).map_err(|_| "unreadable answer".to_string())?;
    let ok = envelope["ok"].as_bool().unwrap_or(false);
    let delivery_of = |value: &Value| {
        value["disposition"]["delivery"]
            .as_str()
            .map(str::to_string)
    };
    if ok {
        let data = envelope["data"].clone();
        let delivery = delivery_of(&data);
        Ok(Answer {
            ok,
            data,
            code: String::new(),
            message: String::new(),
            delivery,
        })
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
        "ACTION_NOT_SUPPORTED" | "PLATFORM_NOT_SUPPORTED" | "POLICY_DENIED" => {
            ("DESKTOP_ACTION_UNSUPPORTED", "tool_failure")
        }
        "INVALID_ARGS" => ("DESKTOP_ACTION_INVALID", "validation"),
        _ if untouched => ("DESKTOP_UNAVAILABLE", "tool_failure"),
        "TIMEOUT" | "APP_UNRESPONSIVE" => ("COMMAND_TIMEOUT", "timeout"),
        _ => ("OUTCOME_UNKNOWN", "tool_failure"),
    };
    // A refusal that says the keys are dangerous is the contract's own hotkey rule.
    let code =
        if answer.code == "ACTION_FAILED" && answer.message.to_lowercase().contains("dangerous") {
            "DESKTOP_HOTKEY_BLOCKED"
        } else {
            code
        };
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
    /// The ref the model sees, in the contract's form (`d-` and 8 hex digits); the server refuses any other shape.
    reference: String,
    /// The helper's own ref (`@<snapshot>:eN`), used only between this module and the helper.
    helper: String,
    role: String,
    name: String,
    frame: [f64; 4],
    actions: Vec<String>,
    enabled: bool,
    secure: bool,
    /// The element that has keyboard focus: where typing without a target goes.
    focused: bool,
    /// What it holds now (a field's text, a slider's position). Never read from a secure field.
    value: String,
    /// Checked, selected, expanded...: an effect that changes neither its name nor its value.
    states: String,
    /// What it is called, without what it holds: a field keeps its ref while text is typed into it.
    identity: String,
}

impl Target {
    fn json(&self) -> Value {
        json!({
            "ref": self.reference, "role": self.role, "name": self.name,
            "frame": { "x": self.frame[0], "y": self.frame[1], "width": self.frame[2], "height": self.frame[3] },
            "actions": self.actions, "enabled": self.enabled, "secure": self.secure,
            "value": if self.value == self.name { "" } else { self.value.as_str() },
        })
    }
}

struct Observation {
    snapshot_id: String,
    application: String,
    window_title: String,
    targets: Vec<Target>,
    /// The display the window is on, in points (x, y, width, height): normalised frames and coordinates are fractions of it.
    screen: [f64; 4],
    /// That display's place in the helper's list, which is what its screenshot asks for.
    display: usize,
    /// The window itself, as fractions of the display.
    window: Option<[f64; 4]>,
}

fn collect(node: &Value, screen: [f64; 4], out: &mut Vec<Target>) {
    if let Some(reference) = node["ref_id"].as_str() {
        let states: Vec<&str> = node["states"]
            .as_array()
            .map(|list| list.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let bounds = &node["bounds"];
        let (x, y, w, h) = (
            bounds["x"].as_f64().unwrap_or(0.0),
            bounds["y"].as_f64().unwrap_or(0.0),
            bounds["width"].as_f64().unwrap_or(0.0),
            bounds["height"].as_f64().unwrap_or(0.0),
        );
        let actions: Vec<String> = node["available_actions"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let name = node["name"]
            .as_str()
            .or_else(|| node["value"].as_str())
            .unwrap_or_default();
        let secure = states.contains(&"secure");
        // Anonymous, action-less, invisible rows are scaffolding, not targets.
        if !actions.is_empty()
            && w > 1.0
            && h > 1.0
            && !states.contains(&"offscreen")
            && out.len() < MAX_TARGETS
        {
            let round = |value: f64| (value * 100_000.0).round() / 100_000.0;
            out.push(Target {
                reference: contract_ref(reference),
                helper: reference.to_string(),
                role: node["role"].as_str().unwrap_or("unknown").to_string(),
                // A secure field's content is never read back to the model.
                name: if secure {
                    String::new()
                } else {
                    name.chars().take(100).collect()
                },
                frame: [
                    round((x - screen[0]) / screen[2]),
                    round((y - screen[1]) / screen[3]),
                    round(w / screen[2]),
                    round(h / screen[3]),
                ],
                actions,
                enabled: !states.contains(&"disabled"),
                secure,
                focused: states.contains(&"focused"),
                value: if secure {
                    String::new()
                } else {
                    node["value"]
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(100)
                        .collect()
                },
                states: states
                    .iter()
                    .filter(|state| **state != "focused")
                    .copied()
                    .collect::<Vec<_>>()
                    .join(","),
                identity: node["name"].as_str().unwrap_or_default().to_string(),
            });
        }
    }
    for child in node["children"].as_array().into_iter().flatten() {
        collect(child, screen, out);
    }
}

/// `d-` and the first 8 hex digits of the helper ref's SHA-256: the shape the server accepts, unique within one observation.
fn contract_ref(helper: &str) -> String {
    use sha2::Digest;
    format!(
        "d-{}",
        &hex::encode(sha2::Sha256::digest(helper.as_bytes()))[..8]
    )
}

/// Refs that outlive one look: a control is its role, its name and which of that kind it is. The helper's own ref changes with every snapshot; this one lets the model
/// keep what it has read, send several steps at once, and be told only what changed.
fn assign_references(targets: &mut [Target]) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for target in targets.iter_mut() {
        let name = if target.identity.is_empty() && target.helper.is_empty() {
            &target.name
        } else {
            &target.identity
        };
        let key = format!("{}|{}", target.role, name);
        let count = seen.entry(key.clone()).or_insert(0);
        target.reference = contract_ref(&format!("{key}|{count}"));
        *count += 1;
    }
}

/// The contract's key names, as the helper spells them.
fn helper_key(key: &str) -> String {
    match key.trim().to_lowercase().as_str() {
        "arrowup" => "up".into(),
        "arrowdown" => "down".into(),
        "arrowleft" => "left".into(),
        "arrowright" => "right".into(),
        other => other.to_string(),
    }
}

/// The displays as the helper lists them, in points, with the primary one's place. The list's order is what its screenshot's `--screen` counts.
fn displays(engine: &dyn Engine) -> (Vec<[f64; 4]>, usize) {
    let listed = engine
        .call(&["list-displays".into()])
        .ok()
        .and_then(|answer| answer.ok.then_some(answer.data));
    let entries = listed
        .as_ref()
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let rects: Vec<[f64; 4]> = entries
        .iter()
        .filter_map(|display| {
            let bounds = &display["bounds"];
            Some([
                bounds["x"].as_f64().unwrap_or(0.0),
                bounds["y"].as_f64().unwrap_or(0.0),
                bounds["width"].as_f64()?.max(1.0),
                bounds["height"].as_f64()?.max(1.0),
            ])
        })
        .collect();
    if rects.is_empty() {
        return (vec![[0.0, 0.0, 1920.0, 1080.0]], 0);
    }
    let primary = entries
        .iter()
        .position(|display| display["is_primary"].as_bool() == Some(true))
        .filter(|place| *place < rects.len())
        .unwrap_or(0);
    (rects, primary)
}

/// The display a window sits on: the one holding its middle.
fn display_of(window: &Value, rects: &[[f64; 4]], primary: usize) -> usize {
    let (Some(x), Some(y), Some(w), Some(h)) = (
        window["x"].as_f64(),
        window["y"].as_f64(),
        window["width"].as_f64(),
        window["height"].as_f64(),
    ) else {
        return primary;
    };
    let (middle_x, middle_y) = (x + w / 2.0, y + h / 2.0);
    rects
        .iter()
        .position(|r| {
            middle_x >= r[0] && middle_x < r[0] + r[2] && middle_y >= r[1] && middle_y < r[1] + r[3]
        })
        .unwrap_or(primary)
}

/// What an open menu, sheet, popover or alert of the app shows. A window snapshot does not include them, and they are what the person acts on next.
fn open_surfaces(engine: &dyn Engine, application: &str, screen: [f64; 4]) -> Vec<Target> {
    let Some(listed) = engine
        .call(&["list-surfaces".into(), "--app".into(), application.into()])
        .ok()
        .and_then(|answer| answer.ok.then_some(answer.data))
    else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    for surface in listed["surfaces"].as_array().into_iter().flatten() {
        let kind = surface
            .as_str()
            .or_else(|| surface["kind"].as_str())
            .or_else(|| surface["type"].as_str())
            .or_else(|| surface["surface"].as_str())
            .unwrap_or_default();
        if !["menu", "sheet", "popover", "alert"].contains(&kind) {
            continue;
        }
        let answer = engine.call(&[
            "snapshot".into(),
            "-i".into(),
            "--compact".into(),
            "--include-bounds".into(),
            "--app".into(),
            application.into(),
            "--surface".into(),
            kind.into(),
        ]);
        if let Some(answer) = answer.ok().filter(|answer| answer.ok) {
            collect(&answer.data["tree"], screen, &mut targets);
        }
    }
    targets
}

/// The words on screen, added when the controls alone say too little of what the window shows (a web page, an Electron or custom-drawn app). Only words inside
/// the observed window, and none a control already names. Local and free; without Screen Recording permission there are simply no words.
fn add_screen_text(engine: &dyn Engine, observation: &mut Observation) {
    if observation.targets.len() >= READ_TEXT_BELOW {
        return;
    }
    let words = read_screen(engine, observation.display);
    let inside = |target: &Target| match observation.window {
        Some([x, y, w, h]) => {
            let (middle_x, middle_y) = (
                target.frame[0] + target.frame[2] / 2.0,
                target.frame[1] + target.frame[3] / 2.0,
            );
            middle_x >= x && middle_x <= x + w && middle_y >= y && middle_y <= y + h
        }
        None => true,
    };
    let fresh: Vec<Target> = words
        .into_iter()
        .filter(|word| inside(word))
        .filter(|word| {
            !observation
                .targets
                .iter()
                .any(|target| target.name == word.name)
        })
        .collect();
    observation.targets.extend(fresh);
}

/// The controls of an app as they are now, in whatever opens over its window first.
fn observe_controls(engine: &dyn Engine, application: Option<&str>) -> Result<Observation> {
    let known = displays(engine);
    observe_on(engine, application, &known)
}

fn observe_on(
    engine: &dyn Engine,
    application: Option<&str>,
    (rects, primary): &(Vec<[f64; 4]>, usize),
) -> Result<Observation> {
    let (rects, primary) = (rects.as_slice(), *primary);
    // The app is always named: the window in front is often Wiqo's own, where the request was typed, and a bare snapshot would read that.
    let named = application
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(crate::apps::frontmost);
    let mut args: Vec<String> = vec![
        "snapshot".into(),
        "-i".into(),
        "--compact".into(),
        "--include-bounds".into(),
    ];
    if let Some(name) = &named {
        args.extend(["--app".into(), name.clone()]);
    }
    let answer = engine.call(&args).map_err(unavailable)?;
    if !answer.ok {
        // An app that is running with no window (TextEdit after its last document closed) is a real place to work: its shortcuts (command+n) still
        // reach it. It is reported as an app with nothing on screen yet, not as an app that is missing.
        if let (Some(name), "WINDOW_NOT_FOUND") = (&named, answer.code.as_str()) {
            return Ok(Observation {
                snapshot_id: format!("none-{}", uuid::Uuid::new_v4()),
                application: name.clone(),
                window_title: String::new(),
                targets: Vec::new(),
                screen: rects[primary],
                display: primary,
                window: None,
            });
        }
        // A window the accessibility tree cannot see (a game, a canvas, a remote desktop) is read from its pixels instead.
        if answer.code == "ACTION_NOT_SUPPORTED" {
            let targets = read_screen(engine, primary);
            if !targets.is_empty() {
                return Ok(Observation {
                    snapshot_id: format!("ocr-{}", uuid::Uuid::new_v4()),
                    application: named.unwrap_or_default(),
                    window_title: String::new(),
                    targets,
                    screen: rects[primary],
                    display: primary,
                    window: None,
                });
            }
        }
        return Err(contract_error(&answer));
    }
    let bounds = &answer.data["tree"]["bounds"];
    let display = display_of(bounds, rects, primary);
    let screen = rects[display];
    let window = bounds["width"].as_f64().map(|width| {
        [
            (bounds["x"].as_f64().unwrap_or(screen[0]) - screen[0]) / screen[2],
            (bounds["y"].as_f64().unwrap_or(screen[1]) - screen[1]) / screen[3],
            width / screen[2],
            bounds["height"].as_f64().unwrap_or(screen[3]) / screen[3],
        ]
    });
    let application = answer.data["app"].as_str().unwrap_or_default().to_string();
    let mut collected = Vec::new();
    collect(&answer.data["tree"], screen, &mut collected);
    let over_window = open_surfaces(engine, named.as_deref().unwrap_or(&application), screen);
    let mut targets = ranked(over_window);
    targets.extend(ranked(collected));
    targets.truncate(MAX_TARGETS);
    assign_references(&mut targets);
    Ok(Observation {
        snapshot_id: answer.data["snapshot_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        application,
        window_title: answer.data["window"]["title"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        targets,
        screen,
        display,
        window,
    })
}

fn observe(engine: &dyn Engine, application: Option<&str>) -> Result<Observation> {
    let mut observation = observe_controls(engine, application)?;
    add_screen_text(engine, &mut observation);
    assign_references(&mut observation.targets);
    Ok(observation)
}

/// What the model reads first: the controls a person acts with (buttons, fields, menus), then the rest. The result travels in a bounded message, and
/// a file dialog's sidebar listed first used to push its "Yeni Belge" button out of it. A cell that only repeats the row beside it, and nameless
/// scaffolding (scroll areas, bars, handles), are left out.
fn ranked(targets: Vec<Target>) -> Vec<Target> {
    let rank = |role: &str| match role {
        "button" | "menubutton" | "popupbutton" | "splitbutton" => 0,
        "textfield" | "textarea" | "searchfield" | "combobox" => 1,
        "checkbox" | "radiobutton" | "switch" | "tab" | "menuitem" | "link" | "slider"
        | "stepper" => 2,
        "text" => 3,
        _ => 4,
    };
    let mut kept: Vec<Target> = Vec::new();
    for target in targets {
        let scaffolding = target.name.trim().is_empty()
            && matches!(
                target.role.as_str(),
                "scrollarea"
                    | "scrollbar"
                    | "handle"
                    | "group"
                    | "splitter"
                    | "ruler"
                    | "browser"
                    | "list"
                    | "outline"
            );
        let repeat = target.role == "cell"
            && kept
                .iter()
                .any(|other| other.name == target.name && other.role != "cell");
        if !scaffolding && !repeat {
            kept.push(target);
        }
    }
    kept.sort_by_key(|target| rank(&target.role));
    kept
}

/// Below this many controls the tree is taken to be blind to the window, and the screen's text is read as well.
/// Fewer controls than this and the window is mostly something the tree does not name, so its words are read too.
const READ_TEXT_BELOW: usize = 25;
const READ_TEXT_FOR: Duration = Duration::from_secs(3);
const MAX_TEXT_TARGETS: usize = 80;

/// The words on the primary screen, each a target the model can click by ref (no helper ref: a click lands on the middle of the word).
fn read_screen(engine: &dyn Engine, display: usize) -> Vec<Target> {
    let path = std::env::temp_dir().join(format!("wiqo-ocr-{}.png", uuid::Uuid::new_v4()));
    let taken = engine
        .call(&[
            "screenshot".into(),
            "--screen".into(),
            display.to_string(),
            path.to_string_lossy().to_string(),
        ])
        .is_ok_and(|answer| answer.ok);
    // Words are a help, never worth holding an action for: a late reading is dropped and the turn goes on with the controls alone.
    let words = if taken {
        let (send, receive) = std::sync::mpsc::channel();
        let picture = path.clone();
        std::thread::spawn(move || {
            let _ = send.send(crate::ocr::read(&picture));
            let _ = std::fs::remove_file(&picture);
        });
        receive.recv_timeout(READ_TEXT_FOR).unwrap_or_default()
    } else {
        Vec::new()
    };
    words
        .into_iter()
        .take(MAX_TEXT_TARGETS)
        .map(|word| {
            let round = |value: f64| (value * 100_000.0).round() / 100_000.0;
            Target {
                reference: contract_ref(&format!("ocr:{}:{:?}", word.text, word.frame)),
                helper: String::new(),
                role: "text".into(),
                name: word.text,
                frame: word.frame.map(round),
                actions: vec!["Click".into()],
                enabled: true,
                secure: false,
                focused: false,
                value: String::new(),
                states: String::new(),
                identity: String::new(),
            }
        })
        .collect()
}

fn unavailable(reason: String) -> wiqo_core::wire::WireError {
    if reason == "helper missing" {
        tool_failure(
            "DESKTOP_ADAPTER_UNAVAILABLE",
            "Masaüstü kontrol bileşeni bu Wiqo sürümünde yok.",
        )
    } else if reason == "timeout" {
        wiqo_core::wire::WireError::new(
            "COMMAND_TIMEOUT",
            "Masaüstü işlemi zamanında bitmedi; sonucu doğrulanamadı.",
            "timeout",
        )
    } else {
        tool_failure(
            "DESKTOP_UNAVAILABLE",
            "Masaüstü kontrol bileşeni çalıştırılamadı.",
        )
    }
}

// ---------- what a task last saw ----------

struct Seen {
    snapshot_id: String,
    at: Instant,
    scene: Scene,
}

/// What the latest observation showed: the controls, the app they belong to and the screen they sit on.
#[derive(Clone)]
struct Scene {
    targets: Vec<Target>,
    application: String,
    screen: [f64; 4],
    window: Option<[f64; 4]>,
}

fn seen() -> &'static Mutex<HashMap<String, Seen>> {
    static SEEN: OnceLock<Mutex<HashMap<String, Seen>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}

fn remember(task: &str, observation: &Observation) {
    if let Ok(mut map) = seen().lock() {
        map.insert(
            task.to_string(),
            Seen {
                snapshot_id: observation.snapshot_id.clone(),
                at: Instant::now(),
                scene: Scene {
                    targets: observation.targets.clone(),
                    application: observation.application.clone(),
                    screen: observation.screen,
                    window: observation.window,
                },
            },
        );
    }
}

/// The tasks the person stopped. Their next command is refused, whatever it is: the model does not get to look again and carry on.
fn stopped() -> &'static Mutex<HashSet<String>> {
    static STOPPED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    STOPPED.get_or_init(Default::default)
}

/// "Durdur": every task that has touched the screen stops here. A new request is a new task and runs.
pub fn stop_all() {
    stop_tasks(|_| true);
    show_cursor(None, "stop", "Durduruldu");
}

fn stop_tasks(which: impl Fn(&str) -> bool) {
    if let (Ok(mut map), Ok(mut stopped)) = (seen().lock(), stopped().lock()) {
        let tasks: Vec<String> = map.keys().filter(|task| which(task)).cloned().collect();
        for task in tasks {
            map.remove(&task);
            stopped.insert(task);
        }
    }
}

fn refuse_if_stopped(task: &str) -> Result<()> {
    if stopped().lock().is_ok_and(|stopped| stopped.contains(task)) {
        return Err(permission(
            "DESKTOP_STOPPED_BY_PERSON",
            "Bu görevi sen durdurdun; ekrana dokunmuyorum.",
        ));
    }
    Ok(())
}

// ---------- what the person sees: Wiqo's own cursor ----------

/// One move of the cursor Wiqo draws on the screen. `x`, `y` are in the helper's screen points; absent, the cursor stays and only the words change.
#[derive(Debug, Clone, Serialize)]
pub struct CursorEvent {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub action: &'static str,
    pub label: String,
}

type CursorSink = Box<dyn Fn(CursorEvent) + Send + Sync>;
static CURSOR_SINK: OnceLock<CursorSink> = OnceLock::new();
/// The cursor reaches the target before the action lands, so the person sees where it goes.
const CURSOR_TRAVEL: Duration = Duration::from_millis(280);

/// The window that draws the cursor. Without one (the command-line agent, tests) nothing is drawn and nothing waits.
pub fn set_cursor_sink(sink: CursorSink) {
    let _ = CURSOR_SINK.set(sink);
}

fn show_cursor(at: Option<[f64; 2]>, action: &'static str, label: &str) {
    let Some(sink) = CURSOR_SINK.get() else {
        return;
    };
    sink(CursorEvent {
        x: at.map(|point| point[0]),
        y: at.map(|point| point[1]),
        action,
        label: label.to_string(),
    });
    if at.is_some() {
        std::thread::sleep(CURSOR_TRAVEL);
    }
}

fn middle_of(frame: [f64; 4], screen: [f64; 4]) -> [f64; 2] {
    [
        screen[0] + (frame[0] + frame[2] / 2.0) * screen[2],
        screen[1] + (frame[1] + frame[3] / 2.0) * screen[3],
    ]
}

fn named_target<'a>(scene: &'a Scene, reference: Option<&String>) -> Option<&'a Target> {
    let reference = reference?;
    scene
        .targets
        .iter()
        .find(|target| &target.reference == reference)
}

/// The point on screen a fraction of the display names.
fn screen_point(x: Option<f64>, y: Option<f64>, screen: [f64; 4]) -> Option<[f64; 2]> {
    let (x, y) = (x?, y?);
    ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y))
        .then(|| [screen[0] + x * screen[2], screen[1] + y * screen[3]])
}

/// Where the step acts: its control, or the point it names, or (a scroll with neither) the middle of the window.
fn aim(step: &Request, scene: &Scene) -> Option<[f64; 2]> {
    let reference = if step.action == "dragDrop" {
        step.source_ref.as_ref()
    } else {
        step.target_ref.as_ref()
    };
    named_target(scene, reference)
        .map(|target| middle_of(target.frame, scene.screen))
        .or_else(|| screen_point(step.normalized_x, step.normalized_y, scene.screen))
        .or_else(|| {
            (step.action == "scroll")
                .then(|| middle_of(scene.window.unwrap_or([0.0, 0.0, 1.0, 1.0]), scene.screen))
        })
}

fn cursor_words(step: &Request, scene: &Scene) -> (&'static str, String) {
    let name = named_target(scene, step.target_ref.as_ref())
        .or_else(|| named_target(scene, step.source_ref.as_ref()))
        .map(|target| target.name.as_str())
        .unwrap_or_default();
    let (action, doing) = match step.action.as_str() {
        "click" => ("click", "Tıklıyor"),
        "doubleClick" => ("click", "Çift tıklıyor"),
        "tripleClick" => ("click", "Seçiyor"),
        "rightClick" => ("click", "Sağ tıklıyor"),
        "type" | "setValue" => ("type", "Yazıyor"),
        "scroll" => ("move", "Kaydırıyor"),
        "dragDrop" => ("drag", "Sürüklüyor"),
        "hotkey" => ("type", "Kısayol"),
        "menu" => ("type", "Menü"),
        "wait" => ("move", "Bekliyor"),
        _ => ("move", "Gidiyor"),
    };
    let label = if name.is_empty() {
        doing.to_string()
    } else {
        format!("{doing}: {}", name.chars().take(40).collect::<String>())
    };
    (action, label)
}

// ---------- the person comes first ----------

/// While the person's hands are on the keyboard or mouse, Wiqo waits; it carries on when they stop.
const QUIET_FOR: f64 = 1.2;
/// Our own input can read as theirs for a moment after it lands.
const OWN_INPUT_MARGIN: f64 = 0.4;
#[cfg(not(test))]
const LONGEST_WAIT: Duration = Duration::from_secs(15);
#[cfg(test)]
const LONGEST_WAIT: Duration = Duration::from_millis(300);

fn last_action() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(Default::default)
}

/// They touched the keyboard or mouse just now, and it was not our own input landing.
fn person_is_active(idle: f64, since_ours: f64) -> bool {
    idle < QUIET_FOR && idle + OWN_INPUT_MARGIN < since_ours
}

fn wait_for_the_person(engine: &dyn Engine) -> Result<()> {
    let started = Instant::now();
    let mut told = false;
    loop {
        let Some(idle) = engine.seconds_since_input() else {
            return Ok(());
        };
        let since_ours = last_action()
            .lock()
            .ok()
            .and_then(|last| last.map(|at| at.elapsed().as_secs_f64()))
            .unwrap_or(f64::INFINITY);
        if !person_is_active(idle, since_ours) {
            return Ok(());
        }
        if started.elapsed() >= LONGEST_WAIT {
            return Err(tool_failure(
                "DESKTOP_PERSON_ACTIVE",
                "Bilgisayarı sen kullanıyorsun; araya girmedim. Bırakınca devam edebilirim.",
            ));
        }
        if !told {
            told = true;
            show_cursor(None, "wait", "Sen kullanıyorsun, bekliyorum");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

// ---------- Wiqo's own windows are the person's ----------

fn is_wiqo(application: &str) -> bool {
    let name = fold(application.trim());
    name == "wiqo" || name == "wiqo-desktop" || name == "wiqo desktop"
}

fn own_window_refused() -> wiqo_core::wire::WireError {
    permission(
        "DESKTOP_SELF_OBSERVATION_UNAVAILABLE",
        "Wiqo'nun kendi penceresine dokunulmaz: izin kartı orada. Hangi uygulamada çalışılacağını application ile söyle.",
    )
}

/// The agent never drives the window that holds its own approval card: it could say yes to itself. A click on a point lands on whatever is in front, so the target
/// app is brought forward first, and the click is refused if Wiqo is still there.
fn keep_off_own_windows(engine: &dyn Engine, step: &Request, scene: &Scene) -> Result<()> {
    if is_wiqo(&scene.application) {
        return Err(own_window_refused());
    }
    let by_point = step.target_ref.is_none()
        && step.source_ref.is_none()
        && matches!(
            step.action.as_str(),
            "click" | "doubleClick" | "tripleClick" | "rightClick" | "scroll" | "dragDrop"
        );
    if by_point {
        engine.bring_to_front(&scene.application);
        if engine.frontmost().is_some_and(|front| is_wiqo(&front)) {
            return Err(own_window_refused());
        }
    }
    Ok(())
}

// ---------- what needs the person's yes ----------

/// A control whose press cannot be taken back, spelled as `fold` leaves it. This is the safety net under the model's judgement, on what the screen really says: a
/// wrong match only costs one card, a missed one is why the card rule is not the only check.
const IRREVERSIBLE_WORDS: &[&str] = &[
    "gonder",
    "gonderin",
    "yolla",
    "send",
    "sil",
    "silin",
    "delete",
    "remove",
    "kaldir",
    "kaldirin",
    "erase",
    "buy",
    "purchase",
    "ode",
    "odeme",
    "odeyin",
    "pay",
    "checkout",
    "yayinla",
    "publish",
    "paylas",
    "share",
    "transfer",
    "havale",
    "eft",
    "bicimlendir",
    "format",
    "sifirla",
    "reset",
    "uninstall",
    "unsubscribe",
    "onayla",
    "confirm",
    "replace",
    "discard",
    "kaydetme",
    "logout",
];
const IRREVERSIBLE_PHRASES: &[&str] = &[
    "satin al",
    "siparis ver",
    "place order",
    "copu bosalt",
    "empty trash",
    "cop kutusuna tasi",
    "move to trash",
    "oturumu kapat",
    "cikis yap",
    "sign out",
    "log out",
    "hesabi kapat",
    "aboneligi iptal",
    "uzerine yaz",
    "dont save",
    "don't save",
];
/// Longer than this it is a sentence on the screen, not a control's name.
const LONGEST_CONTROL_NAME: usize = 40;

fn fold(text: &str) -> String {
    text.chars()
        .flat_map(|letter| match letter {
            'İ' | 'I' | 'ı' => vec!['i'],
            'ş' | 'Ş' => vec!['s'],
            'ğ' | 'Ğ' => vec!['g'],
            'ü' | 'Ü' => vec!['u'],
            'ö' | 'Ö' => vec!['o'],
            'ç' | 'Ç' => vec!['c'],
            other => other.to_lowercase().collect(),
        })
        .collect()
}

fn cannot_be_taken_back(name: &str) -> bool {
    if name.chars().count() > LONGEST_CONTROL_NAME {
        return false;
    }
    let folded = fold(name);
    let words: Vec<&str> = folded
        .split(|letter: char| !letter.is_alphanumeric() && letter != '\'')
        .filter(|word| !word.is_empty())
        .collect();
    let joined = words.join(" ");
    words.iter().any(|word| IRREVERSIBLE_WORDS.contains(word))
        || IRREVERSIBLE_PHRASES
            .iter()
            .any(|phrase| format!(" {joined} ").contains(&format!(" {phrase} ")))
}

/// The smallest control under a point: what a click there lands on.
fn control_at(point: [f64; 2], scene: &Scene) -> Option<&Target> {
    let [x, y, w, h] = scene.screen;
    let (px, py) = ((point[0] - x) / w, (point[1] - y) / h);
    scene
        .targets
        .iter()
        .filter(|target| {
            let [fx, fy, fw, fh] = target.frame;
            px >= fx && px <= fx + fw && py >= fy && py <= fy + fh
        })
        .min_by(|a, b| {
            (a.frame[2] * a.frame[3])
                .partial_cmp(&(b.frame[2] * b.frame[3]))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// The command a menu path ends in: "Dosya > Kaydet" is "Kaydet".
fn menu_item(step: &Request) -> Option<&str> {
    step.text
        .as_deref()?
        .rsplit('>')
        .next()
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

/// What a finished step did, in words a person reads. Typed text is not repeated: this line is kept on the server.
fn done_words(step: &Request, scene: &Scene) -> String {
    let name = named_target(scene, step.target_ref.as_ref())
        .or_else(|| named_target(scene, step.source_ref.as_ref()))
        .map(|target| target.name.chars().take(40).collect::<String>())
        .filter(|name| !name.is_empty());
    let on = |verb: &str| match &name {
        Some(name) => format!("“{name}” {verb}"),
        None => format!("Ekranda {verb}"),
    };
    match step.action.as_str() {
        "click" | "doubleClick" | "rightClick" => on("tıklandı"),
        "tripleClick" => on("seçildi"),
        "type" => "Yazı yazıldı".to_string(),
        "setValue" => on("değeri ayarlandı"),
        "scroll" => on("kaydırıldı"),
        "dragDrop" => on("sürüklendi"),
        "move" => on("imleç götürüldü"),
        "menu" => format!(
            "“{}” menü komutu çalıştırıldı",
            menu_item(step).unwrap_or_default()
        ),
        "hotkey" => format!(
            "{} kısayolu basıldı",
            step.keys.as_deref().unwrap_or_default().join("+")
        ),
        _ => "Beklendi".to_string(),
    }
}

/// Shortcuts that send or delete at once.
fn irreversible_keys(keys: &[String]) -> bool {
    let has = |name: &str| keys.iter().any(|key| key.eq_ignore_ascii_case(name));
    let enter = has("enter") || has("return");
    let delete = has("delete") || has("backspace");
    ((has("command") || has("control")) && enter) || ((has("command") || has("shift")) && delete)
}

/// The sentence on the person's card when a step needs their yes. Nothing has been sent when this is said.
fn needs_a_yes(step: &Request, scene: &Scene) -> Option<String> {
    match step.action.as_str() {
        "click" | "doubleClick" | "tripleClick" => {
            let target = named_target(scene, step.target_ref.as_ref()).or_else(|| {
                screen_point(step.normalized_x, step.normalized_y, scene.screen)
                    .and_then(|point| control_at(point, scene))
            })?;
            cannot_be_taken_back(&target.name).then(|| {
                format!(
                    "{} uygulamasında “{}” düğmesine basılacak.",
                    scene.application, target.name
                )
            })
        }
        "menu" => {
            let item = menu_item(step)?;
            cannot_be_taken_back(item).then(|| {
                format!(
                    "{} uygulamasında “{item}” menü komutu çalıştırılacak.",
                    scene.application
                )
            })
        }
        "hotkey" => step
            .keys
            .as_deref()
            .filter(|keys| irreversible_keys(keys))
            .map(|keys| {
                format!(
                    "{} uygulamasında {} tuşlarına basılacak.",
                    scene.application,
                    keys.join("+")
                )
            }),
        _ => None,
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
        outcome(result.unwrap_or_else(|_| {
            Err(tool_failure(
                "DESKTOP_UNAVAILABLE",
                "Masaüstü işlemi tamamlanamadı.",
            ))
        }))
    }
}

/// While Wiqo works on the screen, the screen stays on: a sleeping display has no windows to read and no controls to press, and the person is often
/// away. Each step wakes the display and holds it awake for five more minutes (macOS `caffeinate`, Windows `SetThreadExecutionState`).
fn stay_awake() {
    #[cfg(target_os = "macos")]
    {
        static HOLD: Mutex<Option<std::process::Child>> = Mutex::new(None);
        if let Ok(mut hold) = HOLD.lock() {
            if let Some(mut previous) = hold.take() {
                let _ = previous.kill();
                let _ = previous.wait();
            }
            *hold = Command::new("/usr/bin/caffeinate")
                .args(["-du", "-t", "300"])
                .spawn()
                .ok();
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Power::{
            SetThreadExecutionState, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
        };
        // One-shot: resets the idle timers now, so the display wakes and the system does not sleep under the step.
        unsafe { SetThreadExecutionState(ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED) };
    }
}

fn control(engine: &dyn Engine, value: &Value) -> Result<Done> {
    let request: Request = payload(value, "computerDesktop", "masaüstü")?;
    if request.task_id.trim().is_empty() {
        return Err(tool_failure(
            "DESKTOP_TASK_INVALID",
            "Masaüstü görevi geçersiz.",
        ));
    }
    if !request.person {
        refuse_if_stopped(&request.task_id)?;
    }
    if request.action != "cancel" {
        stay_awake();
    }
    match request.action.as_str() {
        "cancel" => {
            if let Ok(mut map) = seen().lock() {
                map.remove(&request.task_id);
            }
            Ok(Done::new(
                "Masaüstü görevi durduruldu.",
                Some(content(&request, None, true, false, Vec::new())),
            ))
        }
        "observe" => {
            let named = request
                .text
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty());
            if !request.person
                && named
                    .map(str::to_string)
                    .or_else(|| engine.frontmost())
                    .is_some_and(|name| is_wiqo(&name))
            {
                return Err(own_window_refused());
            }
            let observation = observe(engine, request.text.as_deref())?;
            if !request.person && is_wiqo(&observation.application) {
                return Err(own_window_refused());
            }
            remember(&request.task_id, &observation);
            if !request.person {
                let at = observation
                    .window
                    .map(|window| middle_of(window, observation.screen));
                show_cursor(at, "look", "Ekrana bakıyor");
            }
            Ok(Done::new(
                if observation.targets.is_empty() {
                    format!(
                        "{} açık ama penceresi yok; kısayolu (ör. command+n) ona gönderilir.",
                        observation.application
                    )
                } else {
                    format!(
                        "{} uygulamasında {} denetim görüldü.",
                        observation.application,
                        observation.targets.len()
                    )
                },
                Some(content(
                    &request,
                    Some(&observation),
                    true,
                    false,
                    Vec::new(),
                )),
            ))
        }
        "runSteps" => {
            let steps = request.steps.clone().unwrap_or_default();
            if steps.is_empty() || steps.len() > MAX_STEPS {
                return Err(tool_failure(
                    "DESKTOP_STEPS_INVALID",
                    "Adım listesi geçersiz.",
                ));
            }
            run_steps(engine, &request, steps)
        }
        _ => run_steps(engine, &request, vec![request.clone()]),
    }
}

/// Runs the steps in order, and stops at the first one that fails or leaves the result in doubt. After every step the screen is read again, so what the
/// model is told is what the app shows now, not what the helper hoped for.
fn run_steps(engine: &dyn Engine, request: &Request, steps: Vec<Request>) -> Result<Done> {
    let before = current_scene(engine, request)?;
    let start = before.clone();
    let mut previous = before;
    let mut done = Vec::new();
    let mut evidence = Vec::new();
    let mut latest: Option<Observation> = None;
    let mut failure: Option<wiqo_core::wire::WireError> = None;
    for (index, step) in steps.iter().enumerate() {
        // Step by step: the person can stop or take over between any two of them.
        let cleared = if request.person {
            Ok(())
        } else {
            refuse_if_stopped(&request.task_id)
                .and_then(|_| keep_off_own_windows(engine, step, &previous))
                .and_then(|_| wait_for_the_person(engine))
                .and_then(|_| match needs_a_yes(step, &previous) {
                    Some(sentence) if !request.card_approved => {
                        Err(permission("DESKTOP_APPROVAL_REQUIRED", sentence))
                    }
                    _ => Ok(()),
                })
        };
        if let Err(error) = cleared {
            evidence.push(json!({ "index": index, "action": step.action, "targetRef": step.target_ref, "ok": false, "changed": false, "verified": false }));
            failure = Some(error);
            break;
        }
        let (cursor_action, cursor_label) = cursor_words(step, &previous);
        show_cursor(aim(step, &previous), cursor_action, &cursor_label);
        if step.action == "dragDrop" {
            // The real pointer moves for a drag: the cursor goes with it to where it drops.
            let end = named_target(&previous, step.destination_ref.as_ref())
                .map(|target| middle_of(target.frame, previous.screen))
                .or_else(|| {
                    screen_point(step.to_normalized_x, step.to_normalized_y, previous.screen)
                });
            if let (Some(end), Some(sink)) = (end, CURSOR_SINK.get()) {
                sink(CursorEvent {
                    x: Some(end[0]),
                    y: Some(end[1]),
                    action: "drag",
                    label: cursor_label.clone(),
                });
            }
        }
        let mut acted = perform(engine, step, &previous);
        // The helper refused a ref that was already out of date, and nothing was sent: the same control, looked up again.
        if acted
            .as_ref()
            .is_err_and(|error| error.code == "DESKTOP_STALE_OBSERVATION")
        {
            if let Ok(look) = observe_controls(engine, Some(&previous.application)) {
                previous = Scene {
                    targets: look.targets,
                    application: look.application,
                    screen: look.screen,
                    window: look.window,
                };
                acted = perform(engine, step, &previous);
            }
        }
        if let Ok(mut last) = last_action().lock() {
            *last = Some(Instant::now());
        }
        let verdict = match acted {
            Ok(answer) => answer,
            Err(error) => {
                evidence.push(json!({ "index": index, "action": step.action, "targetRef": step.target_ref, "ok": false, "changed": false, "verified": false, "errorCode": error.code }));
                failure = Some(error);
                break;
            }
        };
        let delivery = verdict.delivery.clone().unwrap_or_default();
        // Read the same app again: a step is verified when the helper read its effect back, or when its controls visibly changed. (The app is named:
        // the frontmost window may be Wiqo's own, where the person typed the request.)
        // Apps answer asynchronously (a new window takes a moment to appear): read again for up to 1.5 s until something changed.
        let known = displays(engine);
        let settling = Instant::now();
        let mut after = observe_on(engine, Some(&previous.application), &known).ok();
        while settling.elapsed() < SETTLE_FOR
            && !after
                .as_ref()
                .is_some_and(|after| differs(&previous.targets, &after.targets))
        {
            std::thread::sleep(Duration::from_millis(200));
            after = observe_on(engine, Some(&previous.application), &known).ok();
        }
        let changed = after
            .as_ref()
            .is_some_and(|after| differs(&previous.targets, &after.targets));
        let verified = delivery == "delivered_verified" || (delivery != "not_delivered" && changed);
        done.push(done_words(step, &previous));
        if let Some(after) = after.as_mut() {
            add_screen_text(engine, after);
            assign_references(&mut after.targets);
        }
        evidence.push(json!({ "index": index, "action": step.action, "targetRef": step.target_ref, "ok": true, "changed": changed, "verified": verified }));
        if let Some(after) = after {
            remember(&request.task_id, &after);
            previous = Scene {
                targets: after.targets.clone(),
                application: after.application.clone(),
                screen: after.screen,
                window: after.window,
            };
            latest = Some(after);
        }
        // An input that may have landed and shows no effect is not repeated by the next step: the model looks first.
        if !verified && delivery != "not_delivered" && index + 1 < steps.len() {
            failure = Some(wiqo_core::wire::WireError::new(
                "DESKTOP_TYPE_UNVERIFIED",
                "Adımın etkisi görülemedi; sonraki adımlar çalıştırılmadı.",
                "tool_failure",
            ));
            break;
        }
    }
    let all_verified = failure.is_none()
        && !evidence.is_empty()
        && evidence.iter().all(|entry| entry["verified"] == true);
    let any_changed = evidence.iter().any(|entry| entry["changed"] == true);
    let list = latest
        .as_ref()
        .filter(|latest| !shows_a_new_screen(&start, latest))
        .map(|latest| changes_between(&start.targets, &latest.targets));
    let mut body = content(request, latest.as_ref(), true, any_changed, evidence);
    if let Some((added, changed, removed)) = list {
        // The same screen: only what changed goes to the model, which keeps the list it has. A step costs a few lines instead of the whole window.
        body["value"]["targets"] = json!([]);
        body["value"]["addedTargets"] = json!(added);
        body["value"]["changedTargets"] = json!(changed);
        body["value"]["removedTargetRefs"] = json!(removed);
    }
    let body = with_verified(body, all_verified);
    let so_far = done.join(", ");
    match failure {
        Some(mut error) => {
            if !so_far.is_empty() {
                error.message = format!("Yapılan: {so_far}. Sonra: {}", error.message);
            }
            Ok(Done::partial(error, body))
        }
        None => Ok(Done::new(
            if all_verified {
                format!("{so_far}; ekranda görüldü.")
            } else {
                format!("{so_far}; etkisi ekranda doğrulanamadı.")
            },
            Some(body),
        )),
    }
}

/// Another app, or so much different that the model's list is no use: it gets the whole of the new one.
fn shows_a_new_screen(before: &Scene, after: &Observation) -> bool {
    if before.application != after.application {
        return true;
    }
    let (added, changed, removed) = changes_between(&before.targets, &after.targets);
    let moved = added.len() + changed.len() + removed.len();
    moved > (before.targets.len() / 2).max(8)
}

type TargetChanges = (Vec<Value>, Vec<Value>, Vec<String>);

fn changes_between(before: &[Target], after: &[Target]) -> TargetChanges {
    let was: HashMap<&str, &Target> = before
        .iter()
        .map(|target| (target.reference.as_str(), target))
        .collect();
    let is: HashSet<&str> = after
        .iter()
        .map(|target| target.reference.as_str())
        .collect();
    let mut added = Vec::new();
    let mut changed = Vec::new();
    for target in after {
        match was.get(target.reference.as_str()) {
            None => added.push(target.json()),
            Some(old)
                if old.name != target.name
                    || old.value != target.value
                    || old.enabled != target.enabled
                    || old.states != target.states =>
            {
                changed.push(target.json())
            }
            _ => {}
        }
    }
    let removed = before
        .iter()
        .filter(|target| !is.contains(target.reference.as_str()))
        .map(|target| target.reference.clone())
        .collect();
    (added, changed, removed)
}

fn with_verified(mut body: Value, verified: bool) -> Value {
    body["value"]["verified"] = json!(verified);
    body
}

/// The step's own rules before anything is sent: an observation exists, is recent, and is the one the model named.
/// The screen a step acts on. A look that has grown old, or that the model read before the last one, is simply taken again: the refs survive it, and a control that is
/// gone is refused by name below. Only with no look at all is the model sent back.
fn current_scene(engine: &dyn Engine, request: &Request) -> Result<Scene> {
    let (fresh, scene) = {
        let map = seen()
            .lock()
            .map_err(|_| tool_failure("DESKTOP_UNAVAILABLE", "Masaüstü durumu okunamadı."))?;
        let prior = map.get(&request.task_id).ok_or_else(|| {
            tool_failure(
                "DESKTOP_OBSERVATION_REQUIRED",
                "Önce masaüstünü gözlemleyip güncel hedefleri alın.",
            )
        })?;
        let same = request
            .snapshot_id
            .as_deref()
            .is_none_or(|named| named == prior.snapshot_id);
        (
            same && prior.at.elapsed() <= OBSERVATION_LIFETIME,
            prior.scene.clone(),
        )
    };
    if fresh {
        return Ok(scene);
    }
    let look = observe(engine, Some(&scene.application)).map_err(|_| {
        tool_failure(
            "DESKTOP_STALE_OBSERVATION",
            "Masaüstü gözlemi eskidi; işlemden önce yeniden gözlemleyin.",
        )
    })?;
    remember(&request.task_id, &look);
    Ok(Scene {
        targets: look.targets,
        application: look.application,
        screen: look.screen,
        window: look.window,
    })
}

fn differs(before: &[Target], after: &[Target]) -> bool {
    before.len() != after.len()
        || before.iter().zip(after).any(|(a, b)| {
            a.role != b.role
                || a.name != b.name
                || a.enabled != b.enabled
                || a.value != b.value
                || a.states != b.states
                || a.focused != b.focused
        })
}

/// One step as helper commands. Every ref must be one the model saw in the latest observation; it is turned back into the helper's own ref here.
/// Keys go to the observed app by name, never to whatever window happens to be in front (often Wiqo's own, where the request was typed).
fn perform(engine: &dyn Engine, step: &Request, scene: &Scene) -> Result<Answer> {
    let find = |reference: Option<&String>| -> Result<&Target> {
        let reference = reference.ok_or_else(|| {
            tool_failure(
                "DESKTOP_ACTION_INVALID",
                "Bu işlem için bir hedef gerekiyor.",
            )
        })?;
        named_target(scene, Some(reference)).ok_or_else(|| {
            tool_failure(
                "DESKTOP_TARGET_NOT_FOUND",
                format!("{reference} hedefi güncel görünümde yok."),
            )
        })
    };
    // A point on screen, from the model's 0-1 coordinates: the last resort when a control has no ref.
    let point = || screen_point(step.normalized_x, step.normalized_y, scene.screen);
    let mouse = |at: [f64; 2], count: u8, button: &str| -> Vec<String> {
        let mut args = vec!["mouse-click".to_string()];
        args.extend(coordinates("--xy", at));
        args.extend([
            "--count".into(),
            count.to_string(),
            "--button".into(),
            button.into(),
            "--headed".into(),
        ]);
        args
    };
    let click = |command: &str, count: u8, button: &str| -> Result<Vec<String>> {
        if step.target_ref.is_some() {
            let target = find(step.target_ref.as_ref())?;
            refuse_secure(target)?;
            // A word read from the screen has no helper ref: the click lands on its middle.
            if target.helper.is_empty() {
                return Ok(mouse(middle_of(target.frame, scene.screen), count, button));
            }
            return Ok(match count {
                3 => vec![
                    "triple-click".into(),
                    target.helper.clone(),
                    "--headed".into(),
                ],
                2 => vec![
                    "double-click".into(),
                    target.helper.clone(),
                    "--headed".into(),
                ],
                _ => vec![command.into(), target.helper.clone()],
            });
        }
        let at = point().ok_or_else(|| {
            tool_failure(
                "DESKTOP_TARGET_REQUIRED",
                "Bir hedef ref ya da 0-1 arası koordinat gerekiyor.",
            )
        })?;
        Ok(mouse(at, count, button))
    };
    // One end of a drag: a control the helper knows by ref, otherwise a point (a word read from the screen, a canvas, a slider's track).
    let drag_end = |flag: &str,
                    reference: Option<&String>,
                    x: Option<f64>,
                    y: Option<f64>|
     -> Result<Vec<String>> {
        if reference.is_some() {
            let target = find(reference)?;
            refuse_secure(target)?;
            if !target.helper.is_empty() {
                return Ok(vec![flag.to_string(), target.helper.clone()]);
            }
            return Ok(coordinates(
                &format!("{flag}-xy"),
                middle_of(target.frame, scene.screen),
            ));
        }
        let at = screen_point(x, y, scene.screen).ok_or_else(|| {
            tool_failure(
                "DESKTOP_TARGET_REQUIRED",
                "Sürüklemenin iki ucu için ref ya da 0-1 arası koordinat gerekiyor.",
            )
        })?;
        Ok(coordinates(&format!("{flag}-xy"), at))
    };
    let args: Vec<String> = match step.action.as_str() {
        "click" => click("click", 1, "left")?,
        "doubleClick" => click("click", 2, "left")?,
        "tripleClick" => click("click", 3, "left")?,
        "rightClick" => click("right-click", 1, "right")?,
        "move" => {
            let at = step
                .target_ref
                .is_some()
                .then(|| find(step.target_ref.as_ref()))
                .transpose()?
                .map(|target| middle_of(target.frame, scene.screen))
                .or_else(point)
                .ok_or_else(|| {
                    tool_failure(
                        "DESKTOP_TARGET_REQUIRED",
                        "İmleç için bir hedef ya da 0-1 arası koordinat gerekiyor.",
                    )
                })?;
            let mut args = vec!["mouse-move".to_string()];
            args.extend(coordinates("--xy", at));
            args.push("--headed".into());
            args
        }
        "type" => return type_text(engine, step, scene),
        "setValue" => {
            let target = find(step.target_ref.as_ref())?;
            refuse_secure(target)?;
            let value = step
                .text
                .clone()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Yazılacak değer yok."))?;
            vec!["set-value".into(), target.helper.clone(), value]
        }
        "scroll" => {
            let direction = step.direction.clone().unwrap_or_else(|| "down".into());
            if !["up", "down", "left", "right"].contains(&direction.as_str()) {
                return Err(tool_failure(
                    "DESKTOP_ACTION_INVALID",
                    "Kaydırma yönü geçersiz.",
                ));
            }
            let amount = step.amount.unwrap_or(3).clamp(1, 20);
            let target = step
                .target_ref
                .is_some()
                .then(|| find(step.target_ref.as_ref()))
                .transpose()?;
            match target.filter(|target| !target.helper.is_empty()) {
                Some(target) => vec![
                    "scroll".into(),
                    target.helper.clone(),
                    "--direction".into(),
                    direction,
                    "--amount".into(),
                    amount.to_string(),
                ],
                // No control to name (a web page, a canvas, a word read from the screen): the wheel, where the step aims.
                None => {
                    let at = aim(step, scene).unwrap_or_else(|| {
                        middle_of(scene.window.unwrap_or([0.0, 0.0, 1.0, 1.0]), scene.screen)
                    });
                    let lines = amount as i64;
                    let (dx, dy) = match direction.as_str() {
                        "up" => (0, lines),
                        "down" => (0, -lines),
                        "right" => (lines, 0),
                        _ => (-lines, 0),
                    };
                    vec![
                        "mouse-wheel".into(),
                        format!("--x={}", at[0].round()),
                        format!("--y={}", at[1].round()),
                        format!("--dx={dx}"),
                        format!("--dy={dy}"),
                        "--headed".into(),
                    ]
                }
            }
        }
        "hotkey" => {
            let keys = step
                .keys
                .clone()
                .filter(|keys| !keys.is_empty())
                .ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Kısayol tuşları yok."))?;
            let combo = keys
                .iter()
                .map(|key| helper_key(key))
                .collect::<Vec<_>>()
                .join("+");
            return press(engine, &combo, &scene.application);
        }
        "dragDrop" => {
            let mut args = vec!["drag".to_string()];
            args.extend(drag_end(
                "--from",
                step.source_ref.as_ref(),
                step.normalized_x,
                step.normalized_y,
            )?);
            args.extend(drag_end(
                "--to",
                step.destination_ref.as_ref(),
                step.to_normalized_x,
                step.to_normalized_y,
            )?);
            args.extend(["--duration".into(), "350".into(), "--headed".into()]);
            args
        }
        "menu" => return run_menu(engine, step, scene),
        // Until the words appear, or for a set time.
        "wait" => match step.text.as_deref().filter(|text| !text.is_empty()) {
            Some(text) => vec![
                "wait".into(),
                "--text".into(),
                text.into(),
                "--app".into(),
                scene.application.clone(),
                "--timeout".into(),
                step.amount.unwrap_or(5000).clamp(50, 15000).to_string(),
            ],
            None => vec![
                "wait".into(),
                step.amount.unwrap_or(500).clamp(50, 5000).to_string(),
            ],
        },
        _ => {
            return Err(tool_failure(
                "DESKTOP_ACTION_UNSUPPORTED",
                "Bu masaüstü işlemi desteklenmiyor.",
            ))
        }
    };
    call(engine, &args)
}

/// A command from the app's menu bar: how an expert reaches what has no button on screen. It works in any app and needs no look at the window.
fn run_menu(engine: &dyn Engine, step: &Request, scene: &Scene) -> Result<Answer> {
    let item = menu_item(step)
        .ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Menü komutunun adı yok."))?;
    engine.bring_to_front(&scene.application);
    let find = |exact: bool| {
        let mut args = vec![
            "find".to_string(),
            "--app".into(),
            scene.application.clone(),
            "--surface".into(),
            "menubar".into(),
            "--role".into(),
            "menuitem".into(),
            "--name".into(),
            item.to_string(),
            "--first".into(),
        ];
        if exact {
            args.push("--exact".into());
        }
        engine.call(&args).ok().filter(|answer| answer.ok)
    };
    let found = find(true).or_else(|| find(false)).ok_or_else(|| {
        tool_failure(
            "DESKTOP_TARGET_NOT_FOUND",
            format!(
                "{} menüsünde “{item}” yok; menü adları uygulamanın dilinde olur.",
                scene.application
            ),
        )
    })?;
    let matched = &found.data["match"];
    let reference = matched["ref_id"]
        .as_str()
        .ok_or_else(|| tool_failure("DESKTOP_TARGET_NOT_FOUND", format!("“{item}” bulunamadı.")))?;
    let disabled = matched["states"]
        .as_array()
        .is_some_and(|states| states.iter().any(|state| state == "disabled"));
    if disabled {
        return Err(tool_failure(
            "DESKTOP_TARGET_NOT_FOUND",
            format!("“{item}” şu an kullanılamıyor (gri)."),
        ));
    }
    call(engine, &["click".into(), reference.to_string()])
}

/// A point as the helper takes it; a negative one (a display left of the primary) must be written with `=`.
fn coordinates(flag: &str, at: [f64; 2]) -> Vec<String> {
    let value = format!("{},{}", at[0].round(), at[1].round());
    if at[0] < 0.0 || at[1] < 0.0 {
        vec![format!("{flag}={value}")]
    } else {
        vec![flag.to_string(), value]
    }
}

/// Keys as a person presses them: the app is brought to the front first (it may have no window yet, like TextEdit with no document), and the keys
/// go only once the system says it is in front, so they never land in another app.
fn press(engine: &dyn Engine, combo: &str, application: &str) -> Result<Answer> {
    if !application.is_empty() && !engine.bring_to_front(application) {
        return Err(tool_failure(
            "DESKTOP_APP_UNAVAILABLE",
            format!("{application} öne getirilemedi; tuşlar başka bir uygulamaya gitmesin diye gönderilmedi."),
        ));
    }
    call(engine, &["press".into(), combo.to_string()])
}

fn call(engine: &dyn Engine, args: &[String]) -> Result<Answer> {
    let answer = engine.call(args).map_err(unavailable)?;
    if answer.ok {
        Ok(answer)
    } else {
        Err(contract_error(&answer))
    }
}

/// Text goes in the way a person would put it there. Into the named field, or the focused one when no field is named, through the accessibility
/// call that the app reads back; a field that refuses it (many web and custom editors) gets the text pasted: put on the clipboard, command/control+V
/// to the app, and the person's own clipboard put back.
fn type_text(engine: &dyn Engine, step: &Request, scene: &Scene) -> Result<Answer> {
    let text = step
        .text
        .clone()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| tool_failure("DESKTOP_ACTION_INVALID", "Yazılacak metin yok."))?;
    let target = match step.target_ref.as_ref() {
        Some(reference) => Some(
            scene
                .targets
                .iter()
                .find(|target| &target.reference == reference)
                .ok_or_else(|| {
                    tool_failure(
                        "DESKTOP_TARGET_NOT_FOUND",
                        format!("{reference} hedefi güncel görünümde yok."),
                    )
                })?,
        ),
        None => scene.targets.iter().find(|target| target.focused),
    };
    if target.is_some_and(|target| target.secure) {
        return Err(tool_failure(
            "DESKTOP_SECURE_FIELD_BLOCKED",
            "Parola alanına yazılmaz; kullanıcı kendisi girmeli.",
        ));
    }
    // A field read from the screen's text: click into it the way a person would, then paste.
    if let Some(target) = target.filter(|target| target.helper.is_empty()) {
        let mut args = vec!["mouse-click".to_string()];
        args.extend(coordinates("--xy", middle_of(target.frame, scene.screen)));
        args.push("--headed".into());
        call(engine, &args)?;
        return paste(engine, &text, &scene.application);
    }
    if let Some(target) = target {
        match call(
            engine,
            &["type".into(), target.helper.clone(), text.clone()],
        ) {
            Ok(answer) => return Ok(answer),
            // Only a refusal that sent nothing falls back to pasting; anything that may have typed is reported as it is.
            Err(error)
                if error.code != "DESKTOP_ACTION_UNSUPPORTED"
                    && error.code != "DESKTOP_UNAVAILABLE" =>
            {
                return Err(error)
            }
            Err(_) => {
                let _ = call(engine, &["focus".into(), target.helper.clone()]);
            }
        }
    }
    paste(engine, &text, &scene.application)
}

fn paste(engine: &dyn Engine, text: &str, application: &str) -> Result<Answer> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|_| tool_failure("DESKTOP_INPUT_UNAVAILABLE", "Pano kullanılamıyor."))?;
    let saved = clipboard.get_text().ok();
    // Text or a picture goes back afterwards; other kinds (files, rich text) cannot be read through this and are lost to the paste.
    let saved_picture = saved
        .is_none()
        .then(|| clipboard.get_image().ok())
        .flatten();
    clipboard
        .set_text(text.to_string())
        .map_err(|_| tool_failure("DESKTOP_INPUT_UNAVAILABLE", "Pano kullanılamıyor."))?;
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let pasted = press(engine, &format!("{modifier}+v"), application);
    // The app reads the clipboard when it handles the keys; give it a moment before the person's own text goes back.
    std::thread::sleep(Duration::from_millis(250));
    if let Some(saved) = saved {
        let _ = clipboard.set_text(saved);
    } else if let Some(picture) = saved_picture {
        let _ = clipboard.set_image(picture);
    }
    pasted
}

fn refuse_secure(target: &Target) -> Result<()> {
    if target.secure {
        return Err(permission(
            "DESKTOP_SECURE_FIELD_BLOCKED",
            "Parola alanıyla etkileşilmez; kullanıcı kendisi yapmalı.",
        ));
    }
    Ok(())
}

fn content(
    request: &Request,
    observation: Option<&Observation>,
    access: bool,
    changed: bool,
    evidence: Vec<Value>,
) -> Value {
    let targets: Vec<Value> = observation
        .map(|observation| observation.targets.iter().map(Target::json).collect())
        .unwrap_or_default();
    data(
        "computerDesktop",
        json!({
            "taskId": request.task_id,
            "action": request.action,
            "snapshotId": observation.map(|observation| observation.snapshot_id.clone()),
            "applicationName": observation.map(|observation| observation.application.clone()),
            "windowTitle": observation.map(|observation| observation.window_title.clone()),
            "targets": targets,
            "addedTargets": [],
            "changedTargets": [],
            "removedTargetRefs": [],
            "accessGranted": access,
            "changed": changed,
            "verified": true,
            "evidence": evidence,
        }),
    )
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
        fn bring_to_front(&self, _: &str) -> bool {
            true
        }

        fn call(&self, args: &[String]) -> std::result::Result<Answer, String> {
            let line = args.join(" ");
            let mut rules = self.0.lock().unwrap();
            let position = rules
                .iter()
                .position(|(words, _)| words.iter().all(|word| line.contains(word)))
                .ok_or_else(|| format!("no rule for {line}"))?;
            let envelope = if rules
                .iter()
                .filter(|(words, _)| words.iter().all(|word| line.contains(word)))
                .count()
                > 1
            {
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
        (
            vec!["list-displays"],
            json!({ "ok": true, "data": [{ "is_primary": true, "bounds": { "width": 1000.0, "height": 500.0 } }] }),
        )
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
            Ok(done) => (
                done.data.unwrap_or(Value::Null),
                done.error.map(|error| error.code),
            ),
            Err(error) => (Value::Null, Some(error.code)),
        }
    }

    #[test]
    fn observing_lists_only_real_controls_with_normalised_frames_and_hides_secure_names() {
        let engine = Script::new(vec![displays(), (vec!["snapshot"], screen("s1", "7"))]);
        let (data, code) = result(control(
            engine.as_ref(),
            &request("t-obs", "observe", json!({})),
        ));
        assert_eq!(code, None);
        let targets = data["value"]["targets"].as_array().unwrap();
        assert_eq!(targets.len(), 2, "the anonymous group is scaffolding");
        // The server accepts only `d-` and 8 hex digits as a desktop ref.
        let shape = regex::Regex::new("^d-[0-9a-f]{8}$").unwrap();
        assert!(targets
            .iter()
            .all(|target| shape.is_match(target["ref"].as_str().unwrap())));
        assert_eq!(targets[0]["frame"]["x"], 0.1);
        assert_eq!(targets[0]["frame"]["width"], 0.04);
        assert_eq!(targets[1]["secure"], true);
        assert_eq!(targets[1]["name"], "");
    }

    #[test]
    fn a_click_needs_a_recent_observation_that_the_model_named() {
        let engine = Script::new(vec![displays(), (vec!["snapshot"], screen("s2", "7"))]);
        let none = result(control(
            engine.as_ref(),
            &request(
                "t-none",
                "click",
                json!({ "targetRef": contract_ref("button|7|0") }),
            ),
        ));
        assert_eq!(none.1.as_deref(), Some("DESKTOP_OBSERVATION_REQUIRED"));
        control(engine.as_ref(), &request("t-snap", "observe", json!({}))).unwrap();
        let wrong = result(control(
            engine.as_ref(),
            &request(
                "t-snap",
                "click",
                json!({ "targetRef": contract_ref("button|7|0"), "snapshotId": "other" }),
            ),
        ));
        // A look the model read before the last one is taken again, not refused: the ref still names the same control.
        assert_ne!(wrong.1.as_deref(), Some("DESKTOP_STALE_OBSERVATION"));
        assert_ne!(wrong.1.as_deref(), Some("DESKTOP_TARGET_NOT_FOUND"));
        let unseen = result(control(
            engine.as_ref(),
            &request(
                "t-snap",
                "click",
                json!({ "targetRef": contract_ref("button|yok|0") }),
            ),
        ));
        assert_eq!(unseen.1.as_deref(), Some("DESKTOP_TARGET_NOT_FOUND"));
    }

    #[test]
    fn a_click_is_verified_by_reading_the_screen_again() {
        let engine = Script::new(vec![
            displays(),
            (vec!["snapshot"], screen("s3", "7")),
            (
                vec!["click", "@s3:e1"],
                json!({ "ok": true, "data": { "action": "click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
            ),
            (vec!["snapshot"], screen("s4", "8")),
        ]);
        control(engine.as_ref(), &request("t-click", "observe", json!({}))).unwrap();
        let (data, code) = result(control(
            engine.as_ref(),
            &request(
                "t-click",
                "click",
                json!({ "targetRef": contract_ref("button|7|0") }),
            ),
        ));
        assert_eq!(code, None);
        assert_eq!(
            data["value"]["verified"], true,
            "the button's name changed on screen"
        );
        assert_eq!(data["value"]["changed"], true);
        assert_eq!(data["value"]["snapshotId"], "s4");
    }

    #[test]
    fn a_click_whose_effect_cannot_be_seen_is_sent_but_not_called_verified() {
        let engine = Script::new(vec![
            displays(),
            (vec!["snapshot"], screen("s5", "7")),
            (
                vec!["click"],
                json!({ "ok": true, "data": { "action": "click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
            ),
            (vec!["snapshot"], screen("s5", "7")),
        ]);
        control(engine.as_ref(), &request("t-quiet", "observe", json!({}))).unwrap();
        let (data, code) = result(control(
            engine.as_ref(),
            &request(
                "t-quiet",
                "click",
                json!({ "targetRef": contract_ref("button|7|0") }),
            ),
        ));
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
        let permission =
            parse_envelope(r#"{"ok":false,"error":{"code":"PERM_DENIED","message":"x"}}"#).unwrap();
        assert_eq!(
            contract_error(&permission).code,
            "ACCESSIBILITY_PERMISSION_REQUIRED"
        );
    }

    #[test]
    fn a_password_field_is_never_typed_into_or_clicked() {
        let engine = Script::new(vec![displays(), (vec!["snapshot"], screen("s6", "7"))]);
        control(engine.as_ref(), &request("t-secure", "observe", json!({}))).unwrap();
        let typed = result(control(
            engine.as_ref(),
            &request(
                "t-secure",
                "type",
                json!({ "targetRef": contract_ref("textfield|Parola|0"), "text": "x" }),
            ),
        ));
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
        let (_, code) = result(control(
            &Missing,
            &request("t-missing", "observe", json!({})),
        ));
        assert_eq!(code.as_deref(), Some("DESKTOP_ADAPTER_UNAVAILABLE"));
    }

    #[test]
    fn steps_stop_when_one_cannot_be_seen_to_work() {
        let engine = Script::new(vec![
            displays(),
            (vec!["snapshot"], screen("s7", "7")),
            (
                vec!["click"],
                json!({ "ok": true, "data": { "action": "click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
            ),
            (vec!["snapshot"], screen("s7", "7")),
        ]);
        control(engine.as_ref(), &request("t-steps", "observe", json!({}))).unwrap();
        let steps = json!({ "steps": [{ "action": "click", "targetRef": contract_ref("button|7|0") }, { "action": "click", "targetRef": contract_ref("button|7|0") }] });
        let (data, code) = result(control(
            engine.as_ref(),
            &request("t-steps", "runSteps", steps),
        ));
        assert_eq!(code.as_deref(), Some("DESKTOP_TYPE_UNVERIFIED"));
        assert_eq!(
            data["value"]["evidence"].as_array().unwrap().len(),
            1,
            "the second click was not sent"
        );
    }

    #[test]
    fn a_shortcut_reaches_the_helper_in_its_own_key_names() {
        let engine = Script::new(vec![
            displays(),
            (vec!["snapshot"], screen("s8", "7")),
            (
                vec!["press", "cmd+n"],
                json!({ "ok": true, "data": { "action": "press", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
            ),
            (
                vec!["press", "up"],
                json!({ "ok": true, "data": { "action": "press", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
            ),
        ]);
        control(engine.as_ref(), &request("t-keys", "observe", json!({}))).unwrap();
        let (_, code) = result(control(
            engine.as_ref(),
            &request("t-keys", "hotkey", json!({ "keys": ["cmd", "n"] })),
        ));
        assert_eq!(code, None);
        let (_, code) = result(control(
            engine.as_ref(),
            &request("t-keys", "hotkey", json!({ "keys": ["arrowup"] })),
        ));
        assert_eq!(code, None, "arrowup is the helper's up");
    }

    fn editor(snapshot: &str, text: &str) -> Value {
        json!({ "ok": true, "data": { "app": "TextEdit", "snapshot_id": snapshot, "window": { "title": "Adsız" }, "tree": {
            "role": "window", "children": [
                { "ref_id": format!("@{snapshot}:e2"), "role": "textfield", "value": text, "states": ["focused"], "available_actions": ["SetValue", "TypeText"], "bounds": { "x": 0.0, "y": 40.0, "width": 800.0, "height": 400.0 } }
            ] } } })
    }

    #[test]
    fn text_without_a_named_field_goes_into_the_focused_one_and_is_read_back() {
        let engine = Script::new(vec![
            displays(),
            (vec!["snapshot"], editor("s9", "")),
            (
                vec!["type", "@s9:e2", "merhaba"],
                json!({ "ok": true, "data": { "action": "type", "disposition": { "delivery": "delivered_verified", "retry": "unsafe" } } }),
            ),
            (vec!["snapshot"], editor("s10", "merhaba")),
        ]);
        control(engine.as_ref(), &request("t-type", "observe", json!({}))).unwrap();
        let (data, code) = result(control(
            engine.as_ref(),
            &request("t-type", "type", json!({ "text": "merhaba" })),
        ));
        assert_eq!(code, None);
        assert_eq!(data["value"]["verified"], true);
        // The same screen: only what changed comes back, the field with its new text.
        assert_eq!(data["value"]["changedTargets"][0]["name"], "merhaba");
    }

    #[test]
    fn a_click_without_a_ref_lands_on_the_point_the_model_named() {
        let engine = Script::new(vec![
            displays(),
            (vec!["snapshot"], screen("s11", "7")),
            (
                vec!["mouse-click", "--xy 500,250", "--headed"],
                json!({ "ok": true, "data": { "action": "mouse_click", "disposition": { "delivery": "delivered_unverified", "retry": "unsafe" } } }),
            ),
            (vec!["snapshot"], screen("s12", "8")),
        ]);
        control(engine.as_ref(), &request("t-point", "observe", json!({}))).unwrap();
        let (data, code) = result(control(
            engine.as_ref(),
            &request(
                "t-point",
                "click",
                json!({ "normalizedX": 0.5, "normalizedY": 0.5 }),
            ),
        ));
        assert_eq!(code, None);
        assert_eq!(data["value"]["verified"], true);
    }

    #[test]
    fn a_dialogs_buttons_come_before_its_sidebar_and_repeats_are_dropped() {
        let target = |role: &str, name: &str| Target {
            reference: contract_ref(name),
            helper: String::new(),
            role: role.into(),
            name: name.into(),
            frame: [0.0; 4],
            actions: vec!["Click".into()],
            enabled: true,
            secure: false,
            focused: false,
            value: String::new(),
            states: String::new(),
            identity: name.to_string(),
        };
        let ranked = ranked(vec![
            target("treeitem", "Desktop"),
            target("cell", "Desktop"),
            target("scrollarea", ""),
            target("textfield", "Ara"),
            target("button", "Yeni Belge"),
        ]);
        let names: Vec<&str> = ranked.iter().map(|target| target.name.as_str()).collect();
        assert_eq!(names, ["Yeni Belge", "Ara", "Desktop"]);
    }

    fn acting_scene(names: &[&str]) -> Scene {
        Scene {
            targets: names
                .iter()
                .enumerate()
                .map(|(index, name)| Target {
                    reference: format!("d-{index}"),
                    helper: format!("@s:e{index}"),
                    role: "button".into(),
                    name: (*name).into(),
                    frame: [0.1 * index as f64, 0.1, 0.05, 0.05],
                    actions: vec!["Click".into()],
                    enabled: true,
                    secure: false,
                    focused: false,
                    value: String::new(),
                    states: String::new(),
                    identity: (*name).into(),
                })
                .collect(),
            application: "Mail".into(),
            screen: [1000.0, 100.0, 2000.0, 1000.0],
            window: Some([0.0, 0.0, 0.5, 0.5]),
        }
    }

    fn step(value: Value) -> Request {
        serde_json::from_value(value).unwrap()
    }

    /// The helper's arguments for one step, as one line.
    fn sent(step: &Request, scene: &Scene) -> String {
        struct Echo(Mutex<String>);
        impl Engine for Echo {
            fn call(&self, args: &[String]) -> std::result::Result<Answer, String> {
                *self.0.lock().unwrap() = args.join(" ");
                Ok(Answer {
                    ok: true,
                    data: json!({}),
                    code: String::new(),
                    message: String::new(),
                    delivery: None,
                })
            }
        }
        let echo = Echo(Mutex::new(String::new()));
        perform(&echo, step, scene)
            .map_err(|error| error.code)
            .unwrap();
        let line = echo.0.lock().unwrap().clone();
        line
    }

    #[test]
    fn a_button_that_cannot_be_taken_back_needs_a_yes_whatever_the_language_or_the_way_to_it() {
        let scene = acting_scene(&["Gönder", "Kaydet", "Satın Al", "Silindir çiz", "Delete"]);
        let click = |target: &str| step(json!({ "action": "click", "targetRef": target }));
        assert!(needs_a_yes(&click("d-0"), &scene)
            .unwrap()
            .contains("“Gönder”"));
        assert!(
            needs_a_yes(&click("d-1"), &scene).is_none(),
            "saving is not"
        );
        assert!(needs_a_yes(&click("d-2"), &scene).is_some());
        assert!(
            needs_a_yes(&click("d-3"), &scene).is_none(),
            "a word that only starts the same"
        );
        assert!(needs_a_yes(&click("d-4"), &scene).is_some());
        // The same button by its point on the screen.
        let by_point = step(json!({ "action": "click", "normalizedX": 0.02, "normalizedY": 0.12 }));
        assert!(needs_a_yes(&by_point, &scene).is_some());
        let keys = |keys: Value| step(json!({ "action": "hotkey", "keys": keys }));
        assert!(needs_a_yes(&keys(json!(["command", "enter"])), &scene).is_some());
        assert!(needs_a_yes(&keys(json!(["command", "delete"])), &scene).is_some());
        assert!(needs_a_yes(&keys(json!(["command", "s"])), &scene).is_none());
        assert!(needs_a_yes(&keys(json!(["enter"])), &scene).is_none());
    }

    #[test]
    fn the_refusal_comes_before_anything_is_sent_and_a_signed_yes_or_the_persons_own_tap_passes() {
        let snapshot = "s-gate";
        let rules = || {
            Script::new(vec![
                displays(),
                (
                    vec!["list-surfaces"],
                    json!({ "ok": true, "data": { "surfaces": [] } }),
                ),
                (vec!["snapshot"], screen(snapshot, "Gönder")),
                (
                    vec!["click"],
                    json!({ "ok": true, "data": { "disposition": { "delivery": "delivered_verified" } } }),
                ),
            ])
        };
        for (task, extra, refused) in [
            ("t-gate-model", json!({}), true),
            ("t-gate-card", json!({ "cardApproved": true }), false),
            ("t-gate-person", json!({ "person": true }), false),
        ] {
            let engine = rules();
            let (data, _) = result(control(
                engine.as_ref(),
                &request(task, "observe", json!({ "text": "Mail" })),
            ));
            let target = data["value"]["targets"][0]["ref"].clone();
            let mut extra = extra;
            extra["targetRef"] = target;
            let (_, code) = result(control(engine.as_ref(), &request(task, "click", extra)));
            assert_eq!(
                code.as_deref() == Some("DESKTOP_APPROVAL_REQUIRED"),
                refused,
                "{task}: {code:?}"
            );
        }
    }

    #[test]
    fn a_stopped_task_does_nothing_more_and_a_new_one_runs() {
        let engine = Script::new(vec![
            displays(),
            (
                vec!["list-surfaces"],
                json!({ "ok": true, "data": { "surfaces": [] } }),
            ),
            (vec!["snapshot"], screen("s-stop", "Tamam")),
        ]);
        let (_, code) = result(control(
            engine.as_ref(),
            &request("t-stopped", "observe", json!({ "text": "Mail" })),
        ));
        assert_eq!(code, None);
        stop_tasks(|task| task == "t-stopped");
        let (_, code) = result(control(
            engine.as_ref(),
            &request("t-stopped", "observe", json!({ "text": "Mail" })),
        ));
        assert_eq!(code.as_deref(), Some("DESKTOP_STOPPED_BY_PERSON"));
        let (_, code) = result(control(
            engine.as_ref(),
            &request("t-after-stop", "observe", json!({ "text": "Mail" })),
        ));
        assert_eq!(code, None);
    }

    #[test]
    fn the_person_is_active_only_when_the_input_is_recent_and_not_our_own() {
        assert!(
            person_is_active(0.1, f64::INFINITY),
            "their hand, before we did anything"
        );
        assert!(
            person_is_active(0.2, 5.0),
            "their hand, long after our last step"
        );
        assert!(!person_is_active(0.2, 0.3), "our own input landing");
        assert!(!person_is_active(5.0, 9.0), "they stopped");
    }

    #[test]
    fn with_nobody_at_the_computer_it_goes_on_at_once() {
        struct Away;
        impl Engine for Away {
            fn call(&self, _: &[String]) -> std::result::Result<Answer, String> {
                Err("unused".into())
            }
            fn seconds_since_input(&self) -> Option<f64> {
                Some(5.0)
            }
        }
        assert!(wait_for_the_person(&Away).is_ok());
    }

    #[test]
    fn an_open_menu_comes_first_and_the_window_is_on_the_display_that_holds_it() {
        let engine = Script::new(vec![
            (
                vec!["list-displays"],
                json!({ "ok": true, "data": [
                    { "bounds": { "x": 0.0, "y": 0.0, "width": 1000.0, "height": 500.0 }, "is_primary": true },
                    { "bounds": { "x": 1000.0, "y": 0.0, "width": 2000.0, "height": 1000.0 }, "is_primary": false }
                ] }),
            ),
            (
                vec!["list-surfaces"],
                json!({ "ok": true, "data": { "surfaces": [{ "kind": "menu" }, { "kind": "window" }] } }),
            ),
            (
                vec!["snapshot", "--surface", "menu"],
                json!({ "ok": true, "data": { "snapshot_id": "s-menu", "tree": { "children": [
                    { "ref_id": "@s-menu:e1", "role": "menuitem", "name": "Kopyala", "available_actions": ["Click"],
                      "bounds": { "x": 1200.0, "y": 100.0, "width": 100.0, "height": 20.0 } }
                ] } } }),
            ),
            (
                vec!["snapshot", "--app"],
                json!({ "ok": true, "data": { "snapshot_id": "s-win", "app": "Notlar", "tree": {
                    "bounds": { "x": 1100.0, "y": 50.0, "width": 800.0, "height": 600.0 },
                    "children": [
                        { "ref_id": "@s-win:e1", "role": "button", "name": "Yeni", "available_actions": ["Click"],
                          "bounds": { "x": 1200.0, "y": 60.0, "width": 40.0, "height": 20.0 } }
                    ] } } }),
            ),
        ]);
        let observation = observe_controls(engine.as_ref(), Some("Notlar")).unwrap();
        assert_eq!(observation.display, 1);
        assert_eq!(observation.targets[0].name, "Kopyala");
        assert_eq!(observation.targets[1].name, "Yeni");
        assert_eq!(
            observation.targets[1].frame[0], 0.1,
            "a fraction of its own display"
        );
    }

    #[test]
    fn the_hand_on_the_mouse_reaches_what_a_ref_cannot() {
        let mut scene = acting_scene(&["Kaydırıcı"]);
        scene.targets.push(Target {
            helper: String::new(),
            reference: "d-word".into(),
            name: "Rapor".into(),
            frame: [0.5, 0.5, 0.1, 0.1],
            ..scene.targets[0].clone()
        });
        // A drag between two points, and from a word on the screen to a control.
        let line = sent(
            &step(
                json!({ "action": "dragDrop", "normalizedX": 0.0, "normalizedY": 0.0, "toNormalizedX": 0.5, "toNormalizedY": 0.5 }),
            ),
            &scene,
        );
        assert!(
            line.contains("--from-xy 1000,100")
                && line.contains("--to-xy 2000,600")
                && line.contains("--headed"),
            "{line}"
        );
        let line = sent(
            &step(json!({ "action": "dragDrop", "sourceRef": "d-word", "destinationRef": "d-0" })),
            &scene,
        );
        assert!(
            line.contains("--from-xy 2100,650") && line.contains("--to @s:e0"),
            "{line}"
        );
        // A scroll with no control is the wheel over the window, in any direction.
        let line = sent(
            &step(json!({ "action": "scroll", "direction": "left", "amount": 2 })),
            &scene,
        );
        assert!(
            line.starts_with("mouse-wheel")
                && line.contains("--dx=-2")
                && line.contains("--x=1500")
                && line.contains("--y=350"),
            "{line}"
        );
        let line = sent(
            &step(json!({ "action": "scroll", "targetRef": "d-word", "direction": "down" })),
            &scene,
        );
        assert!(
            line.starts_with("mouse-wheel") && line.contains("--dy=-3"),
            "{line}"
        );
        assert!(sent(
            &step(json!({ "action": "tripleClick", "targetRef": "d-0" })),
            &scene
        )
        .starts_with("triple-click @s:e0"));
        assert!(sent(
            &step(json!({ "action": "setValue", "targetRef": "d-0", "text": "75" })),
            &scene
        )
        .starts_with("set-value @s:e0 75"));
    }

    #[test]
    fn the_agent_cannot_reach_its_own_approval_card() {
        struct WiqoInFront;
        impl Engine for WiqoInFront {
            fn call(&self, args: &[String]) -> std::result::Result<Answer, String> {
                Err(format!("nothing should reach the helper: {args:?}"))
            }
            fn frontmost(&self) -> Option<String> {
                Some("Wiqo".into())
            }
        }
        // Looking at it, by name or because it is in front.
        for extra in [json!({ "text": "wiqo-desktop" }), json!({})] {
            let (_, code) = result(control(&WiqoInFront, &request("t-self", "observe", extra)));
            assert_eq!(
                code.as_deref(),
                Some("DESKTOP_SELF_OBSERVATION_UNAVAILABLE")
            );
        }
        // A click on a point while it stays in front, whichever app was observed.
        let scene = acting_scene(&["Yeni"]);
        let by_point = step(json!({ "action": "click", "normalizedX": 0.5, "normalizedY": 0.5 }));
        assert!(keep_off_own_windows(&WiqoInFront, &by_point, &scene).is_err());
        let by_ref = step(json!({ "action": "click", "targetRef": "d-0" }));
        assert!(
            keep_off_own_windows(&WiqoInFront, &by_ref, &scene).is_ok(),
            "a named control of the other app is that app's"
        );
        let mut own = acting_scene(&["İzin ver"]);
        own.application = "Wiqo".into();
        assert!(keep_off_own_windows(&WiqoInFront, &by_ref, &own).is_err());
    }

    #[test]
    fn a_ref_stays_the_same_across_looks_and_while_a_field_fills_up() {
        let look = |snapshot: &str, field: &str| {
            let mut out = Vec::new();
            collect(
                &json!({ "children": [
                    { "ref_id": format!("@{snapshot}:e1"), "role": "button", "name": "Tamam", "available_actions": ["Click"], "bounds": { "x": 1.0, "y": 1.0, "width": 9.0, "height": 9.0 } },
                    { "ref_id": format!("@{snapshot}:e2"), "role": "button", "name": "Tamam", "available_actions": ["Click"], "bounds": { "x": 20.0, "y": 1.0, "width": 9.0, "height": 9.0 } },
                    { "ref_id": format!("@{snapshot}:e3"), "role": "textarea", "value": field, "available_actions": ["Click"], "bounds": { "x": 1.0, "y": 20.0, "width": 9.0, "height": 9.0 } }
                ] }),
                [0.0, 0.0, 100.0, 100.0],
                &mut out,
            );
            assign_references(&mut out);
            out
        };
        let (first, second) = (look("s1", ""), look("s2", "merhaba"));
        assert_ne!(
            first[0].reference, first[1].reference,
            "two of the same are two refs"
        );
        for (a, b) in first.iter().zip(&second) {
            assert_eq!(a.reference, b.reference);
        }
        let (added, changed, removed) = changes_between(&first, &second);
        assert!(added.is_empty() && removed.is_empty());
        assert_eq!(changed.len(), 1, "only the field that filled up");
    }

    #[test]
    fn a_menu_command_is_found_clicked_and_refused_when_grey_or_final() {
        let scene = acting_scene(&[]);
        let engine = Script::new(vec![
            (
                vec!["find", "--surface menubar", "Kaydet"],
                json!({ "ok": true, "data": { "match": { "ref_id": "@m:e1", "role": "menuitem", "states": [] } } }),
            ),
            (vec!["click @m:e1"], json!({ "ok": true, "data": {} })),
            (
                vec!["find", "--surface menubar", "Yazdır"],
                json!({ "ok": true, "data": { "match": { "ref_id": "@m:e2", "role": "menuitem", "states": ["disabled"] } } }),
            ),
        ]);
        let menu = |path: &str| step(json!({ "action": "menu", "text": path }));
        assert!(perform(engine.as_ref(), &menu("Dosya > Kaydet"), &scene).is_ok());
        let grey = perform(engine.as_ref(), &menu("Dosya > Yazdır"), &scene)
            .map(|_| ())
            .unwrap_err();
        assert_eq!(grey.code, "DESKTOP_TARGET_NOT_FOUND");
        assert!(
            needs_a_yes(&menu("Dosya > Çöp Kutusunu Boşalt"), &scene).is_none(),
            "the list is by whole phrase"
        );
        assert!(needs_a_yes(&menu("Posta > Gönder"), &scene).is_some());
    }

    #[test]
    fn a_step_says_what_it_did_and_never_what_was_typed() {
        let scene = acting_scene(&["Yeni"]);
        assert_eq!(
            done_words(
                &step(json!({ "action": "click", "targetRef": "d-0" })),
                &scene
            ),
            "“Yeni” tıklandı"
        );
        assert_eq!(
            done_words(
                &step(json!({ "action": "type", "text": "gizli not" })),
                &scene
            ),
            "Yazı yazıldı"
        );
        assert_eq!(
            done_words(
                &step(json!({ "action": "hotkey", "keys": ["command", "n"] })),
                &scene
            ),
            "command+n kısayolu basıldı"
        );
    }

    #[test]
    fn a_change_in_a_value_or_a_state_counts_as_an_effect() {
        let before = acting_scene(&["Ses"]).targets;
        let mut after = before.clone();
        after[0].value = "75".into();
        assert!(differs(&before, &after));
        let mut after = before.clone();
        after[0].states = "checked".into();
        assert!(differs(&before, &after));
        assert!(!differs(&before, &before.clone()));
    }

    /// Against the real helper on this computer: `WIQO_AGENT_DESKTOP=<path> WIQO_LIVE_APP=<app> cargo test live -- --ignored`.
    #[test]
    #[ignore]
    fn live_observation_names_real_controls() {
        let (Some(app), Some(_)) = (
            std::env::var_os("WIQO_LIVE_APP"),
            std::env::var_os("WIQO_AGENT_DESKTOP"),
        ) else {
            return;
        };
        let engine = Helper::locate();
        let (data, code) = result(control(
            &engine,
            &request(
                "t-live",
                "observe",
                json!({ "text": app.to_string_lossy() }),
            ),
        ));
        assert_eq!(code, None, "{data}");
        let targets = data["value"]["targets"].as_array().unwrap();
        assert!(!targets.is_empty());
        println!("{} targets, first: {}", targets.len(), targets[0]);
    }

    /// The task that failed in the chat, against the real helper and TextEdit:
    /// `WIQO_AGENT_DESKTOP=<path> cargo test -p wiqo-ops live_textedit -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_textedit_new_document_and_text() {
        if std::env::var_os("WIQO_AGENT_DESKTOP").is_none() {
            return;
        }
        let engine = Helper::locate();
        let task = "t-live-textedit";
        let (_, code) = result(control(
            &engine,
            &request(task, "observe", json!({ "text": "TextEdit" })),
        ));
        assert_eq!(code, None);
        let (data, code) = result(control(
            &engine,
            &request(task, "hotkey", json!({ "keys": ["command", "n"] })),
        ));
        println!("command+n: {code:?} verified={}", data["value"]["verified"]);
        assert_eq!(code, None);
        let (data, code) = result(control(
            &engine,
            &request(task, "type", json!({ "text": "merhaba" })),
        ));
        println!(
            "type: {code:?} verified={} targets={}",
            data["value"]["verified"], data["value"]["targets"]
        );
        assert_eq!(code, None);
        assert_eq!(data["value"]["verified"], true);
    }
}
