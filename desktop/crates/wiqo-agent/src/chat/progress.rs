//! What this computer tells the person's other devices about one step of its work (POST /agent/progress, then the phone's chat).
//!
//! The shape is the backend's `agentRunEvents.ts`; `contracts/agent-events/examples` holds one of each and a test below keeps these builders equal to them.
//! A step is told twice: when it starts (the tool, the command, the folder) and when it ends (the exit code, how long it took, the end of what it printed,
//! the files it changed). Output and commands go out with the private values swapped for stand-ins (vault.rs), exactly as the model reads them, and only the end
//! of the output leaves: the whole of it stays here.

use std::path::PathBuf;

use serde_json::{json, Value};

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

/// The step has started.
pub fn running(id: &str, summary: &str, tool: &str, args: &Value, seal: &mut dyn FnMut(&str) -> String) -> Value {
    let mut step = json!({ "id": id, "summary": summary, "status": "running", "tool": tool });
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

/// What the tool printed, as one text, and the exit code when it was a command: the tool's result is JSON with `stdout`, `stderr` and `exitCode`, or a plain text.
fn printed(output: &str) -> (String, Option<i64>) {
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
    let mut step = json!({ "id": id, "summary": summary, "status": if ok { "succeeded" } else { "failed" }, "tool": tool, "durationMs": took_ms as u64 });
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

#[cfg(test)]
mod tests {
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
        for key in ["summary", "status", "tool", "command", "workdir", "exitCode", "durationMs", "output", "message", "files"] {
            assert_eq!(told["step"][key], expected["step"][key], "{key}");
        }
    }

    #[test]
    fn a_running_step_says_what_it_runs_and_where() {
        let mut seal = |text: &str| text.to_string();
        let told = running("local-1", "Testleri çalıştır", "shell", &json!({ "command": "pytest -q", "workdir": "~/Desktop/deneme/proje" }), &mut seal);
        same_fields(&told, &example("step_running"));
    }

    #[test]
    fn a_failed_command_tells_its_exit_code_and_the_end_of_its_output() {
        let mut seal = |text: &str| text.to_string();
        let output = json!({ "kind": "terminalResult", "stdout": "FAILED test_hesap.py::test_toplam - assert 5 == 6\n1 failed, 3 passed in 0.31s", "exitCode": 1 }).to_string();
        let args = json!({ "command": "pytest -q", "workdir": "~/Desktop/deneme/proje" });
        let told = finished("local-1", "Testleri çalıştır", "shell", &args, false, "Komut 1 koduyla bitti.", &output, 2_400, &[], &mut seal);
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
