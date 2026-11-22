//! Does the file just written parse?
//!
//! A developer saves a file and the editor underlines what does not parse. The agent wrote whole files with no one underlining anything: a script whose second
//! half was a pasted-again copy of its first (`let matrix` declared twice) was "verified" by running a copy of its functions from another file, and handed over
//! with every button dead. This runs the language's own syntax check on a file after every write (not the program, only its parse), so the model hears
//! "SyntaxError ... line 116" in the same step and fixes it, as a person would the moment the red line appears. Only tools that are there are used; a
//! language this does not know, or a tool that is not installed, says nothing.

use std::path::Path;
use std::process::{Command, Stdio};

fn run(program: &str, args: &[&str]) -> Option<(bool, String)> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    Some((output.status.success(), text))
}

/// The syntax error of the file in its own words (a few lines), or None when it parses, or when it cannot be checked.
pub fn error_of(path: &Path) -> Option<String> {
    let name = path.to_string_lossy().to_string();
    let extension = path.extension()?.to_string_lossy().to_lowercase();
    let checked = match extension.as_str() {
        "js" | "mjs" | "cjs" => run("node", &["--check", &name]),
        "json" => run("python3", &["-c", "import json,sys; json.load(open(sys.argv[1], encoding='utf-8'))", &name]),
        "py" => run("python3", &["-c", "import ast,sys; ast.parse(open(sys.argv[1], encoding='utf-8').read(), sys.argv[1])", &name]),
        "sh" | "bash" => run("bash", &["-n", &name]),
        "swift" if cfg!(target_os = "macos") => run("swiftc", &["-parse", &name]),
        _ => None,
    };
    let (ok, text) = checked?;
    if ok {
        return None;
    }
    // The tool's own wording, without its own stack of internals: the first lines name the place and the mistake.
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(7)
        .collect();
    Some(lines.join("\n").chars().take(700).collect())
}

#[cfg(test)]
mod tests {
    use super::error_of;

    fn scratch(name: &str, text: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wiqo-syntax-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("folder");
        let path = dir.join(name);
        std::fs::write(&path, text).expect("file");
        path
    }

    #[test]
    fn a_script_with_a_declaration_repeated_is_caught_and_a_clean_one_is_not() {
        if Command_exists("node") {
            let broken = scratch(
                "broken.js",
                "let matrix = null;\nfunction a() {}\nlet matrix = null;\nfunction b(\n",
            );
            assert!(error_of(&broken).expect("an error").contains("SyntaxError"));
            assert!(error_of(&scratch(
                "fine.js",
                "let matrix = null;\nfunction a() { return matrix; }\n"
            ))
            .is_none());
        }
    }

    #[test]
    fn json_python_and_shell_are_checked_and_unknown_files_are_left_alone() {
        if Command_exists("python3") {
            assert!(error_of(&scratch("bad.json", "{\"a\": 1,}")).is_some());
            assert!(error_of(&scratch("ok.json", "{\"a\": 1}")).is_none());
            assert!(error_of(&scratch("bad.py", "def f(:\n  pass\n")).is_some());
            assert!(error_of(&scratch("ok.py", "def f():\n    return 1\n")).is_none());
        }
        assert!(error_of(&scratch("bad.sh", "if then fi\n")).is_some());
        assert!(error_of(&scratch("notes.txt", "{{{{ not code")).is_none());
        assert!(error_of(&scratch("page.html", "<div><span></div>")).is_none());
    }

    #[allow(non_snake_case)]
    fn Command_exists(program: &str) -> bool {
        std::process::Command::new(program)
            .arg("--version")
            .output()
            .is_ok()
    }
}
