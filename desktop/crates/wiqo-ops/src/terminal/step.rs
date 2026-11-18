//! `terminal`: one step of the chat's stream for this computer's terminal sessions. The server only builds the step; this computer runs it in its own
//! session (folder memory, background jobs, undo records) and checks its own policy on every command. The person's yes for a step that changes
//! something was asked before the step got here (the chat, `needs_yes`); the grant a session needs is this computer's own, per folder and mode,
//! for half an hour. Nothing about it leaves this computer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use wiqo_core::engine::{Context, Lane, Operation, Outcome};
use wiqo_core::terminal::{Grant, Session, Terminal};
use wiqo_core::wire::{CommandResult, CommandStatus, WireError};

use super::policy;
use super::Sessions;
use crate::Op;

/// Marks a grant this computer gave itself for a step the person allowed; nothing else carries it.
const LOCAL_STEP: &str = "local-step-v1";
const GRANT_SECONDS: i64 = 30 * 60;
/// What a `wait_job` waits at most: the step's own time limit is longer by a margin.
const MAX_WAIT_SECONDS: u64 = 45;
/// What a `run`/`run_plan` may take: under the contract's long budget (55 s) with room for the answer.
const MAX_RUN_SECONDS: u64 = 50;

#[derive(Default)]
struct Local {
    /// The grant of a folder and mode, by (folder, mode).
    by_folder: HashMap<(String, String), Grant>,
    /// The grant a background job started under: watching and stopping it use the same one.
    by_job: HashMap<String, Grant>,
    /// The folder of the latest step: a step that names none continues there (the shell's own `cd` is kept per grant).
    last_folder: Option<String>,
}

pub(crate) struct TerminalStep {
    sessions: Arc<Sessions>,
    local: Arc<Mutex<Local>>,
}

pub(crate) fn operation(sessions: Arc<Sessions>) -> Op {
    Arc::new(TerminalStep {
        sessions,
        local: Arc::new(Mutex::new(Local::default())),
    })
}

fn refused(code: &str, message: &str) -> Outcome {
    Outcome {
        message: message.into(),
        data: None,
        error: Some(WireError::new(code, message, "validation")),
    }
}

/// A terminal answer as an operation's outcome; "not known" stays not known (the chat never replays it).
fn outcome(result: CommandResult) -> Outcome {
    let error =
        match result.status {
            CommandStatus::Completed => result.error,
            CommandStatus::Unknown => Some(result.error.unwrap_or_else(|| {
                WireError::new("OUTCOME_UNKNOWN", &result.message, "tool_failure")
            })),
            _ => Some(result.error.unwrap_or_else(|| {
                WireError::new("TERMINAL_FAILED", &result.message, "tool_failure")
            })),
        };
    Outcome {
        message: result.message,
        data: result.data,
        error,
    }
}

impl TerminalStep {
    fn session(&self, ctx: &Context) -> Session {
        let local = self.local.clone();
        Session {
            services: ctx.services.clone(),
            grants: Arc::new(move |grant: &Grant| {
                let own = local.lock().expect("terminal grants");
                grant.signature == LOCAL_STEP
                    && grant.expires_at > Utc::now().timestamp()
                    && own
                        .by_folder
                        .values()
                        .chain(own.by_job.values())
                        .any(|kept| kept.id == grant.id)
            }),
            code_enabled: true,
        }
    }

    /// The grant for a folder and mode: the open one, renewed, or a new one. A "full" grant covers reading the same folder.
    fn grant(&self, folder: &str, mode: &str) -> Grant {
        let mut local = self.local.lock().expect("terminal grants");
        let expires_at = Utc::now().timestamp() + GRANT_SECONDS;
        let key = (folder.to_string(), mode.to_string());
        let wider = (folder.to_string(), "full".to_string());
        let existing = local
            .by_folder
            .get(&key)
            .or_else(|| local.by_folder.get(&wider))
            .cloned();
        let grant = match existing {
            Some(grant) => Grant {
                expires_at,
                ..grant
            },
            None => Grant {
                id: Uuid::new_v4().to_string(),
                root_directory: folder.to_string(),
                mode: mode.to_string(),
                expires_at,
                signature: LOCAL_STEP.into(),
            },
        };
        local
            .by_folder
            .insert((folder.to_string(), grant.mode.clone()), grant.clone());
        grant
    }

    async fn ask(&self, ctx: &Context, id: &str, request: &Value) -> CommandResult {
        let digest = hex::encode(Sha256::digest(
            wiqo_contract::canonical_json(request)
                .unwrap_or_default()
                .as_bytes(),
        ));
        self.sessions
            .handle(self.session(ctx), id, request, &digest)
            .await
    }

    async fn run_step(&self, ctx: &Context, value: &Value) -> Outcome {
        let action = value["action"].as_str().unwrap_or("run").to_string();
        let id = ctx.command_id.to_string();
        match action.as_str() {
            // A change Wiqo made, put back file by file, newest first; the first file that cannot be put back stops it.
            "undo_file" => {
                let ids: Vec<String> = value["undoIds"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(|id| id.as_str().map(str::to_string))
                            .collect()
                    })
                    .or_else(|| value["undoId"].as_str().map(|id| vec![id.to_string()]))
                    .unwrap_or_default();
                if ids.is_empty() {
                    return refused("FILE_UNDO_INVALID", "Geri alma kaydı geçersiz.");
                }
                let mut restored = 0;
                for (index, undo) in ids.iter().enumerate() {
                    let result = self
                        .ask(
                            ctx,
                            &format!("{id}:{index}"),
                            &json!({ "action": "undo_file", "undoId": undo }),
                        )
                        .await;
                    if result.status != CommandStatus::Completed {
                        let message = if restored == 0 {
                            result.message.clone()
                        } else {
                            format!(
                                "{restored} dosya eski haline döndü; kalanlar geri alınmadı: {}",
                                result.message
                            )
                        };
                        return Outcome {
                            message,
                            data: None,
                            error: result.error.or_else(|| {
                                Some(WireError::new(
                                    "FILE_UNDO_FAILED",
                                    &result.message,
                                    "tool_failure",
                                ))
                            }),
                        };
                    }
                    restored += 1;
                }
                Outcome::ok(
                    format!("{restored} dosya eski haline getirildi."),
                    json!({ "kind": "undo", "value": { "restored": restored } }),
                )
            }
            "get_job" | "cancel_job" | "send_input" | "wait_job" => {
                let job = value["jobId"].as_str().unwrap_or("").to_string();
                let Some(grant) = self
                    .local
                    .lock()
                    .expect("terminal grants")
                    .by_job
                    .get(&job)
                    .cloned()
                else {
                    return refused(
                        "TERMINAL_JOB_NOT_FOUND",
                        "Bu bilgisayarda bu kimlikle bir terminal işi yok.",
                    );
                };
                if action != "wait_job" {
                    let mut request = value.clone();
                    request["grant"] = json!(grant);
                    return outcome(self.ask(ctx, &id, &request).await);
                }
                // Waits for the job here instead of spending model turns on polling; what it has by then is the answer.
                let seconds = value["waitSeconds"]
                    .as_u64()
                    .unwrap_or(MAX_WAIT_SECONDS)
                    .clamp(1, MAX_WAIT_SECONDS);
                let deadline = std::time::Instant::now() + Duration::from_secs(seconds);
                let mut round = 0;
                loop {
                    let result = self
                        .ask(
                            ctx,
                            &format!("{id}:{round}"),
                            &json!({ "action": "get_job", "jobId": job, "grant": grant }),
                        )
                        .await;
                    let running = result
                        .data
                        .as_ref()
                        .is_some_and(|data| data["value"]["status"] == "running");
                    if !running || std::time::Instant::now() >= deadline {
                        return outcome(result);
                    }
                    round += 1;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            _ => {
                let named = value["workingDirectory"].as_str().map(str::to_string);
                // A command that names no folder and follows none runs from the person's home folder, the way a new terminal window opens: asking the
                // model for a folder it has no reason to know ("en büyük dosyalar", "pil durumu") only made it fail first and retry.
                let folder = named
                    .clone()
                    .or_else(|| {
                        self.local
                            .lock()
                            .expect("terminal grants")
                            .last_folder
                            .clone()
                    })
                    .unwrap_or_else(|| "~".to_string());
                // Under the person's full access every step runs with a full grant: no command is sorted into read or write first.
                let mode = if value["mode"] == "full" || super::policy::full_access() {
                    "full"
                } else {
                    "read"
                };
                let mut request = value.clone();
                if let Some(object) = request.as_object_mut() {
                    object.remove("mode");
                }
                // A read in the person's own folder needs no grant (this computer's own rule); everything else works under one.
                // Only a file read or a search may go without a grant; a command always carries one (a read one when it only looks).
                let file_read = matches!(action.as_str(), "read_file" | "search");
                let grant = (mode == "full"
                    || !file_read
                    || policy::read_folder_without_grant(&folder).is_none())
                .then(|| self.grant(&folder, mode));
                // A step that names no folder goes on where the session's shell ended (its `cd`, kept per grant); one without a grant names its folder.
                if named.is_some() || grant.is_none() {
                    request["workingDirectory"] = json!(folder);
                }
                if let Some(grant) = &grant {
                    request["grant"] = json!(grant);
                }
                // An install, a clone or a build outlives the minute a plain command gets: started as a background job instead of being cut off and
                // reported as unknown. The answer says it runs; wait_job reads its end.
                let promoted = action == "run" && mode == "full" && value["command"].as_str().is_some_and(takes_long);
                let action = if promoted { "start_job".to_string() } else { action };
                if promoted {
                    request["action"] = json!("start_job");
                    request["long"] = json!(true);
                    request["label"] = json!(value["command"].as_str().unwrap_or("").chars().take(80).collect::<String>());
                    if let Some(object) = request.as_object_mut() {
                        object.remove("timeoutSeconds");
                    }
                }
                if action == "start_job" || action == "watch" {
                    request["jobId"] = json!(id);
                } else {
                    // A command that waits for its end must end inside the step's own time limit (the contract's long budget); longer work is a start_job.
                    request["timeoutSeconds"] = json!(value["timeoutSeconds"]
                        .as_u64()
                        .unwrap_or(MAX_RUN_SECONDS)
                        .clamp(1, MAX_RUN_SECONDS));
                }
                let result = self.ask(ctx, &id, &request).await;
                let mut local = self.local.lock().expect("terminal grants");
                if folder != policy::SCRATCH_FOLDER {
                    local.last_folder = Some(folder);
                }
                if let (true, Some(grant)) = (action == "start_job" || action == "watch", grant) {
                    local.by_job.insert(id, grant);
                }
                drop(local);
                outcome(result)
            }
        }
    }
}

/// A command that installs, clones or builds: it usually takes longer than a plain step may wait.
fn takes_long(command: &str) -> bool {
    static SLOW: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    SLOW.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(?:(?:npm|pnpm|yarn|bun)\s+(?:i|install|ci|add|create|build|run\s+build)|npx\s+create-|pip3?\s+install|pipx\s+install|uv\s+(?:pip\s+install|sync)|brew\s+(?:install|upgrade|update|reinstall)|cargo\s+(?:build|install)|git\s+clone|docker\s+(?:build|pull|compose\s+up)|xcodebuild|swift\s+build|winget\s+install|choco\s+install|scoop\s+install|gradlew?\s|mvn\s|go\s+(?:build|install|test|mod\s+download)|(?:npm|pnpm|yarn|bun)\s+(?:test|run\s+(?:test|lint|typecheck|e2e)\S*)|cargo\s+(?:test|check|clippy)|pytest|python3?\s+-m\s+pytest|swift\s+test|make(?:\s|$)|tsc(?:\s|$)|flutter\s+(?:build|pub\s+get)|dotnet\s+(?:build|restore|publish))")
            .expect("slow command pattern")
    })
    .is_match(command)
}

#[async_trait::async_trait]
impl Operation for TerminalStep {
    fn name(&self) -> &'static str {
        "terminal"
    }

    fn lane(&self) -> Lane {
        Lane::SideEffect
    }

    async fn run(&self, ctx: &Context, payload: &Value) -> Outcome {
        if payload["kind"] != "terminal" || !payload["value"].is_object() {
            return refused("INVALID_PAYLOAD", "Geçersiz terminal isteği.");
        }
        self.run_step(ctx, &payload["value"]).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use wiqo_core::engine::Services;

    fn context(services: &Arc<Services>) -> Context {
        Context {
            command_id: Uuid::new_v4(),
            services: services.clone(),
        }
    }

    fn step(value: Value) -> Value {
        json!({ "kind": "terminal", "value": value })
    }

    #[test]
    fn installs_clones_and_builds_are_long_and_a_listing_is_not() {
        for command in ["npm install", "cd app && pnpm i", "pip install requests", "brew install ffmpeg", "git clone https://x/y.git", "cargo build --release", "winget install Git.Git"] {
            assert!(takes_long(command), "{command}");
        }
        for command in ["ls -lS ~/Downloads | head", "git status", "npm --version", "python3 main.py"] {
            assert!(!takes_long(command), "{command}");
        }
    }

    #[tokio::test]
    async fn a_step_runs_under_this_computers_own_grant_and_the_next_one_goes_on_in_the_same_folder(
    ) {
        let services = Services::temporary();
        let folder = std::env::temp_dir().join(format!("wiqo-step-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let folder = folder.canonicalize().unwrap().to_string_lossy().to_string();
        let op = TerminalStep {
            sessions: Sessions::new(),
            local: Arc::new(Mutex::new(Local::default())),
        };

        let nothing = op
            .run(
                &context(&services),
                &step(json!({ "action": "run", "command": "ls" })),
            )
            .await;
        assert!(
            nothing.error.is_none(),
            "no folder named and none before: it runs from the home folder ({})",
            nothing.message
        );

        let wrote = op.run(&context(&services), &step(json!({ "action": "run", "mode": "full", "workingDirectory": folder, "command": "echo merhaba > a.txt", "timeoutSeconds": 900 }))).await;
        assert!(wrote.error.is_none(), "{}", wrote.message);
        let read = op
            .run(
                &context(&services),
                &step(json!({ "action": "run", "mode": "read", "command": "cat a.txt" })),
            )
            .await;
        assert!(read.error.is_none(), "{}", read.message);
        assert!(
            read.data.unwrap().to_string().contains("merhaba"),
            "the second step ran in the first one's folder"
        );

        let job = context(&services);
        let started = op.run(&job, &step(json!({ "action": "start_job", "mode": "full", "workingDirectory": folder, "command": "sleep 1; echo bitti" }))).await;
        assert!(started.error.is_none(), "{}", started.message);
        let waited = op.run(&context(&services), &step(json!({ "action": "wait_job", "jobId": job.command_id.to_string(), "waitSeconds": 30 }))).await;
        assert!(
            waited.data.unwrap().to_string().contains("bitti"),
            "{}",
            waited.message
        );

        let unknown = op
            .run(
                &context(&services),
                &step(json!({ "action": "get_job", "jobId": Uuid::new_v4().to_string() })),
            )
            .await;
        assert_eq!(unknown.error.unwrap().code, "TERMINAL_JOB_NOT_FOUND");
    }

    #[test]
    fn only_a_grant_this_computer_gave_itself_opens_a_session() {
        let op = TerminalStep {
            sessions: Sessions::new(),
            local: Arc::new(Mutex::new(Local::default())),
        };
        let ctx = context(&Services::temporary());
        let own = op.grant("/tmp/x", "read");
        let session = op.session(&ctx);
        assert!((session.grants)(&own));
        assert!(
            !(session.grants)(&Grant {
                signature: "phone-signed".into(),
                ..own.clone()
            }),
            "a grant from outside"
        );
        assert!(
            !(session.grants)(&Grant {
                id: Uuid::new_v4().to_string(),
                ..own.clone()
            }),
            "a grant this computer never gave"
        );
        assert!(
            !(session.grants)(&Grant {
                expires_at: Utc::now().timestamp() - 1,
                ..own
            }),
            "an expired one"
        );
    }
}
