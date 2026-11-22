//! What this computer tells the person's other devices about one step of its work (POST /agent/progress, then the phone's chat).
//!
//! The shape follows the shared agent-event fixtures. These builders can retain local
//! diagnostic fields; the local run projects them to the shared wire before reporting.

use std::path::PathBuf;

use serde_json::{json, Value};
use uuid::Uuid;

/// How much of the end of a step's output is told. The server keeps at most this much too.
const OUTPUT_TAIL: usize = 2_000;
const COMMAND_CHARS: usize = 400;
const FILES_MOST: usize = 20;

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}

fn tail(text: &str, most: usize) -> String {
    let count = text.chars().count();
    if count <= most {
        return text.trim_end().to_string();
    }
    text.chars().skip(count - most).collect::<String>().trim_end().to_string()
}

fn head(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

fn lines(text: &str) -> u64 {
    if text.is_empty() { 0 } else { text.lines().count() as u64 }
}

/// A file a step is about to touch and whether it exists now: `created` or `modified` is told from this once the step is done.
pub struct Touched {
    pub path: PathBuf,
    pub existed: bool,
}

pub fn touched(paths: Vec<PathBuf>) -> Vec<Touched> {
    paths.into_iter().map(|path| Touched { existed: path.exists(), path }).collect()
}

/// An id that is not "local-<uuid>" still maps to the same UUID every time (FNV-1a over 128 bits, with the version and variant bits of a v4).
fn stable_uuid(text: &str) -> Uuid {
    let mut hash: u128 = 0x6c62272e07bb014262b821756295c58d;
    for byte in text.bytes() {
        hash ^= byte as u128;
        hash = hash.wrapping_mul(0x0000000001000000000000000000013b);
    }
    Uuid::from_u128((hash & !(0xf << 76) | (0x4 << 76)) & !(0x3 << 62) | (0x2 << 62))
}

/// The fields every client requires of a step (the phone's `ExecutionStepUpdate`): a UUID id (the one inside "local-<uuid>", so the server keeps it as it is),
/// the desktop's own id beside it, and the kind and the two flags. The same step always gets the same id.
fn base(id: &str, summary: &str, status: &str, tool: &str) -> Value {
    let uuid = Uuid::parse_str(id.trim_start_matches("local-")).unwrap_or_else(|_| stable_uuid(id));
    json!({ "id": uuid.to_string(), "localId": id, "kind": "device", "requiresConfirmation": false, "requiresUserPresence": false, "summary": summary, "status": status, "tool": tool })
}

/// The step has started.
pub fn running(id: &str, summary: &str, tool: &str, args: &Value, seal: &mut dyn FnMut(&str) -> String) -> Value {
    let mut step = base(id, summary, "running", tool);
    if tool == "shell" {
        let command = text(&args["command"]);
        if !command.is_empty() {
            step["command"] = json!(seal(&head(command, COMMAND_CHARS)));
        }
    }
    for key in ["workdir", "path"] {
        if let Some(folder) = args[key].as_str().filter(|folder| !folder.is_empty()) {
            step["workdir"] = json!(head(folder, 300));
            break;
        }
    }
    json!({ "type": "step_update", "step": step })
}

/// The step is still running and this is the end of what it has printed so far: the same step, told again with its `output`, so every client that shows a step
/// shows a long command's output as it comes (no event of its own to learn).
pub fn running_output(id: &str, summary: &str, tool: &str, args: &Value, printed: &str, seal: &mut dyn FnMut(&str) -> String) -> Value {
    let mut told = running(id, summary, tool, args, seal);
    if !printed.trim().is_empty() {
        told["step"]["output"] = json!(seal(&tail(printed, OUTPUT_TAIL)));
    }
    told
}

/// What the tool printed, as one text, and the exit code when it was a command: the tool's result is JSON with `stdout`, `stderr` and `exitCode`, or a plain text.
pub(super) fn printed(output: &str) -> (String, Option<i64>) {
    match serde_json::from_str::<Value>(output) {
        Ok(Value::Object(object)) => {
            let exit = object.get("exitCode").and_then(Value::as_i64);
            let mut parts = Vec::new();
            for key in ["stdout", "stderr", "output", "text"] {
                if let Some(part) = object.get(key).and_then(Value::as_str).filter(|part| !part.trim().is_empty()) {
                    parts.push(part.trim_end().to_string());
                }
            }
            (parts.join("\n"), exit)
        }
        _ => (output.to_string(), None),
    }
}

fn changed_files(tool: &str, args: &Value, touched: &[Touched]) -> Vec<Value> {
    let change = |path: &PathBuf| if touched.iter().any(|known| known.path == *path && !known.existed) { "created" } else { "modified" };
    let mut files = Vec::new();
    match tool {
        "write_file" => {
            if let Some(known) = touched.first() {
                let added = lines(text(&args["content"]));
                files.push(json!({ "path": known.path.display().to_string(), "change": change(&known.path), "added": added }));
            }
        }
        "edit_file" => {
            if let Some(known) = touched.first() {
                files.push(json!({ "path": known.path.display().to_string(), "change": "modified", "added": lines(text(&args["replace"])), "removed": lines(text(&args["find"])) }));
            }
        }
        "apply_patch" => {
            for (known, edit) in touched.iter().zip(args["edits"].as_array().into_iter().flatten()) {
                let patch = text(&edit["patch"]);
                let added = patch.lines().filter(|line| line.starts_with('+') && !line.starts_with("+++")).count();
                let removed = patch.lines().filter(|line| line.starts_with('-') && !line.starts_with("---")).count();
                files.push(json!({ "path": known.path.display().to_string(), "change": change(&known.path), "added": added, "removed": removed }));
            }
        }
        _ => {}
    }
    files.truncate(FILES_MOST);
    files
}

/// The step has ended, well or not.
#[allow(clippy::too_many_arguments)]
pub fn finished(
    id: &str,
    summary: &str,
    tool: &str,
    args: &Value,
    ok: bool,
    brief: &str,
    output: &str,
    took_ms: u128,
    touched: &[Touched],
    seal: &mut dyn FnMut(&str) -> String,
) -> Value {
    let mut step = base(id, summary, if ok { "succeeded" } else { "failed" }, tool);
    step["durationMs"] = json!(took_ms as u64);
    if tool == "shell" {
        let command = text(&args["command"]);
        if !command.is_empty() {
            step["command"] = json!(seal(&head(command, COMMAND_CHARS)));
        }
    }
    if let Some(folder) = ["workdir", "path"].iter().find_map(|key| args[*key].as_str().filter(|folder| !folder.is_empty())) {
        step["workdir"] = json!(head(folder, 300));
    }
    let (shown, exit) = printed(output);
    if let Some(exit) = exit {
        step["exitCode"] = json!(exit);
    }
    // Reading a file would put its whole text on the phone: only what a command printed, or a short result, is told.
    if !shown.trim().is_empty() && tool != "read_file" {
        step["output"] = json!(seal(&tail(&shown, OUTPUT_TAIL)));
    }
    if !ok && !brief.is_empty() {
        step["message"] = json!(head(brief, 400));
    }
    if ok {
        let files = changed_files(tool, args, touched);
        if !files.is_empty() {
            step["files"] = json!(files);
        }
    }
    json!({ "type": "step_update", "step": step })
}

pub fn background_started(id: &str) -> Value {
    running(id, "Komut arka planda çalışıyor", "shell", &Value::Null, &mut str::to_owned)
}

pub fn background_finished(id: &str, success: bool, uncertain: bool, exit_code: Option<i64>) -> Value {
    let status = if uncertain { "blocked" } else if success { "succeeded" } else { "failed" };
    let summary = if uncertain { "Arka plan işinin sonucu bilinmiyor" } else { "Arka plan işi sona erdi" };
    let mut step = base(id, summary, status, "shell");
    if let Some(code) = exit_code {
        step["exitCode"] = json!(code);
    }
    json!({ "type": "step_update", "step": step })
}

/// A step the server planned (chat.rs runs it on this computer), told in the same shape. Its `id` is already a UUID (the server's), its words are the server's
/// `summary`; the PC's own status words (`completed`, `denied`) are the phone's (`succeeded`, `denied`).
pub fn planned(step: &Value, status: &str, message: Option<&str>, summary: Option<&str>) -> Value {
    let id = text(&step["id"]);
    let words = summary.filter(|words| !words.is_empty()).unwrap_or_else(|| text(&step["summary"]));
    let mut told = base(id, words, match status { "completed" => "succeeded", other => other }, "");
    if let Some(object) = told.as_object_mut() {
        object.remove("tool");
        object.insert("requiresConfirmation".into(), json!(step["requiresConfirmation"] == true));
        object.insert("requiresUserPresence".into(), json!(step["requiresUserPresence"] == true));
    }
    if let Some(message) = message.filter(|message| !message.is_empty()) {
        told["message"] = json!(head(message, 400));
    }
    json!({ "type": "step_update", "step": told })
}

/// The end of a run, in the shape of the server's own `done` (reply, steps, executionState, artifacts, backendConfirmations, executionTarget) plus what only
/// this computer knows. The phone decodes it like any other turn's end; a `done` without `steps` did not decode.
pub fn done(reply: &str, run_id: &str, private_kept: usize, outcome: wiqo_contract::schema_generated::AgentRunOutcome) -> Value {
    let state = if outcome.state == wiqo_contract::schema_generated::AgentRunOutcomeState::Done {
        "complete_backend"
    } else {
        "partial_success"
    };
    json!({
        "type": "done", "reply": reply, "steps": [], "executionState": state, "artifacts": [], "backendConfirmations": [],
        "executionTarget": "device", "localRun": run_id, "privateKept": private_kept, "outcome": outcome
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_running_step_tells_the_end_of_what_it_has_printed() {
        use super::running_output;
        use serde_json::json;
        let mut seal = |text: &str| text.to_string();
        let printed = format!("{}\nlast line", "x".repeat(5_000));
        let told = running_output("local-6f0c2b1e-0d4a-4d55-9b0e-0a1c7a5f2e10", "Testleri çalıştır", "shell", &json!({ "command": "pytest -q" }), &printed, &mut seal);
        assert_eq!(told["step"]["status"], "running");
        assert_eq!(told["step"]["command"], "pytest -q");
        let shown = told["step"]["output"].as_str().unwrap_or_default();
        assert!(shown.ends_with("last line") && shown.chars().count() <= 2_000);
        assert!(running_output("local-1", "x", "shell", &json!({}), "  \n", &mut seal)["step"].get("output").is_none());
    }

    #[test]
    fn a_planned_step_is_told_in_the_same_shape_as_the_agents() {
        use super::planned;
        use serde_json::json;
        let step = json!({ "id": "6f0c2b1e-0d4a-4d55-9b0e-0a1c7a5f2e10", "summary": "Dosyayı kopyala", "requiresConfirmation": true });
        let told = planned(&step, "completed", None, None);
        assert_eq!(told["step"]["status"], "succeeded");
        assert_eq!(told["step"]["id"], "6f0c2b1e-0d4a-4d55-9b0e-0a1c7a5f2e10");
        assert_eq!(told["step"]["kind"], "device");
        assert_eq!(told["step"]["requiresConfirmation"], true);
        assert_eq!(told["step"]["requiresUserPresence"], false);
        assert!(told["step"].get("tool").is_none());
        let refused = planned(&step, "denied", Some("Onay verilmedi."), Some("Dosyayı kopyala · 40%"));
        assert_eq!(refused["step"]["status"], "denied");
        assert_eq!(refused["step"]["message"], "Onay verilmedi.");
        assert_eq!(refused["step"]["summary"], "Dosyayı kopyala · 40%");
    }

    use super::*;

    fn example(name: &str) -> Value {
        let text = match name {
            "step_running" => include_str!("../../../../../contracts/agent-events/examples/step_running.json"),
            "step_failed" => include_str!("../../../../../contracts/agent-events/examples/step_failed.json"),
            "step_files" => include_str!("../../../../../contracts/agent-events/examples/step_files.json"),
            other => panic!("no example {other}"),
        };
        serde_json::from_str(text).expect("example is JSON")
    }

    /// The example is the shape the phones read (the server added the UUID id and the fields every client requires); what this computer says must equal it field for field.
    fn same_fields(told: &Value, expected: &Value) {
        for key in ["id", "localId", "kind", "requiresConfirmation", "requiresUserPresence", "summary", "status", "tool", "command", "workdir", "exitCode", "durationMs", "output", "message", "files"] {
            assert_eq!(told["step"][key], expected["step"][key], "{key}");
        }
    }

    #[test]
    fn the_end_of_a_run_is_the_servers_done_with_the_local_run_added() {
        let expected: Value = serde_json::from_str(include_str!("../../../../../contracts/agent-events/examples/done.json")).expect("example is JSON");
        let outcome = wiqo_contract::schema_generated::AgentRunOutcome {
            state: wiqo_contract::schema_generated::AgentRunOutcomeState::Done,
            verified: true,
        };
        let mut actual = done("Testler geçiyor.", "6f0c2b1e-0d4a-4d55-9b0e-0a1c7a5f2e10", 0, outcome);
        assert_eq!(actual.as_object_mut().unwrap().remove("outcome"), Some(json!({ "state": "done", "verified": true })));
        assert_eq!(actual, expected);
    }

    #[test]
    fn a_step_leaves_with_a_uuid_id_and_every_field_the_phone_requires() {
        let mut seal = |text: &str| text.to_string();
        let told = running("local-6f0c2b1e-0d4a-4d55-9b0e-0a1c7a5f2e10", "x", "shell", &json!({ "command": "ls" }), &mut seal);
        assert_eq!(told["step"]["id"], "6f0c2b1e-0d4a-4d55-9b0e-0a1c7a5f2e10");
        assert_eq!(told["step"]["localId"], "local-6f0c2b1e-0d4a-4d55-9b0e-0a1c7a5f2e10");
        assert_eq!(told["step"]["kind"], "device");
        assert_eq!(told["step"]["requiresConfirmation"], false);
        assert_eq!(told["step"]["requiresUserPresence"], false);
        assert!(Uuid::parse_str(told["step"]["id"].as_str().unwrap_or_default()).is_ok());
    }

    #[test]
    fn a_running_step_says_what_it_runs_and_where() {
        let mut seal = |text: &str| text.to_string();
        let told = running("local-6f0c2b1e-0d4a-4d55-9b0e-0a1c7a5f2e10", "Testleri çalıştır", "shell", &json!({ "command": "pytest -q", "workdir": "~/Desktop/deneme/proje" }), &mut seal);
        same_fields(&told, &example("step_running"));
    }

    #[test]
    fn a_failed_command_tells_its_exit_code_and_the_end_of_its_output() {
        let mut seal = |text: &str| text.to_string();
        let output = json!({ "kind": "terminalResult", "stdout": "FAILED test_hesap.py::test_toplam - assert 5 == 6\n1 failed, 3 passed in 0.31s", "exitCode": 1 }).to_string();
        let args = json!({ "command": "pytest -q", "workdir": "~/Desktop/deneme/proje" });
        let told = finished("local-6f0c2b1e-0d4a-4d55-9b0e-0a1c7a5f2e10", "Testleri çalıştır", "shell", &args, false, "Komut 1 koduyla bitti.", &output, 2_400, &[], &mut seal);
        same_fields(&told, &example("step_failed"));
    }

    #[test]
    fn an_edit_tells_the_files_it_changed() {
        let mut seal = |text: &str| text.to_string();
        let dir = PathBuf::from("~/Desktop/deneme/proje");
        let args = json!({ "path": "~/Desktop/deneme/proje/hesap.py", "find": "a - b", "replace": "a + b" });
        let known = vec![Touched { path: dir.join("hesap.py"), existed: true }];
        let told = finished("local-2", "hesap.py düzeltildi", "edit_file", &args, true, "", "{}", 40, &known, &mut seal);
        assert_eq!(told["step"]["files"][0]["change"], "modified");
        assert_eq!(told["step"]["files"][0]["added"], 1);
        assert_eq!(told["step"]["files"][0]["removed"], 1);
        // The example edits two files (one created); the first is this one.
        assert_eq!(told["step"]["files"][0]["path"], example("step_files")["step"]["files"][0]["path"]);
    }

    #[test]
    fn private_values_and_long_output_do_not_leave_as_they_are() {
        let mut seal = |text: &str| text.replace("hunter2", "‹gizli-1›");
        let output = format!("{}\nparola hunter2", "x".repeat(5_000));
        let told = finished("local-3", "komut", "shell", &json!({ "command": "echo hunter2" }), true, "", &output, 5, &[], &mut seal);
        let shown = told["step"]["output"].as_str().unwrap_or_default();
        assert!(shown.chars().count() <= OUTPUT_TAIL + 20);
        assert!(!shown.contains("hunter2") && shown.contains("‹gizli-1›"));
        assert!(!told["step"]["command"].as_str().unwrap_or_default().contains("hunter2"));
    }

    #[test]
    fn reading_a_file_does_not_put_its_text_on_the_phone() {
        let mut seal = |text: &str| text.to_string();
        let told = finished("local-4", "dosyayı oku", "read_file", &json!({ "path": "/a/b.txt" }), true, "", "gizli içerik", 3, &[], &mut seal);
        assert!(told["step"].get("output").is_none());
    }
}
