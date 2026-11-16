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
const MAX_FIELDS: usize = 24;
/// The most controls read from one window before the list is cut to `MAX_TARGETS`: what is cut is still known by its ref, and counted for the model.
const READ_MOST: usize = 400;
/// One command has under a minute (the contract's long class). Steps that would start after this are left for the next command, said plainly, instead of
/// the whole command ending as a timeout whose result nobody knows.
const STEPS_BUDGET: Duration = Duration::from_secs(38);
const CALL_TIMEOUT: Duration = Duration::from_secs(25);
/// How long a step waits to see its effect. A deadline, not a number of looks: one look takes seconds on a heavy window, and seven of them held a task for 47 s.
const SETTLE_FOR: Duration = Duration::from_millis(1500);

mod actions;
mod cursor;
mod guard;
mod observe;
mod session;

use actions::*;
pub use cursor::set_cursor_sink;
use cursor::*;
use guard::*;
use observe::*;
pub use session::stop_all;
pub use guard::note_frontmost;
use session::*;

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
    /// A drag: how long the move takes, and how long the pointer rests over where it drops before it lets go (milliseconds).
    duration_ms: Option<u64>,
    hold_ms: Option<u64>,
    /// `fillForm`: what goes into which field.
    fields: Option<Vec<Field>>,
    /// A field by what it is called (a form), or the words an observation is narrowed to.
    label: Option<String>,
    /// The server can look at a picture of the screen for what the controls do not name: one goes with a look that found little, and with every `look`.
    sight: bool,
    /// The person's own finger on the phone's live screen. Honoured only on a phone's signature: the relay removes it from anything the server sends.
    person: bool,
    /// The person said yes on a card and the relay checked their signature. The relay alone sets this.
    card_approved: bool,
}

/// One field of a form: the control by its ref or by what it is called, and what it should hold.
#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
struct Field {
    label: Option<String>,
    target_ref: Option<String>,
    value: String,
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
    /// Controls the window has beyond the ones listed: known by their refs, not sent.
    hidden: Vec<Target>,
}

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
    /// Controls beyond the listed ones: a ref the model got from a narrowed look still names its control.
    hidden: Vec<Target>,
}

impl Scene {
    fn of(observation: &Observation) -> Scene {
        Scene {
            targets: observation.targets.clone(),
            application: observation.application.clone(),
            screen: observation.screen,
            window: observation.window,
            hidden: observation.hidden.clone(),
        }
    }
}

/// One move of the cursor Wiqo draws on the screen. `x`, `y` are in the helper's screen points; absent, the cursor stays and only the words change.
#[derive(Debug, Clone, Serialize)]
pub struct CursorEvent {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub action: &'static str,
    pub label: String,
}

type CursorSink = Box<dyn Fn(CursorEvent) + Send + Sync>;
#[cfg(not(test))]
const LONGEST_WAIT: Duration = Duration::from_secs(15);
#[cfg(test)]
const LONGEST_WAIT: Duration = Duration::from_millis(300);

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
        "observe" | "look" => {
            let named = request
                .text
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty());
            // Wiqo in front because the person typed here: the look goes to the app they used before it, as a person would turn back to it.
            let front_is_wiqo = engine.frontmost().is_some_and(|front| is_wiqo(&front));
            let target = match named {
                Some(name) => Some(name.to_string()),
                None if front_is_wiqo => app_behind_wiqo(),
                None => None,
            };
            if !request.person && (named.is_some_and(is_wiqo) || (named.is_none() && front_is_wiqo && target.is_none())) {
                return Err(own_window_refused());
            }
            let mut observation = observe(engine, target.as_deref())?;
            if !request.person && is_wiqo(&observation.application) {
                return Err(own_window_refused());
            }
            if let Some(words) = request.label.as_deref().map(plain).filter(|words| !words.is_empty()) {
                narrow(&mut observation, &words);
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
                Some({
                    let mut body = content(&request, Some(&observation), true, false, Vec::new());
                    // What the controls do not name (a canvas, a game, icons without words) the server reads from a picture.
                    if request.sight && (request.action == "look" || says_little(&observation)) {
                        if let Some(picture) = picture_of(engine, observation.display) {
                            body["value"]["picture"] = picture;
                        }
                    }
                    body
                }),
            ))
        }
        "fillForm" => {
            let fields = request.fields.clone().unwrap_or_default();
            if fields.is_empty() || fields.len() > MAX_FIELDS || fields.iter().any(|field| field.value.is_empty()) {
                return Err(tool_failure(
                    "DESKTOP_STEPS_INVALID",
                    "Form alanları geçersiz.",
                ));
            }
            let steps = fields
                .into_iter()
                .map(|field| Request {
                    action: "fill".into(),
                    target_ref: field.target_ref,
                    label: field.label,
                    text: Some(field.value),
                    ..Request::default()
                })
                .collect();
            run_steps(engine, &request, steps)
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

/// Runs the steps in order, and stops at the first one that fails or leaves the result in doubt. After a step the screen is read again, so what the
/// model is told is what the app shows now, not what the helper hoped for. Steps left over (the time of one command ran out, or a step showed no effect)
/// are counted, not failed: the model goes on from what the screen shows.
fn run_steps(engine: &dyn Engine, request: &Request, steps: Vec<Request>) -> Result<Done> {
    let began = Instant::now();
    let before = current_scene(engine, request)?;
    let start = before.clone();
    let mut previous = before;
    let mut done = Vec::new();
    let mut evidence = Vec::new();
    let mut latest: Option<Observation> = None;
    let mut failure: Option<wiqo_core::wire::WireError> = None;
    let mut left = 0usize;
    let mut left_because = "";
    // The person's yes on a card is for one press: a second press that cannot be taken back, later in the same list, asks again.
    let mut yes_left = request.card_approved;
    for (index, step) in steps.iter().enumerate() {
        let last = index + 1 == steps.len();
        if index > 0 && began.elapsed() >= STEPS_BUDGET {
            left = steps.len() - index;
            left_because = "bu çağrının süresi doldu";
            break;
        }
        // Step by step: the person can stop or take over between any two of them.
        let cleared = if request.person {
            Ok(())
        } else {
            refuse_if_stopped(&request.task_id)
                .and_then(|_| keep_off_own_windows(engine, step, &previous))
                .and_then(|_| wait_for_the_person(engine))
                .and_then(|_| match needs_a_yes(step, &previous) {
                    Some(sentence) if !yes_left => {
                        Err(permission("DESKTOP_APPROVAL_REQUIRED", sentence))
                    }
                    Some(_) => {
                        yes_left = false;
                        Ok(())
                    }
                    None => Ok(()),
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
                previous = Scene::of(&look);
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
        // The step read its own effect back (a field holds what was typed, the words scrolled into view).
        let read_back = verdict.data["readBack"] == true;
        // A wait or a move of the pointer has no effect to look for.
        let quiet = matches!(step.action.as_str(), "wait" | "move");
        done.push(done_words(step, &previous));
        // A field that holds what was typed needs no look at the whole window before the next field: one look comes after the last.
        if step.action == "fill" && read_back && !last {
            evidence.push(json!({ "index": index, "action": step.action, "targetRef": step.target_ref, "ok": true, "changed": true, "verified": true }));
            continue;
        }
        // Read the same app again: a step is verified when the helper read its effect back, or when its controls visibly changed. (The app is named:
        // the frontmost window may be Wiqo's own, where the person typed the request.)
        // Apps answer asynchronously (a new window takes a moment to appear): read again for up to 1.5 s until something changed. A step that already
        // read its effect back, or has none, is not waited for.
        let known = displays(engine);
        let settled = quiet
            || read_back
            || (delivery == "delivered_verified"
                && matches!(step.action.as_str(), "type" | "setValue" | "fill"));
        let settling = Instant::now();
        let mut after = observe_on(engine, Some(&previous.application), &known).ok();
        while !settled
            && settling.elapsed() < SETTLE_FOR
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
        // A field that was read and does not hold what was typed is not verified by something else on the screen having moved.
        let verified = verdict.data["readBack"] != false
            && (quiet
                || read_back
                || delivery == "delivered_verified"
                || (delivery != "not_delivered" && changed));
        if let Some(after) = after.as_mut() {
            // The words on the screen take seconds to read: they are read for the look the model gets, and when a later step names one of them.
            let names_a_word = |next: &Request| {
                [&next.target_ref, &next.source_ref, &next.destination_ref]
                    .into_iter()
                    .flatten()
                    .any(|reference| !after.targets.iter().chain(&after.hidden).any(|target| &target.reference == reference))
            };
            if last || steps[index + 1..].iter().any(names_a_word) {
                add_screen_text(engine, after);
                assign_references(&mut after.targets);
            }
        }
        evidence.push(json!({ "index": index, "action": step.action, "targetRef": step.target_ref, "ok": true, "changed": changed, "verified": verified }));
        if let Some(after) = after {
            remember(&request.task_id, &after);
            previous = Scene::of(&after);
            latest = Some(after);
        }
        // An input that may have landed and shows no effect is not repeated by the next step: the model looks first. (A form's fields each name their
        // own control, so one that could not be read back does not send the next one astray.)
        if !verified && delivery != "not_delivered" && !last && step.action != "fill" {
            left = steps.len() - index - 1;
            left_because = "son adımın etkisi ekranda görülemedi";
            break;
        }
    }
    // The list ended early with nothing read since the last step: the model still gets what the screen shows now.
    if latest.is_none() && !evidence.is_empty() {
        if let Ok(mut look) = observe_controls(engine, Some(&previous.application)) {
            add_screen_text(engine, &mut look);
            assign_references(&mut look.targets);
            remember(&request.task_id, &look);
            latest = Some(look);
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
    body["value"]["stepsLeft"] = json!(left);
    let so_far = done.join(", ");
    if left > 0 {
        body["value"]["note"] = json!(format!(
            "İlk {} adım yapıldı, kalan {left} adım çalıştırılmadı: {left_because}. Yapılanı tekrarlama; ekranın şimdiki haline göre kalanı gönder.",
            done.len()
        ));
    }
    let body = with_verified(body, all_verified);
    match failure {
        Some(mut error) => {
            if !so_far.is_empty() {
                error.message = format!("Yapılan: {so_far}. Sonra: {}", error.message);
            }
            Ok(Done::partial(error, body))
        }
        None => Ok(Done::new(
            if left > 0 {
                format!("{so_far}; kalan {left} adım çalıştırılmadı ({left_because}).")
            } else if all_verified {
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
    Ok(Scene::of(&look))
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

#[cfg(test)]
mod measure;
#[cfg(test)]
mod tests;
