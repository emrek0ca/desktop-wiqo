//! Terminal sessions (terminal-session-v1..v5): the server's agent works in a folder the phone approved — runs commands and plans, starts background
//! jobs it can watch, feed and stop, reads, writes, searches and edits files — and every step is held to the grant (folder, mode, end time), the
//! command policy, and an undo record for file changes.

pub(crate) mod code;
mod edit;
mod policy;
mod profile;
pub(crate) mod shell;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
use wiqo_core::engine::Context;
use wiqo_core::terminal::{Grant, Session, Terminal};
use wiqo_core::wire::{timestamp, CommandResult, CommandStatus, WireError};

pub use edit::undo_recorded;
pub use policy::redact_secrets;
use policy::{READ_WITHOUT_GRANT_ID, SCRATCH_FOLDER};
use shell::{Control, Input, WriteScope};

const FEATURES: [&str; 5] = [
    "terminal-session-v1",
    "terminal-session-v2",
    "terminal-session-v3",
    "terminal-session-v4",
    "terminal-session-v5",
];

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Request {
    action: Option<String>,
    grant: Option<Grant>,
    command: Option<String>,
    working_directory: Option<String>,
    timeout_seconds: Option<u64>,
    job_id: Option<String>,
    interactive: Option<bool>,
    input: Option<String>,
    file_path: Option<String>,
    patch: Option<String>,
    shell: Option<String>,
    steps: Option<Vec<PlanStep>>,
    start_line: Option<usize>,
    end_line: Option<usize>,
    content: Option<String>,
    pattern: Option<String>,
    glob: Option<String>,
    edits: Option<Vec<FileEdit>>,
    replacements: Option<Vec<ReplacementRequest>>,
    undo_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PlanStep {
    command: String,
    continue_on_error: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FileEdit {
    file_path: String,
    patch: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ReplacementRequest {
    file_path: String,
    find: String,
    replace: String,
    replace_all: Option<bool>,
}

/// A background job: its process, its last output, and how it ended.
struct Job {
    grant_id: String,
    working_directory: PathBuf,
    started_at: chrono::DateTime<Utc>,
    state: &'static str,
    message: String,
    error_code: Option<String>,
    output: Option<Value>,
    control: Control,
}

#[derive(Default)]
struct State {
    jobs: HashMap<String, Arc<Mutex<Job>>>,
    /// Where each session's shell last ended (after `cd`), by grant id.
    directories: HashMap<String, PathBuf>,
    /// What each session's shell exported, by grant id; applied before its next command and never sent anywhere.
    exports: HashMap<String, String>,
    /// Answers by command id and request digest: a repeated command is answered, never run again.
    answered: HashMap<String, (String, CommandResult, Instant)>,
    running: HashMap<String, String>,
}

#[derive(Default)]
pub struct Sessions {
    state: Arc<Mutex<State>>,
    profile: Arc<OnceLock<Value>>,
}

impl Sessions {
    pub fn new() -> Arc<Sessions> {
        let sessions = Arc::new(Sessions::default());
        let profile = sessions.profile.clone();
        std::thread::spawn(move || {
            let _ = profile.set(profile::profile());
        });
        sessions
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("terminal lock")
    }
}

fn result(
    status: CommandStatus,
    message: impl Into<String>,
    data: Option<Value>,
    error: Option<WireError>,
    started: chrono::DateTime<Utc>,
) -> CommandResult {
    CommandResult {
        command_id: Uuid::new_v4(),
        status,
        message: message.into(),
        started_at: timestamp(started),
        finished_at: timestamp(Utc::now()),
        data,
        error,
    }
}

fn denied(code: &str, message: impl Into<String>) -> CommandResult {
    let message = message.into();
    result(
        CommandStatus::Denied,
        message.clone(),
        None,
        Some(WireError::new(code, message, "permission")),
        Utc::now(),
    )
}

fn terminal_result(kind: &str, directory: &Path, extra: Value) -> Value {
    let mut value = json!({ "kind": kind, "workingDirectory": directory.to_string_lossy() });
    if let (Some(target), Some(fields)) = (value.as_object_mut(), extra.as_object()) {
        target.extend(fields.clone());
    }
    json!({ "kind": "terminalResult", "value": value })
}

fn finished(
    outcome: Result<(String, Value), WireError>,
    started: chrono::DateTime<Utc>,
) -> CommandResult {
    match outcome {
        Ok((message, data)) => result(CommandStatus::Completed, message, Some(data), None, started),
        Err(error) => result(
            CommandStatus::Failed,
            error.message.clone(),
            None,
            Some(error),
            started,
        ),
    }
}

fn write_scope(grant: &Grant, root: &Path) -> WriteScope {
    if grant.mode == "full" {
        WriteScope::Folder(root.to_path_buf())
    } else {
        WriteScope::ReadOnly
    }
}

fn valid_command(command: &str) -> bool {
    !command.is_empty() && command.chars().count() <= 4_000 && !command.contains('\0')
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(work).await.ok()
}

#[async_trait]
impl Terminal for Sessions {
    fn features(&self) -> Vec<&'static str> {
        FEATURES.to_vec()
    }

    fn profile(&self) -> Option<Value> {
        self.profile.get().cloned()
    }

    async fn handle(
        &self,
        session: Session,
        command_id: &str,
        raw: &Value,
        digest: &str,
    ) -> CommandResult {
        {
            let mut state = self.lock();
            state
                .answered
                .retain(|_, (_, _, at)| at.elapsed() < Duration::from_secs(24 * 3600));
            if let Some((seen, answer, _)) = state.answered.get(command_id) {
                return if seen == digest {
                    answer.clone()
                } else {
                    denied("TERMINAL_GRANT_INVALID", "Terminal isteği doğrulanamadı.")
                };
            }
            if state.running.contains_key(command_id) {
                return denied("TERMINAL_DUPLICATE", "Bu terminal isteği zaten çalışıyor.");
            }
            state
                .running
                .insert(command_id.to_string(), digest.to_string());
        }
        let answer = match serde_json::from_value::<Request>(raw.clone()) {
            Ok(request) => self.dispatch(&session, command_id, request).await,
            Err(_) => denied("TERMINAL_ACTION_INVALID", "Terminal isteği çözülemedi."),
        };
        let mut state = self.lock();
        state.running.remove(command_id);
        state.answered.insert(
            command_id.to_string(),
            (digest.to_string(), answer.clone(), Instant::now()),
        );
        if state.answered.len() > 256 {
            let mut oldest: Vec<(String, Instant)> = state
                .answered
                .iter()
                .map(|(id, (_, _, at))| (id.clone(), *at))
                .collect();
            oldest.sort_by_key(|(_, at)| *at);
            for (id, _) in oldest.into_iter().take(128) {
                state.answered.remove(&id);
            }
        }
        answer
    }

    fn cancel_all(&self) {
        for job in self.lock().jobs.values() {
            let mut job = job.lock().expect("job lock");
            if job.state == "running" && !job.control.cancel.load(Ordering::SeqCst) {
                job.control.cancel.store(true, Ordering::SeqCst);
                shell::terminate(job.control.pid.load(Ordering::SeqCst));
                job.message = "İptal istendi; PC işlemi durduruluyor.".into();
            }
        }
    }
}

impl Sessions {
    async fn dispatch(
        &self,
        session: &Session,
        command_id: &str,
        request: Request,
    ) -> CommandResult {
        let action = request.action.clone().unwrap_or_else(|| "run".into());
        self.prune();
        if action == "undo_file" {
            let context = Context {
                command_id: Uuid::new_v4(),
                services: session.services.clone(),
            };
            let started = Utc::now();
            return match request.undo_id.as_deref() {
                Some(id) => match crate::undo::undo_file(&context, id) {
                    Ok(message) => result(CommandStatus::Completed, message, None, None, started),
                    Err(error) => result(
                        CommandStatus::Failed,
                        error.message.clone(),
                        None,
                        Some(error),
                        started,
                    ),
                },
                None => result(
                    CommandStatus::Failed,
                    "Geri alma kaydı geçersiz.",
                    None,
                    Some(WireError::new(
                        "FILE_UNDO_INVALID",
                        "Geri alma kaydı geçersiz.",
                        "validation",
                    )),
                    started,
                ),
            };
        }
        if matches!(action.as_str(), "get_job" | "cancel_job" | "send_input") {
            return self.job_action(session, &action, &request);
        }
        // Without a card, only scoped file reading and search are allowed.
        let reads_only = matches!(action.as_str(), "read_file" | "search");
        let grant = match &request.grant {
            Some(grant) => grant.clone(),
            None => match request
                .working_directory
                .as_deref()
                .and_then(policy::read_folder_without_grant)
                .filter(|_| reads_only)
            {
                Some(folder) => Grant {
                    id: READ_WITHOUT_GRANT_ID.into(),
                    root_directory: folder.to_string_lossy().to_string(),
                    mode: "read".into(),
                    expires_at: Utc::now().timestamp() + 60,
                    signature: String::new(),
                },
                None => return denied("TERMINAL_GRANT_INVALID", "Terminal oturum izni eksik."),
            },
        };
        if !session.code_enabled {
            return denied(
                "TERMINAL_DISABLED",
                "Kod çalıştırma Wiqo ayarlarında kapalı.",
            );
        }
        if request.grant.is_some() && !(session.grants)(&grant) {
            return denied("TERMINAL_GRANT_INVALID", "Terminal oturum izni geçersiz ya da süresi dolmuş; telefondan yeniden onay gerekiyor.");
        }
        let named = request.working_directory.as_deref().map(|raw| {
            if raw == SCRATCH_FOLDER {
                policy::scratch_directory().to_string_lossy().to_string()
            } else {
                raw.to_string()
            }
        });
        let requested = named
            .or_else(|| {
                self.lock()
                    .directories
                    .get(&grant.id)
                    .map(|path| path.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| grant.root_directory.clone());
        let (Some(root), Some(directory)) = (
            policy::resolve_directory(&grant.root_directory),
            policy::resolve_directory(&requested),
        ) else {
            return denied("TERMINAL_OUTSIDE_GRANT", format!("{requested} onaylanan klasörün ({}) dışında; o klasör için directory vererek yeni onay iste.", grant.root_directory));
        };
        if !policy::is_within(&directory, &root) {
            return denied("TERMINAL_OUTSIDE_GRANT", format!("{requested} onaylanan klasörün ({}) dışında; o klasör için directory vererek yeni onay iste.", grant.root_directory));
        }
        // The folder approved on the card may not exist yet: exactly that one is created, never a subfolder the model names.
        if policy::creatable_directory(&root, &grant.mode)
            && std::fs::create_dir_all(&root).is_err()
        {
            return denied(
                "TERMINAL_DIRECTORY_CREATE_FAILED",
                format!("{} klasörü yoktu ve oluşturulamadı.", grant.root_directory),
            );
        }
        let started = Utc::now();
        match action.as_str() {
            "read_file" | "write_file" | "search" | "edit_file" | "undo" | "apply_patch" => {
                let (session, request, grant, root, directory) = (
                    session.clone(),
                    request.clone(),
                    grant.clone(),
                    root.clone(),
                    directory.clone(),
                );
                let action = action.clone();
                blocking(move || {
                    finished(
                        file_action(&session, &action, &request, &grant, &root, &directory),
                        started,
                    )
                })
                .await
                .unwrap_or_else(|| {
                    denied(
                        "OUTCOME_UNKNOWN",
                        "Terminal işlemi beklenmedik şekilde durdu.",
                    )
                })
            }
            "run_plan" => {
                self.run_plan(session, &request, &grant, &root, &directory)
                    .await
            }
            "run" | "start_job" => {
                let command = request.command.clone().unwrap_or_default();
                if !valid_command(&command) {
                    return denied("TERMINAL_COMMAND_INVALID", "Komut geçersiz.");
                }
                if policy::is_forbidden(&command) {
                    return denied(
                        "TERMINAL_COMMAND_FORBIDDEN",
                        "Bu komut Wiqo terminal oturumlarında çalıştırılmaz.",
                    );
                }
                if grant.mode != "full" && !policy::is_read_only(&command) {
                    return denied("TERMINAL_READ_ONLY", "Bu oturum yalnızca okuma izni veriyor; değiştiren komut için yeni onay gerekiyor.");
                }
                if action == "start_job" {
                    return self.start_job(
                        session, command_id, &request, &grant, &root, &directory, command,
                    );
                }
                self.run_once(&request, &grant, &root, &directory, command, started)
                    .await
            }
            _ => denied("TERMINAL_ACTION_INVALID", "Terminal işlemi desteklenmiyor."),
        }
    }

    async fn run_once(
        &self,
        request: &Request,
        grant: &Grant,
        root: &Path,
        directory: &Path,
        command: String,
        started: chrono::DateTime<Utc>,
    ) -> CommandResult {
        let seconds = request.timeout_seconds.unwrap_or(60).clamp(1, 110);
        let keeps_session = grant.id != READ_WITHOUT_GRANT_ID;
        let exports = if keeps_session {
            self.lock().exports.get(&grant.id).cloned()
        } else {
            None
        };
        let (scope, dir, shell) = (
            write_scope(grant, root),
            directory.to_path_buf(),
            request.shell.clone(),
        );
        let Some(outcome) = blocking(move || {
            shell::run_tracked(
                &command,
                &dir,
                seconds,
                Some(&scope),
                exports.as_deref(),
                shell.as_deref(),
                &Control::default(),
            )
        })
        .await
        else {
            return denied(
                "OUTCOME_UNKNOWN",
                "Terminal işlemi beklenmedik şekilde durdu.",
            );
        };
        if keeps_session {
            let mut state = self.lock();
            if let Some(end) = outcome
                .end_directory
                .as_deref()
                .and_then(|end| policy::resolve_directory(&end.to_string_lossy()))
                .filter(|end| policy::is_within(end, root))
            {
                state.directories.insert(grant.id.clone(), end);
            }
            if let Some(exports) = outcome.exports {
                state.exports.insert(grant.id.clone(), exports);
            }
        }
        let status = match &outcome.error {
            None => CommandStatus::Completed,
            Some(error) if error.category.as_deref() == Some("timeout") => CommandStatus::Unknown,
            Some(_) => CommandStatus::Failed,
        };
        let message = outcome
            .error
            .as_ref()
            .map(|error| error.message.clone())
            .unwrap_or_else(|| "Komut tamamlandı.".into());
        result(
            status,
            message,
            Some(json!({ "kind": "processOutput", "value": outcome.output })),
            outcome.error,
            started,
        )
    }

    async fn run_plan(
        &self,
        session: &Session,
        request: &Request,
        grant: &Grant,
        root: &Path,
        directory: &Path,
    ) -> CommandResult {
        let steps = request.steps.clone().unwrap_or_default();
        if !(1..=8).contains(&steps.len()) {
            return denied("TERMINAL_PLAN_INVALID", "Plan 1-8 komut içermeli.");
        }
        for step in &steps {
            if !valid_command(&step.command) {
                return denied("TERMINAL_COMMAND_INVALID", "Plandaki bir komut geçersiz.");
            }
            if policy::is_forbidden(&step.command) {
                let short: String = step.command.chars().take(80).collect();
                return denied(
                    "TERMINAL_COMMAND_FORBIDDEN",
                    format!(
                        "Plandaki bir komut Wiqo terminal oturumlarında çalıştırılmaz: {short}"
                    ),
                );
            }
            if grant.mode != "full" && !policy::is_read_only(&step.command) {
                return denied("TERMINAL_READ_ONLY", "Bu oturum yalnızca okuma izni veriyor; plandaki değiştiren komut için yeni onay gerekiyor.");
            }
        }
        let started = Utc::now();
        let clock = Instant::now();
        let budget = request.timeout_seconds.unwrap_or(110).clamp(5, 110);
        let keeps_session = grant.id != READ_WITHOUT_GRANT_ID;
        let mut exports = if keeps_session {
            self.lock().exports.get(&grant.id).cloned()
        } else {
            None
        };
        let mut current = directory.to_path_buf();
        let mut results: Vec<Value> = Vec::new();
        let mut failed = false;
        for step in steps {
            // The grant is checked again before every step: a revoked or expired grant stops the plan.
            let active = request.grant.is_none() || (session.grants)(grant);
            let remaining = budget.saturating_sub(clock.elapsed().as_secs());
            if failed || !active || remaining < 1 {
                failed = true;
                results.push(json!({ "command": step.command, "exitCode": -1, "stdout": "", "stderr": "", "durationMs": 0, "truncated": false, "skipped": true }));
                continue;
            }
            let (command, dir, scope, carried, shell) = (
                step.command.clone(),
                current.clone(),
                write_scope(grant, root),
                exports.clone(),
                request.shell.clone(),
            );
            let outcome = blocking(move || {
                shell::run_tracked(
                    &command,
                    &dir,
                    remaining,
                    Some(&scope),
                    carried.as_deref(),
                    shell.as_deref(),
                    &Control::default(),
                )
            })
            .await;
            let Some(outcome) = outcome else {
                failed = true;
                continue;
            };
            if let Some(end) = outcome
                .end_directory
                .as_deref()
                .and_then(|end| policy::resolve_directory(&end.to_string_lossy()))
                .filter(|end| policy::is_within(end, root))
            {
                current = end;
            }
            if outcome.exports.is_some() {
                exports = outcome.exports.clone();
            }
            let output = &outcome.output;
            let exit = output["exitCode"].as_i64().unwrap_or(-1);
            results.push(json!({
                "command": step.command, "exitCode": exit, "stdout": output["stdout"], "stderr": output["stderr"],
                "durationMs": output["durationMs"], "truncated": output["truncated"], "skipped": false,
            }));
            if exit != 0 && step.continue_on_error != Some(true) {
                failed = true;
            }
        }
        {
            let mut state = self.lock();
            state.directories.insert(grant.id.clone(), current.clone());
            if let (true, Some(exports)) = (keeps_session, exports) {
                state.exports.insert(grant.id.clone(), exports);
            }
        }
        let all_ok = results
            .iter()
            .all(|step| step["exitCode"] == 0 && step["skipped"] == false);
        let data = terminal_result("plan", &current, json!({ "steps": results }));
        result(
            if all_ok {
                CommandStatus::Completed
            } else {
                CommandStatus::Failed
            },
            if all_ok {
                "Plan tamamlandı."
            } else {
                "Plandaki bir komut hata verdi."
            },
            Some(data),
            None,
            started,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_job(
        &self,
        session: &Session,
        command_id: &str,
        request: &Request,
        grant: &Grant,
        root: &Path,
        directory: &Path,
        command: String,
    ) -> CommandResult {
        let Some(id) = request
            .job_id
            .as_deref()
            .filter(|id| Uuid::parse_str(id).is_ok())
            .map(str::to_string)
        else {
            return denied("TERMINAL_JOB_ID_INVALID", "Arka plan işi kimliği geçersiz.");
        };
        let interactive = request.interactive == Some(true) && grant.mode == "full" && cfg!(unix);
        let remaining = (grant.expires_at - Utc::now().timestamp()).max(1) as u64;
        let seconds = request
            .timeout_seconds
            .unwrap_or(600)
            .min(remaining)
            .clamp(1, 1800);
        let (job, exports) = {
            let mut state = self.lock();
            if let Some(existing) = state.jobs.get(&id) {
                if existing.lock().expect("job lock").grant_id != grant.id {
                    return denied(
                        "TERMINAL_GRANT_INVALID",
                        "Bu iş farklı bir terminal iznine bağlı.",
                    );
                }
                drop(state);
                return self.snapshot(&id);
            }
            let control = Control {
                input: interactive.then(|| Arc::new(Input::default())),
                ..Control::default()
            };
            let job = Arc::new(Mutex::new(Job {
                grant_id: grant.id.clone(),
                working_directory: directory.to_path_buf(),
                started_at: Utc::now(),
                state: "running",
                message: "Terminal işi çalışıyor.".into(),
                error_code: None,
                output: None,
                control,
            }));
            state.jobs.insert(id.clone(), job.clone());
            (job, state.exports.get(&grant.id).cloned())
        };
        let watcher = job.clone();
        let progress: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(move |output| {
            let mut job = watcher.lock().expect("job lock");
            if job.state == "running" {
                job.output = Some(output);
            }
        });
        let control = {
            let mut job = job.lock().expect("job lock");
            job.control.progress = Some(progress);
            job.control.clone()
        };
        let (state, outbox, scope, dir, root, grant_id, shell, job_id) = (
            self.state.clone(),
            session.outbox.clone(),
            write_scope(grant, root),
            directory.to_path_buf(),
            root.to_path_buf(),
            grant.id.clone(),
            request.shell.clone(),
            id.clone(),
        );
        let starting_directory = self.lock().directories.get(&grant.id).cloned();
        std::thread::spawn(move || {
            let outcome = shell::run_tracked(
                &command,
                &dir,
                seconds,
                Some(&scope),
                exports.as_deref(),
                shell.as_deref(),
                &control,
            );
            let mut job = job.lock().expect("job lock");
            let cancelled = job.control.cancel.load(Ordering::SeqCst);
            {
                // A late background job must not overwrite where a newer foreground command left the session.
                let mut shared = state.lock().expect("terminal lock");
                if !cancelled
                    && outcome.error.is_none()
                    && shared.directories.get(&grant_id) == starting_directory.as_ref()
                {
                    if let Some(end) = outcome
                        .end_directory
                        .as_deref()
                        .and_then(|end| policy::resolve_directory(&end.to_string_lossy()))
                        .filter(|end| policy::is_within(end, &root))
                    {
                        shared.directories.insert(grant_id.clone(), end);
                    }
                    if let Some(exports) = &outcome.exports {
                        shared.exports.insert(grant_id.clone(), exports.clone());
                    }
                }
            }
            job.output = Some(outcome.output.clone());
            let (state_name, message, code): (&'static str, String, Option<String>) = if cancelled {
                (
                    "cancelled",
                    "Terminal işi kullanıcı tarafından durduruldu.".into(),
                    None,
                )
            } else {
                match &outcome.error {
                    Some(error) if error.category.as_deref() == Some("timeout") => (
                        "unknown",
                        "Terminal işi zaman aşımına uğradı; sonucu doğrulanamadı.".into(),
                        Some(error.code.clone()),
                    ),
                    Some(error) => ("failed", error.message.clone(), Some(error.code.clone())),
                    None => ("completed", "Terminal işi tamamlandı.".into(), None),
                }
            };
            job.state = state_name;
            job.message = message;
            job.error_code = code.clone();
            outbox.send(&json!({ "type": "terminalJobUpdate", "jobId": job_id, "state": state_name, "errorCode": code }));
        });
        let _ = command_id;
        self.snapshot(&id)
    }

    fn snapshot(&self, id: &str) -> CommandResult {
        let Some(job) = self.lock().jobs.get(id).cloned() else {
            return denied("TERMINAL_JOB_UNKNOWN", "Bu PC sürecinde iş bulunamadı.");
        };
        let job = job.lock().expect("job lock");
        let content = json!({ "jobId": id, "status": job.state, "output": job.output, "message": job.message, "errorCode": job.error_code });
        let status = match job.state {
            "unknown" => CommandStatus::Unknown,
            "failed" => CommandStatus::Failed,
            _ => CommandStatus::Completed,
        };
        let error = job
            .error_code
            .as_ref()
            .map(|code| WireError::new(code, job.message.clone(), "tool_failure"));
        let mut answer = result(
            status,
            job.message.clone(),
            Some(json!({ "kind": "terminalJob", "value": content })),
            error,
            job.started_at,
        );
        answer.command_id = Uuid::parse_str(id).unwrap_or_else(|_| Uuid::new_v4());
        answer
    }

    fn job_action(&self, session: &Session, action: &str, request: &Request) -> CommandResult {
        let job = request
            .job_id
            .as_deref()
            .filter(|id| Uuid::parse_str(id).is_ok())
            .and_then(|id| self.lock().jobs.get(id).cloned());
        let Some(job) = job else {
            let message = "Bu PC sürecinde iş bulunamadı; yeniden başlatıldıysa sonucu bilinmiyor.";
            let content = json!({ "jobId": request.job_id.clone().unwrap_or_default(), "status": "unknown", "message": message, "errorCode": "TERMINAL_JOB_UNKNOWN" });
            return result(
                CommandStatus::Unknown,
                message,
                Some(json!({ "kind": "terminalJob", "value": content })),
                Some(WireError::new(
                    "TERMINAL_JOB_UNKNOWN",
                    message,
                    "tool_failure",
                )),
                Utc::now(),
            );
        };
        let id = request.job_id.clone().unwrap_or_default();
        // A job's own grant may watch or stop it even after the grant expired; it can never start a process or widen the folder.
        let Some(grant) = request.grant.as_ref() else {
            return denied(
                "TERMINAL_GRANT_INVALID",
                "İşin imzalı terminal izni doğrulanamadı.",
            );
        };
        {
            let current = job.lock().expect("job lock");
            let root = policy::resolve_directory(&grant.root_directory);
            if grant.id != current.grant_id
                || !root.is_some_and(|root| policy::is_within(&current.working_directory, &root))
            {
                return denied(
                    "TERMINAL_GRANT_INVALID",
                    "İşin imzalı terminal izni doğrulanamadı.",
                );
            }
        }
        if action == "send_input" {
            let input = request.input.clone().unwrap_or_default();
            let current = job.lock().expect("job lock");
            let allowed = session.code_enabled
                && (session.grants)(grant)
                && grant.mode == "full"
                && current.state == "running"
                && !current.control.cancel.load(Ordering::SeqCst)
                && !input.is_empty()
                && input.len() <= 4096
                && !input.contains('\0')
                && !policy::is_forbidden(&input);
            let Some(endpoint) = current.control.input.clone().filter(|_| allowed) else {
                return denied(
                    "TERMINAL_INPUT_DENIED",
                    "Etkin tam izinli etkileşimli iş gerekli.",
                );
            };
            if input == "\u{3}" {
                shell::interrupt(current.control.pid.load(Ordering::SeqCst));
            } else if !endpoint.send(&input) {
                return denied(
                    "TERMINAL_INPUT_UNCERTAIN",
                    "Girdi teslimi doğrulanamadı; tekrarlamadan önce çıktıyı kontrol et.",
                );
            }
            drop(current);
            // Give the program a moment to answer, so the snapshot shows its reaction.
            std::thread::sleep(Duration::from_millis(300));
        }
        if action == "cancel_job" {
            let mut current = job.lock().expect("job lock");
            if current.state == "running" {
                current.control.cancel.store(true, Ordering::SeqCst);
                shell::terminate(current.control.pid.load(Ordering::SeqCst));
                current.message = "İptal istendi; PC işlemi durduruluyor.".into();
            }
        }
        self.snapshot(&id)
    }

    /// Finished jobs older than a day go, and at most 50 finished ones are kept.
    fn prune(&self) {
        let mut state = self.lock();
        let mut finished: Vec<(String, chrono::DateTime<Utc>)> = state
            .jobs
            .iter()
            .filter_map(|(id, job)| {
                let job = job.lock().ok()?;
                (job.state != "running").then(|| (id.clone(), job.started_at))
            })
            .collect();
        finished.sort_by_key(|(_, at)| *at);
        let excess = finished.len().saturating_sub(50);
        for (index, (id, at)) in finished.into_iter().enumerate() {
            if index < excess || Utc::now() - at > chrono::Duration::hours(24) {
                state.jobs.remove(&id);
            }
        }
    }
}

/// terminal-session-v2 file actions; the grant, the folder boundary and the policy were already checked.
fn file_action(
    session: &Session,
    action: &str,
    request: &Request,
    grant: &Grant,
    root: &Path,
    directory: &Path,
) -> Result<(String, Value), WireError> {
    let deny = |code: &str, message: String| WireError::new(code, message, "permission");
    let full = grant.mode == "full";
    match action {
        "undo" => {
            let id = request.undo_id.as_deref().filter(|_| full).ok_or_else(|| {
                deny(
                    "TERMINAL_UNDO_INVALID",
                    "Geri alma kaydı veya tam klasör izni bulunamadı.".into(),
                )
            })?;
            let message = edit::undo(&session.services, id, root)?;
            Ok((
                message.clone(),
                terminal_result("undo", directory, json!({ "text": message })),
            ))
        }
        "read_file" => {
            let raw = request.file_path.clone().unwrap_or_default();
            let path = policy::resolve_existing_file(&raw, directory, root).ok_or_else(|| {
                deny("TERMINAL_FILE_SCOPE", format!("{} bulunamadı ya da onaylı klasörün ({}) dışında. search ile doğru yolu bul.", if raw.is_empty() { "Dosya" } else { &raw }, grant.root_directory))
            })?;
            let (text, total) = edit::read(&path, request.start_line, request.end_line)?;
            Ok((
                "Dosya okundu.".into(),
                terminal_result(
                    "read",
                    directory,
                    json!({ "text": text, "files": [raw], "totalLines": total }),
                ),
            ))
        }
        "write_file" => {
            if !full {
                return Err(deny(
                    "TERMINAL_READ_ONLY",
                    "Dosya yazmak için tam izin gerekiyor.".into(),
                ));
            }
            let raw = request.file_path.clone().unwrap_or_default();
            let content = request.content.clone();
            let path = policy::resolve_writable_file(&raw, directory, root).filter(|_| content.is_some()).ok_or_else(|| {
                deny(
                    "TERMINAL_FILE_SCOPE",
                    format!("{} yazılamadı: onaylı klasörün ({}) içinde, var olan bir klasörde olmalı. Alt klasör gerekiyorsa önce run ile mkdir -p çalıştır.", if raw.is_empty() { "Dosya" } else { &raw }, grant.root_directory),
                )
            })?;
            let before = edit::Before::read(&path).ok_or_else(|| {
                deny(
                    "TERMINAL_UNDO_BACKUP_FAILED",
                    "Dosyanın önceki hali güvenle okunamadı; değişiklik uygulanmadı.".into(),
                )
            })?;
            let created = edit::write(&path, &content.unwrap_or_default())?;
            let undo = edit::register_undo(&session.services, root, vec![before]);
            let text = if created { "created" } else { "updated" };
            Ok((
                if created {
                    "Dosya oluşturuldu."
                } else {
                    "Dosya güncellendi."
                }
                .into(),
                terminal_result(
                    "write",
                    directory,
                    json!({ "text": text, "files": [raw], "undoId": undo }),
                ),
            ))
        }
        "search" => {
            let text = edit::search(
                request.pattern.as_deref().unwrap_or(""),
                directory,
                request.glob.as_deref(),
            )?;
            Ok((
                "Arama tamamlandı.".into(),
                terminal_result("search", directory, json!({ "text": text })),
            ))
        }
        "apply_patch" => {
            if !full {
                return Err(deny(
                    "TERMINAL_READ_ONLY",
                    "Yama için tam izin gerekiyor.".into(),
                ));
            }
            // v1: one patch on `filePath`; v2: `edits`.
            let edits: Vec<FileEdit> = match (&request.edits, &request.file_path, &request.patch) {
                (Some(edits), _, _) if !edits.is_empty() => edits.clone(),
                (_, Some(path), Some(patch)) => vec![FileEdit {
                    file_path: path.clone(),
                    patch: patch.clone(),
                }],
                _ => {
                    return Err(deny(
                        "TERMINAL_PATCH_SCOPE",
                        "Yama için tam izin ve onaylı klasördeki mevcut bir dosya gerekiyor."
                            .into(),
                    ))
                }
            };
            if !(1..=10).contains(&edits.len())
                || edits.iter().map(|edit| edit.patch.len()).sum::<usize>() > 300_000
            {
                return Err(deny(
                    "TERMINAL_PATCH_SCOPE",
                    "1-10 dosyalık, toplam 300 KB altında yama gerekiyor.".into(),
                ));
            }
            let mut resolved = Vec::new();
            for edit in &edits {
                let path = policy::resolve_existing_file(&edit.file_path, directory, root)
                    .ok_or_else(|| {
                        deny(
                            "TERMINAL_PATCH_SCOPE",
                            format!(
                                "{} onaylı klasörde bulunamadı; hiçbir dosya değiştirilmedi.",
                                edit.file_path
                            ),
                        )
                    })?;
                resolved.push((path, edit.patch.clone(), edit.file_path.clone()));
            }
            let before: Vec<edit::Before> = resolved
                .iter()
                .filter_map(|(path, _, _)| edit::Before::read(path))
                .collect();
            if before.len() != resolved.len() {
                return Err(deny(
                    "TERMINAL_UNDO_BACKUP_FAILED",
                    "Dosyaların önceki hali güvenle okunamadı; yama uygulanmadı.".into(),
                ));
            }
            let diff = edit::patch(&resolved)?;
            let undo = edit::register_undo(&session.services, root, before);
            let files: Vec<String> = edits.iter().map(|edit| edit.file_path.clone()).collect();
            Ok((
                "Yama uygulandı.".into(),
                terminal_result(
                    "edits",
                    directory,
                    json!({ "text": diff, "files": files, "undoId": undo }),
                ),
            ))
        }
        "edit_file" => {
            if !full {
                return Err(deny(
                    "TERMINAL_READ_ONLY",
                    "Dosya düzenlemek için tam izin gerekiyor.".into(),
                ));
            }
            let replacements = request.replacements.clone().unwrap_or_default();
            if !(1..=20).contains(&replacements.len())
                || replacements
                    .iter()
                    .map(|item| item.find.len() + item.replace.len())
                    .sum::<usize>()
                    > 300_000
            {
                return Err(deny(
                    "TERMINAL_EDIT_INVALID",
                    "1-20 değişiklik, toplam 300 KB altında gerekiyor.".into(),
                ));
            }
            let mut resolved = Vec::new();
            for item in &replacements {
                let path = policy::resolve_existing_file(&item.file_path, directory, root)
                    .ok_or_else(|| {
                        deny(
                            "TERMINAL_FILE_SCOPE",
                            format!(
                                "{} onaylı klasörde bulunamadı; hiçbir dosya değiştirilmedi.",
                                item.file_path
                            ),
                        )
                    })?;
                resolved.push(edit::Replacement {
                    path,
                    display: item.file_path.clone(),
                    find: item.find.clone(),
                    replace: item.replace.clone(),
                    replace_all: item.replace_all == Some(true),
                });
            }
            let mut unique: Vec<PathBuf> = resolved.iter().map(|item| item.path.clone()).collect();
            unique.sort();
            unique.dedup();
            let before: Vec<edit::Before> = unique
                .iter()
                .filter_map(|path| edit::Before::read(path))
                .collect();
            if before.len() != unique.len() {
                return Err(deny(
                    "TERMINAL_UNDO_BACKUP_FAILED",
                    "Dosyaların önceki hali güvenle okunamadı; düzenleme uygulanmadı.".into(),
                ));
            }
            let summary = edit::replace(&resolved)?;
            let undo = edit::register_undo(&session.services, root, before);
            let mut files: Vec<String> = replacements
                .iter()
                .map(|item| item.file_path.clone())
                .collect();
            files.sort();
            files.dedup();
            Ok((
                "Dosyalar düzenlendi.".into(),
                terminal_result(
                    "edits",
                    directory,
                    json!({ "text": summary, "files": files, "undoId": undo }),
                ),
            ))
        }
        _ => Err(deny(
            "TERMINAL_ACTION_INVALID",
            "Terminal işlemi desteklenmiyor.".into(),
        )),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use wiqo_core::engine::Services;
    use wiqo_core::terminal::Outbox;

    fn session(accept: bool) -> Session {
        static SERVICES: OnceLock<Arc<Services>> = OnceLock::new();
        let services = SERVICES.get_or_init(Services::temporary).clone();
        Session {
            services,
            grants: Arc::new(move |_| accept),
            outbox: Outbox::default(),
            code_enabled: true,
        }
    }

    fn grant(root: &Path, mode: &str) -> Value {
        json!({ "id": Uuid::new_v4().to_string(), "rootDirectory": root.to_string_lossy(), "mode": mode, "expiresAt": Utc::now().timestamp() + 600, "signature": "x" })
    }

    #[tokio::test]
    async fn a_session_runs_edits_watches_a_job_and_never_runs_a_command_twice() {
        let sessions = Sessions::new();
        let root = dunce::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("wiqo-terminal-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let full = grant(&root, "full");
        let run = |command: &str| json!({ "action": "run", "grant": full, "command": command });

        let answer = sessions
            .handle(
                session(true),
                "c1",
                &run("cd src && echo merhaba > a.txt && pwd"),
                "d1",
            )
            .await;
        assert_eq!(answer.status, CommandStatus::Completed, "{answer:?}");
        let again = sessions
            .handle(
                session(true),
                "c1",
                &run("cd src && echo merhaba > a.txt && pwd"),
                "d1",
            )
            .await;
        assert_eq!(
            again.finished_at, answer.finished_at,
            "the stored answer, not a second run"
        );
        // The next command continues in src/.
        let read = sessions
            .handle(
                session(true),
                "c2",
                &json!({ "action": "read_file", "grant": full, "filePath": "a.txt" }),
                "d2",
            )
            .await;
        assert!(read.data.unwrap()["value"]["text"]
            .as_str()
            .unwrap()
            .contains("merhaba"));

        let edit = json!({ "action": "edit_file", "grant": full, "replacements": [{ "filePath": "a.txt", "find": "merhaba", "replace": "selam" }] });
        let edited = sessions.handle(session(true), "c3", &edit, "d3").await;
        let undo_id = edited.data.unwrap()["value"]["undoId"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            std::fs::read_to_string(root.join("src/a.txt")).unwrap(),
            "selam\n"
        );
        sessions
            .handle(
                session(true),
                "c4",
                &json!({ "action": "undo", "grant": full, "undoId": undo_id }),
                "d4",
            )
            .await;
        assert_eq!(
            std::fs::read_to_string(root.join("src/a.txt")).unwrap(),
            "merhaba\n"
        );

        let outside = sessions
            .handle(
                session(true),
                "c5",
                &json!({ "action": "read_file", "grant": full, "filePath": "/etc/hosts" }),
                "d5",
            )
            .await;
        assert_eq!(outside.error.unwrap().code, "TERMINAL_FILE_SCOPE");
        let forbidden = sessions
            .handle(session(true), "c6", &run("sudo ls"), "d6")
            .await;
        assert_eq!(forbidden.error.unwrap().code, "TERMINAL_COMMAND_FORBIDDEN");
        let read_only =
            json!({ "action": "run", "grant": grant(&root, "read"), "command": "touch x" });
        assert_eq!(
            sessions
                .handle(session(true), "c7", &read_only, "d7")
                .await
                .error
                .unwrap()
                .code,
            "TERMINAL_READ_ONLY"
        );
        assert_eq!(
            sessions
                .handle(session(false), "c8", &run("ls"), "d8")
                .await
                .error
                .unwrap()
                .code,
            "TERMINAL_GRANT_INVALID"
        );

        let job = Uuid::new_v4().to_string();
        let start =
            json!({ "action": "start_job", "grant": full, "command": "sleep 30", "jobId": job });
        let started = sessions.handle(session(true), "c9", &start, "d9").await;
        assert_eq!(started.data.unwrap()["value"]["status"], "running");
        let cancel = json!({ "action": "cancel_job", "grant": full, "jobId": job });
        sessions.handle(session(true), "c10", &cancel, "d10").await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let state = sessions
            .handle(
                session(true),
                "c11",
                &json!({ "action": "get_job", "grant": full, "jobId": job }),
                "d11",
            )
            .await;
        assert_eq!(state.data.unwrap()["value"]["status"], "cancelled");

        let plan = json!({ "action": "run_plan", "grant": full, "steps": [{ "command": "echo one" }, { "command": "exit 2" }, { "command": "echo never" }] });
        let planned = sessions.handle(session(true), "c12", &plan, "d12").await;
        let steps = planned.data.unwrap()["value"]["steps"].clone();
        assert_eq!(steps[2]["skipped"], true);
        assert_eq!(planned.status, CommandStatus::Failed);
    }
}
