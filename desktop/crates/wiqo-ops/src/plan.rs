//! `runFilePlan` (rova-backend docs/plan-packet.md): the steps in order, each held to the rules it would meet on its own, stopping at the first failure.
//! The phone approved the whole plan once; nothing here widens what it said. A step may name an earlier step's files: "$s1" is its first file, "$s1*" all.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use wiqo_contract::{is_valid_local_path, FilePolicy};
use wiqo_core::engine::{Context, Lane, Operation, Outcome};
use wiqo_core::storage::Settings;

use crate::fsx::Entry;
use crate::op::{data, outcome, payload, tool_failure, Done, Result};
use crate::Op;

const MAX_STEPS: usize = 6;
const MAX_BOUND_FILES: usize = 20;

pub(crate) fn operations() -> Vec<Op> {
    vec![Arc::new(RunFilePlan)]
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
struct Step {
    id: String,
    op: String,
    path: Option<String>,
    destination: Option<String>,
    query: Option<String>,
    alternatives: Option<Vec<String>>,
    kind: Option<String>,
    folder: Option<String>,
    modified_within_days: Option<i64>,
    sort: Option<String>,
    transfer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Plan {
    steps: Vec<Step>,
}

/// `PlanStepResult`.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct StepResult {
    id: String,
    op: String,
    status: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<Entry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transfer_id: Option<String>,
}

impl StepResult {
    fn new(step: &Step, status: &'static str, message: impl Into<String>) -> StepResult {
        StepResult {
            id: step.id.clone(),
            op: step.op.clone(),
            status,
            message: message.into(),
            files: None,
            text: None,
            artifact_id: None,
            transfer_id: None,
        }
    }
}

struct RunFilePlan;

#[async_trait]
impl Operation for RunFilePlan {
    fn name(&self) -> &'static str {
        "runFilePlan"
    }

    fn lane(&self) -> Lane {
        Lane::SideEffect
    }

    async fn run(&self, ctx: &Context, value: &Value) -> Outcome {
        outcome(run_plan(ctx, value).await)
    }
}

async fn run_plan(ctx: &Context, value: &Value) -> Result<Done> {
    let plan: Plan = payload(value, "filePlan", "dosya planı")?;
    if plan.steps.is_empty() || plan.steps.len() > MAX_STEPS {
        return Err(tool_failure("INVALID_PAYLOAD", "Plan 1-6 adım olmalı."));
    }
    let mut results: Vec<StepResult> = Vec::new();
    let mut files: HashMap<String, Vec<Entry>> = HashMap::new();
    let mut failure: Option<String> = None;
    for step in &plan.steps {
        if let Some(failure) = &failure {
            results.push(StepResult::new(
                step,
                "skipped",
                format!("Önceki adım başarısız: {failure}"),
            ));
            continue;
        }
        let result = run_step(ctx, step, &files).await;
        if let Some(found) = &result.files {
            files.insert(step.id.clone(), found.clone());
        }
        if result.status != "completed" {
            failure = Some(format!("{} — {}", step.id, result.message));
        }
        results.push(result);
    }
    let done = results
        .iter()
        .filter(|result| result.status == "completed")
        .count();
    let content = data("planResult", json!({ "steps": results }));
    match failure {
        None => Ok(Done::new(
            format!("Plan tamamlandı ({done}/{} adım).", results.len()),
            Some(content),
        )),
        Some(failure) => Ok(Done::partial(
            tool_failure("PLAN_STEP_FAILED", format!("Plan durdu: {failure}")),
            content,
        )),
    }
}

fn capability(op: &str) -> &'static str {
    match op {
        "search" | "list" => "device.files.list",
        "read" | "send" => "device.files.read",
        "move" => "device.files.move",
        _ => "device.files.copy",
    }
}

fn admissible(path: &str) -> bool {
    is_valid_local_path(path) && !FilePolicy::get().is_sensitive(path)
}

/// The files an operation's result lists (`{kind: "files", value: [...]}`).
fn listed(done: &Done) -> Option<Vec<Entry>> {
    let value = done.data.as_ref()?;
    if value["kind"] != "files" {
        return None;
    }
    serde_json::from_value(value["value"].clone()).ok()
}

fn operation(path: &str, destination: Option<String>) -> Value {
    json!({ "kind": "fileOperation", "value": { "path": path, "destination": destination } })
}

async fn run_step(ctx: &Context, step: &Step, files: &HashMap<String, Vec<Entry>>) -> StepResult {
    let fail = |message: String| StepResult::new(step, "failed", message);
    if !Settings::open(&ctx.services.data_dir).capability_enabled(capability(&step.op)) {
        return fail("Bu işlem Wiqo'nun Yetenekler ayarında kapalı.".into());
    }
    let raw = step.path.clone();
    // "$s1*": every file the earlier step found, one after another.
    if let Some(binding) = raw
        .as_deref()
        .and_then(|raw| raw.strip_prefix('$'))
        .and_then(|raw| raw.strip_suffix('*'))
    {
        let found: Vec<Entry> = files
            .get(binding)
            .map(|found| {
                found
                    .iter()
                    .filter(|entry| !entry.is_directory)
                    .take(MAX_BOUND_FILES)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if found.is_empty() {
            return fail("Önceki adımın sonucunda dosya yok.".into());
        }
        let mut made: Vec<Entry> = Vec::new();
        for entry in &found {
            if !admissible(&entry.path) {
                return fail(format!("{} plan içinde kullanılamaz.", entry.name));
            }
            let destination = if step.op == "convert" {
                None
            } else {
                step.destination
                    .as_ref()
                    .map(|folder| format!("{}/{}", folder.trim_end_matches('/'), entry.name))
            };
            let one = Step {
                path: Some(entry.path.clone()),
                destination,
                ..step.clone()
            };
            let result = Box::pin(run_step(ctx, &one, &HashMap::new())).await;
            if result.status != "completed" {
                return fail(format!("{}: {}", entry.name, result.message));
            }
            made.extend(result.files.unwrap_or_default());
        }
        let verb = match step.op.as_str() {
            "move" => "taşındı",
            "copy" => "kopyalandı",
            _ => "çevrildi",
        };
        let mut result =
            StepResult::new(step, "completed", format!("{} dosya {verb}.", found.len()));
        result.files = (!made.is_empty()).then_some(made);
        return result;
    }
    let path = match raw.as_deref() {
        Some(raw) if raw.starts_with('$') => {
            match files.get(&raw[1..]).and_then(|found| found.first()) {
                Some(bound) if admissible(&bound.path) => Some(bound.path.clone()),
                Some(_) => return fail("Bağlanan dosya plan içinde kullanılamaz.".into()),
                None => return fail("Önceki adımın sonucunda dosya yok.".into()),
            }
        }
        other => other.map(str::to_string),
    };
    let done = |result: Result<Done>| -> std::result::Result<Done, Box<StepResult>> {
        result.map_err(|error| Box::new(StepResult::new(step, "failed", error.message)))
    };
    let completed = |message: String, files: Option<Vec<Entry>>| {
        let mut result = StepResult::new(step, "completed", message);
        result.files = files;
        result
    };
    let blocking = |work: Box<dyn FnOnce() -> Result<Done> + Send>| async move {
        tokio::task::spawn_blocking(work).await.unwrap_or_else(|_| {
            Err(tool_failure(
                "OUTCOME_UNKNOWN",
                "Adım beklenmedik şekilde durdu.",
            ))
        })
    };
    match step.op.as_str() {
        "search" => {
            let request = json!({ "kind": "fileSearch", "value": {
                "query": step.query.clone().unwrap_or_default(), "folder": step.folder, "kind": step.kind,
                "modifiedWithinDays": step.modified_within_days, "sort": step.sort, "alternatives": step.alternatives,
            }});
            match done(blocking(Box::new(move || crate::search::search(&request))).await) {
                Ok(found) => {
                    let entries = listed(&found);
                    if entries.is_none() {
                        return fail("Arama sonucu okunamadı.".into());
                    }
                    completed(found.message, entries)
                }
                Err(failed) => *failed,
            }
        }
        "list" | "read" | "convert" | "move" | "copy" => {
            let Some(path) = path else {
                return fail("Dosya yolu yok.".into());
            };
            if matches!(step.op.as_str(), "move" | "copy") && step.destination.is_none() {
                return fail("Kaynak veya hedef yok.".into());
            }
            let (context, op, destination) =
                (ctx.clone(), step.op.clone(), step.destination.clone());
            let work: Box<dyn FnOnce() -> Result<Done> + Send> =
                Box::new(move || match op.as_str() {
                    "list" => crate::files::list_files(&context, &operation(&path, None)),
                    "read" => crate::files::read_file(&context, &operation(&path, None)),
                    "convert" => {
                        crate::convert::convert_file(&context, &operation(&path, destination))
                    }
                    "move" => {
                        crate::files::transfer_op(&context, &operation(&path, destination), false)
                    }
                    _ => crate::files::transfer_op(&context, &operation(&path, destination), true),
                });
            match done(blocking(work).await) {
                Ok(result) if step.op == "read" => {
                    let text: String = result
                        .data
                        .as_ref()
                        .and_then(|value| value["value"]["text"].as_str())
                        .unwrap_or_default()
                        .chars()
                        .take(4_000)
                        .collect();
                    let mut read = completed("Dosya okundu.".into(), None);
                    read.text = Some(text);
                    read
                }
                Ok(result) => {
                    let entries = listed(&result);
                    if entries.is_none() {
                        return fail("Sonuç doğrulanamadı.".into());
                    }
                    let message = if step.op == "list" {
                        format!("{} dosya.", entries.as_ref().map(Vec::len).unwrap_or(0))
                    } else {
                        result.message
                    };
                    completed(message, entries)
                }
                Err(failed) => *failed,
            }
        }
        "send" => {
            let Some(path) = path else {
                return fail("Dosya yolu veya aktarım bağlantısı yok.".into());
            };
            let Some(transfer) = step
                .transfer_id
                .as_deref()
                .and_then(|id| uuid::Uuid::parse_str(id).ok())
            else {
                return fail("Dosya yolu veya aktarım bağlantısı yok.".into());
            };
            // The phone registered the transfer under this id; the upload must use it, not the plan's own command id.
            let context = Context {
                command_id: transfer,
                services: ctx.services.clone(),
            };
            match done(crate::transfer::send_file(&context, &operation(&path, None)).await) {
                Ok(result) => {
                    let mut sent = completed("Dosya telefona gönderildi.".into(), None);
                    sent.artifact_id = result
                        .data
                        .as_ref()
                        .and_then(|value| value["value"].as_str())
                        .map(str::to_string);
                    sent.transfer_id = step.transfer_id.clone();
                    sent
                }
                Err(failed) => *failed,
            }
        }
        _ => fail("Bu plan adımı tanınmıyor.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn a_plan_lists_then_copies_every_listed_file_and_stops_at_a_failure() {
        let dir = std::env::temp_dir().join(format!("wiqo-plan-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("in")).unwrap();
        fs::create_dir_all(dir.join("out")).unwrap();
        let dir = dunce::canonicalize(dir).unwrap();
        fs::write(dir.join("in/a.txt"), b"a").unwrap();
        fs::write(dir.join("in/b.txt"), b"b").unwrap();
        let (input, output) = (
            dir.join("in").to_string_lossy().to_string(),
            dir.join("out").to_string_lossy().to_string(),
        );
        let plan = json!({ "kind": "filePlan", "value": { "steps": [
            { "id": "s1", "op": "list", "path": input },
            { "id": "s2", "op": "copy", "path": "$s1*", "destination": output },
            { "id": "s3", "op": "read", "path": "$s9" },
            { "id": "s4", "op": "list", "path": input },
        ]}});
        let ctx = Context::for_tests();
        let done = run_plan(&ctx, &plan).await.unwrap();
        assert!(dir.join("out/a.txt").exists() && dir.join("out/b.txt").exists());
        assert_eq!(
            done.error.as_ref().map(|error| error.code.as_str()),
            Some("PLAN_STEP_FAILED")
        );
        let steps = &done.data.unwrap()["value"]["steps"];
        let statuses: Vec<&str> = steps
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step["status"].as_str().unwrap())
            .collect();
        assert_eq!(statuses, ["completed", "completed", "failed", "skipped"]);
    }
}
