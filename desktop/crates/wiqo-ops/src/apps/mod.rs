//! Applications: what is installed, what is running, and opening, activating or quitting one by the name a person gives it.
//!
//! The name resolution is the same everywhere (`wiqo_contract::names` + `app-aliases.json`): an exact name wins, a fuzzy one only when it clearly beats the
//! runner-up, otherwise the closest names come back as candidates. Only what is really installed or running can be targeted, so an invented name never
//! reaches the operating system. Every change is observed again before it is reported.

#[cfg(all(unix, not(target_os = "macos")))]
mod linux;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(all(unix, not(target_os = "macos")))]
use linux as sys;
#[cfg(target_os = "macos")]
use mac as sys;
#[cfg(target_os = "windows")]
use windows as sys;

#[cfg(target_os = "macos")]
pub(crate) use mac::open_url_in_frontmost_browser;
#[cfg(target_os = "windows")]
pub(crate) use windows::warm;

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use wiqo_contract::names::{normalize, score};
use wiqo_contract::AppAliases;
use wiqo_core::engine::{Context, Lane, Operation, Outcome};
use wiqo_core::wire::WireError;

use crate::op::{data, error, outcome, payload, tool_failure, Result};
use crate::Op;

/// An application the computer can launch.
#[derive(Debug, Clone)]
pub(crate) struct Installed {
    /// The name the person sees.
    pub name: String,
    /// The file name, often the English name on a localized system.
    pub file_name: String,
    /// The macOS bundle identifier (or the desktop-file id on Linux).
    pub bundle: Option<String>,
    pub path: String,
    /// The store category ("productivity", "developer-tools"), when the application says.
    pub category: Option<String>,
}

impl Installed {
    fn names(&self) -> Vec<String> {
        let mut names = vec![self.name.clone()];
        if normalize(&self.file_name) != normalize(&self.name) {
            names.push(self.file_name.clone());
        }
        if let Some(bundle) = &self.bundle {
            names.extend(AppAliases::get().for_bundle(bundle).iter().cloned());
        }
        for sibling in AppAliases::get().siblings_of(&self.name) {
            if !names.contains(&sibling) {
                names.push(sibling);
            }
        }
        names
    }
}

/// Whether an application with exactly this name (or file name) is installed.
pub(crate) fn is_installed(name: &str) -> bool {
    let wanted = normalize(name);
    sys::installed()
        .iter()
        .any(|app| normalize(&app.name) == wanted || normalize(&app.file_name) == wanted)
}

/// An application with a process running.
#[derive(Debug, Clone)]
pub(crate) struct Running {
    pub name: String,
    pub bundle: Option<String>,
    pub is_active: bool,
    pub pids: Vec<u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub name: String,
    pub bundle: Option<String>,
    /// `None` only for a running process that is not in any application folder.
    pub path: Option<String>,
}

enum Resolution {
    Resolved(Target),
    Ambiguous(Vec<String>),
    NotFound(Vec<String>),
}

fn resolve(target: &str, bundle: Option<&str>) -> Resolution {
    struct Candidate {
        target: Target,
        names: Vec<String>,
    }
    let mut candidates: Vec<(String, Candidate)> = Vec::new();
    fn upsert(candidates: &mut Vec<(String, Candidate)>, key: String, candidate: Candidate) {
        match candidates
            .iter_mut()
            .find(|(existing_key, _)| *existing_key == key)
        {
            Some(existing) => existing.1 = candidate,
            None => candidates.push((key, candidate)),
        }
    }
    for record in sys::installed() {
        let key = record
            .bundle
            .as_deref()
            .map(str::to_lowercase)
            .unwrap_or_else(|| record.path.clone());
        let names = record.names();
        upsert(
            &mut candidates,
            key,
            Candidate {
                target: Target {
                    name: record.name,
                    bundle: record.bundle,
                    path: Some(record.path),
                },
                names,
            },
        );
    }
    for group in sys::running() {
        let key = group
            .bundle
            .as_deref()
            .map(str::to_lowercase)
            .unwrap_or_else(|| format!("running:{}", normalize(&group.name)));
        // The same application seen twice (a running "chrome" next to the installed "Google Chrome", whose names include "chrome"): one candidate.
        let key = if candidates
            .iter()
            .any(|(existing_key, _)| *existing_key == key)
        {
            key
        } else {
            let wanted = normalize(&group.name);
            candidates
                .iter()
                .find(|(_, candidate)| candidate.names.iter().any(|name| normalize(name) == wanted))
                .map(|(existing_key, _)| existing_key.clone())
                .unwrap_or(key)
        };
        let existing = candidates
            .iter()
            .find(|(existing_key, _)| *existing_key == key)
            .map(|(_, candidate)| (candidate.target.clone(), candidate.names.clone()));
        match existing {
            // Prefer the name the system shows right now ("Notlar") and keep every other name.
            Some((target, names)) => {
                let mut all = vec![group.name.clone()];
                all.extend(names);
                upsert(
                    &mut candidates,
                    key,
                    Candidate {
                        target: Target {
                            name: group.name,
                            bundle: target.bundle,
                            path: target.path,
                        },
                        names: all,
                    },
                );
            }
            None => {
                let mut names = vec![group.name.clone()];
                if let Some(bundle) = &group.bundle {
                    names.extend(AppAliases::get().for_bundle(bundle).iter().cloned());
                }
                upsert(
                    &mut candidates,
                    key,
                    Candidate {
                        target: Target {
                            name: group.name,
                            bundle: group.bundle,
                            path: None,
                        },
                        names,
                    },
                );
            }
        }
    }
    if let Some(bundle) = bundle.map(str::trim).filter(|bundle| !bundle.is_empty()) {
        if let Some((_, found)) = candidates
            .iter()
            .find(|(key, _)| *key == bundle.to_lowercase())
        {
            return Resolution::Resolved(found.target.clone());
        }
        // A wrong or invented identifier: fall back to the spoken name.
    }
    let query = normalize(target);
    if query.is_empty() {
        return Resolution::NotFound(vec![]);
    }
    let mut ranked: Vec<(&Candidate, f64)> = candidates
        .iter()
        .map(|(_, candidate)| {
            (
                candidate,
                candidate
                    .names
                    .iter()
                    .map(|name| score(name, &query))
                    .fold(0.0, f64::max),
            )
        })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left.0
                    .target
                    .name
                    .to_lowercase()
                    .cmp(&right.0.target.name.to_lowercase())
            })
    });
    let exact: Vec<&(&Candidate, f64)> = ranked.iter().filter(|item| item.1 >= 1.0).collect();
    if exact.len() == 1 {
        return Resolution::Resolved(exact[0].0.target.clone());
    }
    if exact.len() > 1 {
        return Resolution::Ambiguous(
            exact
                .iter()
                .take(5)
                .map(|item| item.0.target.name.clone())
                .collect(),
        );
    }
    let plausible: Vec<&(&Candidate, f64)> = ranked.iter().filter(|item| item.1 >= 0.5).collect();
    let Some(best) = plausible.first() else {
        return Resolution::NotFound(
            ranked
                .iter()
                .take(3)
                .filter(|item| item.1 >= 0.3)
                .map(|item| item.0.target.name.clone())
                .collect(),
        );
    };
    let runner_up = plausible.get(1).map(|item| item.1).unwrap_or(0.0);
    if (best.1 >= 0.86 && best.1 - runner_up >= 0.08) || (plausible.len() == 1 && best.1 >= 0.7) {
        return Resolution::Resolved(best.0.target.clone());
    }
    Resolution::Ambiguous(
        plausible
            .iter()
            .take(5)
            .map(|item| item.0.target.name.clone())
            .collect(),
    )
}

fn not_found(target: &str, suggestions: &[String]) -> String {
    if suggestions.is_empty() {
        format!("\"{target}\" adında bir uygulama bu bilgisayarda yüklü değil.")
    } else {
        format!(
            "\"{target}\" adında bir uygulama bu bilgisayarda yok. En yakın eşleşmeler: {}.",
            suggestions.join(", ")
        )
    }
}

fn ambiguous(candidates: &[String]) -> String {
    format!(
        "Birden fazla uygulama eşleşti: {}. Hangisi olduğunu adıyla belirt.",
        candidates.join(", ")
    )
}

fn matching_running(name: &str, bundle: Option<&str>) -> Option<Running> {
    let wanted = normalize(name);
    sys::running().into_iter().find(|app| match bundle {
        Some(bundle) => app
            .bundle
            .as_deref()
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(bundle)),
        None => normalize(&app.name) == wanted,
    })
}

async fn wait_for(
    name: &str,
    bundle: Option<&str>,
    running: bool,
    active: bool,
    seconds: u64,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let (name, bundle) = (name.to_string(), bundle.map(str::to_string));
        let found = tokio::task::spawn_blocking(move || matching_running(&name, bundle.as_deref()))
            .await
            .ok()
            .flatten();
        let state_matches = if running {
            found.is_some()
        } else {
            found.is_none()
        };
        if state_matches && (!active || found.as_ref().is_some_and(|app| app.is_active)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(work).await.ok()
}

// One flat constructor for the contract's `ApplicationControlContent`; a struct would only restate its fields.
#[allow(clippy::too_many_arguments)]
fn content(
    action: &str,
    name: &str,
    bundle: Option<&str>,
    before: &str,
    after: &str,
    changed: bool,
    verified: bool,
    requires_force: Option<bool>,
) -> Value {
    let mut value = json!({ "action": action, "resolvedName": name, "stateBefore": before, "stateAfter": after, "changed": changed, "verified": verified, "candidates": [] });
    if let Some(bundle) = bundle {
        value["bundleIdentifier"] = json!(bundle);
    }
    if let Some(force) = requires_force {
        value["requiresForce"] = json!(force);
    }
    value
}

struct Control {
    action: String,
    target: String,
    bundle: Option<String>,
    force: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlPayload {
    action: String,
    target: String,
    #[serde(default)]
    bundle_identifier: Option<String>,
    #[serde(default)]
    force: bool,
}

/// Resolves and executes a human application intent. Every state-changing branch observes the process again before claiming success.
async fn control(request: Control) -> std::result::Result<Value, (Option<Value>, WireError)> {
    let target = request.target.trim().to_string();
    let bundle_hint = request.bundle.clone();
    let hint = bundle_hint.clone();
    let wanted = target.clone();
    let resolution = blocking(move || resolve(&wanted, hint.as_deref()))
        .await
        .ok_or((
            None,
            tool_failure("OUTCOME_UNKNOWN", "Uygulama çözümlenemedi."),
        ))?;
    let application = match resolution {
        Resolution::Ambiguous(names) => {
            let value = content(
                &request.action,
                &target,
                bundle_hint.as_deref(),
                "ambiguous",
                "unchanged",
                false,
                false,
                None,
            );
            let mut with_candidates = value;
            with_candidates["candidates"] = json!(names);
            return Err((
                Some(with_candidates),
                tool_failure("APPLICATION_AMBIGUOUS", ambiguous(&names)),
            ));
        }
        Resolution::NotFound(suggestions) => {
            let value = content(
                &request.action,
                &target,
                bundle_hint.as_deref(),
                "not_installed",
                "unchanged",
                false,
                false,
                None,
            );
            return Err((
                Some(value),
                tool_failure("APPLICATION_NOT_FOUND", not_found(&target, &suggestions)),
            ));
        }
        Resolution::Resolved(application) => application,
    };
    let (name, bundle) = (application.name.clone(), application.bundle.clone());
    let running_now = {
        let (n, b) = (name.clone(), bundle.clone());
        blocking(move || matching_running(&n, b.as_deref()))
            .await
            .flatten()
    };
    let bundle_ref = bundle.as_deref();

    match request.action.as_str() {
        "quit" => {
            if let Some(refusal) = sys::protected(&application) {
                return Err((None, refusal));
            }
            let Some(running) = running_now else {
                return Ok(content(
                    "quit",
                    &name,
                    bundle_ref,
                    "not_running",
                    "not_running",
                    false,
                    true,
                    None,
                ));
            };
            let _ = {
                let running = running.clone();
                blocking(move || sys::terminate(&running)).await
            };
            let mut closed = wait_for(&name, bundle_ref, false, false, 3).await;
            if !closed && request.force {
                if let Some(still) = {
                    let (n, b) = (name.clone(), bundle.clone());
                    blocking(move || matching_running(&n, b.as_deref()))
                        .await
                        .flatten()
                } {
                    let _ = blocking(move || sys::force_terminate(&still)).await;
                }
                closed = wait_for(&name, bundle_ref, false, false, 2).await;
            }
            let value = content(
                "quit",
                &name,
                bundle_ref,
                "running",
                if closed { "not_running" } else { "running" },
                closed,
                closed,
                if !closed && !request.force {
                    Some(true)
                } else {
                    None
                },
            );
            if closed {
                return Ok(value);
            }
            let (code, message) = if request.force {
                ("APPLICATION_FORCE_QUIT_FAILED", format!("{name} kapatılamadı; uygulama yanıt vermiyor veya sistem tarafından korunuyor."))
            } else {
                ("APPLICATION_QUIT_REQUIRES_FORCE", format!("{name} normal biçimde kapanmadı (kaydedilmemiş belge sorusu açık olabilir). Zorla kapatmak için bunu açıkça iste."))
            };
            Err((Some(value), tool_failure(code, message)))
        }
        "new_tab" => {
            let app = application.clone();
            match blocking(move || sys::open_blank_tab(&app)).await {
                Some(Ok(())) => {
                    let verified = wait_for(&name, bundle_ref, true, true, 4).await;
                    let value = content(
                        "new_tab",
                        &name,
                        bundle_ref,
                        if running_now.is_some() {
                            "running"
                        } else {
                            "not_running"
                        },
                        if verified { "active" } else { "unknown" },
                        true,
                        verified,
                        None,
                    );
                    if verified {
                        Ok(value)
                    } else {
                        Err((Some(value), tool_failure("APPLICATION_VERIFICATION_FAILED", format!("{name} içinde yeni sekme istendi ama uygulamanın öne geldiği doğrulanamadı."))))
                    }
                }
                Some(Err(message)) => Err((
                    None,
                    tool_failure("APPLICATION_NEW_TAB_UNSUPPORTED", message),
                )),
                None => Err((
                    None,
                    tool_failure("OUTCOME_UNKNOWN", "İşlem beklenmedik şekilde durdu."),
                )),
            }
        }
        _ => {
            let activate_only = request.action == "activate";
            if let Some(running) = running_now {
                let activated = blocking(move || sys::activate(&running))
                    .await
                    .unwrap_or(false);
                let mut verified = wait_for(&name, bundle_ref, true, true, 2).await;
                if !verified && application.path.is_some() {
                    // An app with no open window ignores activate(); launching it again brings a window back the way clicking its icon does.
                    let app = application.clone();
                    let _ = blocking(move || sys::launch(&app)).await;
                    verified = wait_for(&name, bundle_ref, true, true, 2).await;
                }
                let value = content(
                    &request.action,
                    &name,
                    bundle_ref,
                    "running",
                    if verified { "active" } else { "running" },
                    activated,
                    verified,
                    None,
                );
                return if verified {
                    Ok(value)
                } else {
                    Err((
                        Some(value),
                        tool_failure(
                            "APPLICATION_ACTIVATE_FAILED",
                            format!("{name} çalışıyor ancak öne getirildiği doğrulanamadı."),
                        ),
                    ))
                };
            }
            let app = application.clone();
            match blocking(move || sys::launch(&app)).await {
                Some(Ok(())) => {}
                Some(Err(message)) => {
                    return Err((None, tool_failure("APPLICATION_OPEN_FAILED", message)))
                }
                None => {
                    return Err((
                        None,
                        tool_failure("OUTCOME_UNKNOWN", "İşlem beklenmedik şekilde durdu."),
                    ))
                }
            }
            let running = wait_for(&name, bundle_ref, true, false, 6).await;
            let active = if activate_only {
                wait_for(&name, bundle_ref, true, true, 2).await
            } else {
                true
            };
            let verified = running && active;
            let value = content(
                &request.action,
                &name,
                bundle_ref,
                "not_running",
                if verified {
                    if activate_only {
                        "active"
                    } else {
                        "running"
                    }
                } else {
                    "unknown"
                },
                true,
                verified,
                None,
            );
            if verified {
                Ok(value)
            } else {
                Err((
                    Some(value),
                    tool_failure(
                        "APPLICATION_VERIFICATION_FAILED",
                        format!("{name} açılma komutu verildi ama çalıştığı doğrulanamadı."),
                    ),
                ))
            }
        }
    }
}

fn finish_control(
    result: std::result::Result<Value, (Option<Value>, WireError)>,
    success: impl FnOnce(&Value) -> String,
) -> Outcome {
    match result {
        Ok(value) => {
            let message = success(&value);
            Outcome::ok(message, data("applicationControl", value))
        }
        Err((value, error)) => Outcome {
            message: error.message.clone(),
            data: value.map(|value| data("applicationControl", value)),
            error: Some(error),
        },
    }
}

/// `ApplicationID`: the ten applications the first phone apps could name; today they are only a way to say a name.
fn application_id_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "safari" => "Safari",
        "xcode" => "Xcode",
        "terminal" => "Terminal",
        "finder" => "Finder",
        "visualStudioCode" => "Visual Studio Code",
        "mail" => "Mail",
        "music" => "Music",
        "messages" => "Messages",
        "notes" => "Notes",
        "systemSettings" => "System Settings",
        _ => return None,
    })
}

pub(crate) fn operations() -> Vec<Op> {
    vec![
        Arc::new(ListRunning),
        Arc::new(OpenApplication),
        Arc::new(OpenByName),
        Arc::new(ControlApplication),
        Arc::new(QuitApplication),
    ]
}

struct ListRunning;

#[async_trait]
impl Operation for ListRunning {
    fn name(&self) -> &'static str {
        "listRunningApplications"
    }

    fn lane(&self) -> Lane {
        Lane::ReadOnly
    }

    async fn run(&self, _ctx: &Context, _payload: &Value) -> Outcome {
        let mut apps = blocking(sys::running).await.unwrap_or_default();
        apps.sort_by(|left, right| {
            right
                .is_active
                .cmp(&left.is_active)
                .then_with(|| crate::fsx::natural_cmp(&left.name, &right.name))
        });
        let list: Vec<Value> = apps
            .into_iter()
            .map(|app| {
                let mut item = json!({ "name": app.name, "isActive": app.is_active });
                if let Some(bundle) = app.bundle {
                    item["bundleIdentifier"] = json!(bundle);
                }
                item
            })
            .collect();
        Outcome::ok(
            "Çalışan uygulamalar listelendi.",
            data("runningApplications", list),
        )
    }
}

struct OpenApplication;

#[async_trait]
impl Operation for OpenApplication {
    fn name(&self) -> &'static str {
        "openApplication"
    }

    async fn run(&self, _ctx: &Context, value: &Value) -> Outcome {
        let id: Result<String> = payload(value, "application", "uygulama");
        let Some(name) = id.ok().and_then(|id| application_id_name(&id)) else {
            return Outcome::failed(tool_failure("INVALID_PAYLOAD", "Uygulama seçimi geçersiz."));
        };
        let result = control(Control {
            action: "open".into(),
            target: name.into(),
            bundle: None,
            force: false,
        })
        .await;
        finish_control(result, |_| format!("{name} açıldı ve kontrol edildi."))
    }
}

struct OpenByName;

#[async_trait]
impl Operation for OpenByName {
    fn name(&self) -> &'static str {
        "openApplicationByName"
    }

    async fn run(&self, _ctx: &Context, value: &Value) -> Outcome {
        let query: Result<String> = payload(value, "applicationQuery", "uygulama arama");
        let query = match query {
            Ok(query) if !normalize(&query).is_empty() => query.trim().to_string(),
            _ => {
                return Outcome::failed(error(
                    "INVALID_APPLICATION_NAME",
                    "Uygulama adı boş olamaz.",
                    "tool_failure",
                ))
            }
        };
        let result = control(Control {
            action: "open".into(),
            target: query.clone(),
            bundle: None,
            force: false,
        })
        .await;
        match result {
            Ok(value) => {
                let resolved = value["resolvedName"].as_str().unwrap_or(&query).to_string();
                let found = {
                    let name = resolved.clone();
                    blocking(move || sys::installed().into_iter().find(|app| app.name == name))
                        .await
                        .flatten()
                };
                let item = json!([{ "name": resolved, "bundleIdentifier": value["bundleIdentifier"], "path": found.map(|app| app.path).unwrap_or_default() }]);
                Outcome::ok(
                    format!("{query} açıldı."),
                    data("installedApplications", item),
                )
            }
            Err((_, error)) => Outcome::failed(error),
        }
    }
}

struct ControlApplication;

#[async_trait]
impl Operation for ControlApplication {
    fn name(&self) -> &'static str {
        "controlApplication"
    }

    async fn run(&self, _ctx: &Context, value: &Value) -> Outcome {
        let parsed: Result<ControlPayload> =
            payload(value, "applicationControl", "uygulama eylemi");
        let request = match parsed {
            Ok(request)
                if ["open", "activate", "new_tab", "quit"].contains(&request.action.as_str())
                    && !request.target.trim().is_empty() =>
            {
                request
            }
            _ => {
                return Outcome::failed(error(
                    "INVALID_PAYLOAD",
                    "Uygulama eylemi veya hedefi geçersiz.",
                    "validation",
                ))
            }
        };
        let action = request.action.clone();
        let result = control(Control {
            action: request.action,
            target: request.target,
            bundle: request.bundle_identifier,
            force: request.force,
        })
        .await;
        finish_control(result, move |value| {
            let name = value["resolvedName"]
                .as_str()
                .unwrap_or("Uygulama")
                .to_string();
            match action.as_str() {
                "open" => format!("{name} açıldı ve doğrulandı."),
                "activate" => format!("{name} öne getirildi ve doğrulandı."),
                "new_tab" => format!("{name} içinde yeni sekme açıldı; uygulama öne geldi."),
                _ if value["changed"] == false => format!("{name} zaten açık değildi."),
                _ => format!("{name} kapatıldı ve doğrulandı."),
            }
        })
    }
}

struct QuitApplication;

#[async_trait]
impl Operation for QuitApplication {
    fn name(&self) -> &'static str {
        "quitApplication"
    }

    async fn run(&self, _ctx: &Context, value: &Value) -> Outcome {
        let id: Result<String> = payload(value, "application", "uygulama");
        let Some(name) = id.ok().and_then(|id| application_id_name(&id)) else {
            return Outcome::failed(tool_failure(
                "INVALID_PAYLOAD",
                "Kapatılacak uygulama seçimi geçersiz.",
            ));
        };
        // The legacy quit asks politely and, failing that, insists: the person named the application and asked for it to be closed.
        let result = control(Control {
            action: "quit".into(),
            target: name.into(),
            bundle: None,
            force: true,
        })
        .await;
        match result {
            Ok(_) => Outcome {
                message: format!("{name} kapatıldı."),
                data: None,
                error: None,
            },
            Err((_, error)) => outcome(Err(error)),
        }
    }
}

const BROWSERS: [&str; 11] = [
    "com.google.Chrome",
    "com.google.Chrome.beta",
    "com.google.Chrome.canary",
    "com.apple.Safari",
    "com.apple.SafariTechnologyPreview",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "org.mozilla.firefox",
    "company.thebrowser.Browser",
    "com.operasoftware.Opera",
    "com.vivaldi.Vivaldi",
];

/// The applications the server may name in its tools (`applicationTargets` of the registration): running ones first, then installed ones, then the other
/// names of installed ones (an alias row has no bundle id so it survives the server's per-bundle dedupe; this desktop maps it back when it is used).
pub(crate) fn targets() -> Vec<Value> {
    let running = sys::running();
    // Never waits: this runs while the registration and every chat turn are being built.
    let installed = sys::installed_now();
    let running_bundles: std::collections::HashSet<String> = running
        .iter()
        .filter_map(|app| app.bundle.as_deref().map(str::to_lowercase))
        .collect();
    let mut used_keys = std::collections::HashSet::new();
    let mut used_names = std::collections::HashSet::new();
    let mut out: Vec<Value> = Vec::new();
    let mut add = |name: &str, bundle: Option<&str>, is_running: bool, category: Option<&str>| {
        let normalized = normalize(name);
        let key = bundle
            .map(str::to_lowercase)
            .unwrap_or_else(|| format!("name:{normalized}"));
        if normalized.is_empty() || used_keys.contains(&key) || used_names.contains(&normalized) {
            return;
        }
        used_keys.insert(key);
        used_names.insert(normalized);
        let actions: &[&str] = match bundle {
            Some("com.apple.finder") => &["open", "activate"],
            Some(id) if BROWSERS.contains(&id) => &["open", "activate", "new_tab", "quit"],
            _ => &["open", "activate", "quit"],
        };
        out.push(json!({ "name": name, "bundleIdentifier": bundle.unwrap_or(""), "isRunning": is_running, "supportedActions": actions, "category": category.unwrap_or("") }));
    };
    let mut sorted_running = running.clone();
    sorted_running.sort_by(|a, b| crate::fsx::natural_cmp(&a.name, &b.name));
    for app in &sorted_running {
        let category = installed
            .iter()
            .find(|record| record.bundle.is_some() && record.bundle == app.bundle)
            .and_then(|record| record.category.as_deref());
        add(&app.name, app.bundle.as_deref(), true, category);
    }
    for record in &installed {
        let is_running = record
            .bundle
            .as_deref()
            .is_some_and(|bundle| running_bundles.contains(&bundle.to_lowercase()));
        add(
            &record.name,
            record.bundle.as_deref(),
            is_running,
            record.category.as_deref(),
        );
    }
    for record in &installed {
        let Some(bundle) = record.bundle.as_deref() else {
            continue;
        };
        let is_running = running_bundles.contains(&bundle.to_lowercase());
        for alias in AppAliases::get().for_bundle(bundle) {
            add(alias, None, is_running, None);
        }
    }
    out.truncate(200);
    out
}

/// Brings an application to the front and confirms it is there (a screenshot of the wrong window is worse than none).
pub(crate) async fn activate_verified(target: &str) -> Result<()> {
    match control(Control { action: "activate".into(), target: target.into(), bundle: None, force: false }).await {
        Ok(value) if value["verified"] == true => Ok(()),
        Ok(_) => Err(tool_failure("APPLICATION_ACTIVATION_UNVERIFIED", format!("{target} öne getirilemedi; başka bir pencerenin görüntüsünü almamak için işlem durduruldu."))),
        Err((_, error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ten_legacy_application_ids_have_names() {
        for id in [
            "safari",
            "xcode",
            "terminal",
            "finder",
            "visualStudioCode",
            "mail",
            "music",
            "messages",
            "notes",
            "systemSettings",
        ] {
            assert!(application_id_name(id).is_some(), "{id}");
        }
        assert!(application_id_name("nothing").is_none());
    }

    #[test]
    fn control_content_has_the_contract_fields() {
        let value = content(
            "open",
            "Safari",
            Some("com.apple.Safari"),
            "not_running",
            "running",
            true,
            true,
            None,
        );
        for key in [
            "action",
            "resolvedName",
            "bundleIdentifier",
            "stateBefore",
            "stateAfter",
            "changed",
            "verified",
            "candidates",
        ] {
            assert!(value.get(key).is_some(), "{key}");
        }
        assert!(value.get("requiresForce").is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn on_a_mac_finder_is_found_by_its_spoken_name_and_nonsense_is_not() {
        assert!(
            matches!(resolve("Finder'ı", None), Resolution::Resolved(target) if target.name == "Finder")
        );
        assert!(matches!(
            resolve("zzzxxqqq uygulaması", None),
            Resolution::NotFound(_)
        ));
    }
}
