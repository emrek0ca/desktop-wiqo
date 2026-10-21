//! A short description of this computer's developer environment for the model: OS, shell, which toolchains exist (with versions) and the git
//! projects in the usual folders (names only). Computed once per launch, off the async threads.

use serde_json::{json, Value};

use super::shell::{self, Control};

const TOOLS: [(&str, &str); 18] = [
    ("node", "--version"),
    ("npm", "--version"),
    ("pnpm", "--version"),
    ("yarn", "--version"),
    ("bun", "--version"),
    ("python3", "--version"),
    ("pip3", "--version"),
    ("git", "--version"),
    ("brew", "--version"),
    ("docker", "--version"),
    ("go", "version"),
    ("cargo", "--version"),
    ("rustc", "--version"),
    ("swift", "--version"),
    ("xcodebuild", "-version"),
    ("java", "-version"),
    ("ruby", "--version"),
    ("rg", "--version"),
];

fn tools() -> Vec<String> {
    if cfg!(windows) {
        let script = TOOLS
            .iter()
            .map(|(name, flag)| format!("if (Get-Command {name} -ErrorAction SilentlyContinue) {{ $v = (& {name} {flag} 2>&1 | Select-Object -First 1); Write-Output \"{name}|x|$v\" }}"))
            .collect::<Vec<_>>()
            .join("; ");
        let mut command = crate::process::command("powershell");
        command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        return parse(&shell::run(command, 20, &Control::default()).stdout);
    }
    let script = TOOLS
        .iter()
        .map(|(name, flag)| format!("if p=$(command -v {name} 2>/dev/null); then v=$({name} {flag} 2>&1 | head -1); printf '%s|%s|%s\\n' \"{name}\" \"$p\" \"$v\"; fi"))
        .collect::<Vec<_>>()
        .join("; ");
    let mut command = crate::process::command(shell::default_shell());
    command
        .args(["-lc", &script])
        .env_clear()
        .env("HOME", super::policy::home())
        .env("TERM", "dumb");
    if let Some(path) = shell::interactive_path() {
        command.env("PATH", path);
    }
    parse(&shell::run(command, 12, &Control::default()).stdout)
}

fn parse(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() != 3 {
                return None;
            }
            let version: String = parts[2]
                .replace("version", "")
                .trim()
                .chars()
                .take(40)
                .collect();
            Some(format!("{} {version}", parts[0]))
        })
        .take(20)
        .collect()
}

fn projects() -> Vec<String> {
    let home = super::policy::home();
    let mut found = Vec::new();
    for folder in [
        "Desktop",
        "Documents",
        "Developer",
        "Projects",
        "Code",
        "code",
        "src",
        "GitHub",
        "source/repos",
    ] {
        let Ok(children) = std::fs::read_dir(home.join(folder)) else {
            continue;
        };
        for child in children.filter_map(|child| child.ok()) {
            if child.path().join(".git").exists() {
                found.push(format!(
                    "~/{folder}/{}",
                    child.file_name().to_string_lossy()
                ));
                if found.len() >= 12 {
                    return found;
                }
            }
        }
    }
    found
}

fn os() -> String {
    let name = sysinfo::System::name().unwrap_or_else(|| std::env::consts::OS.into());
    let version = sysinfo::System::os_version().unwrap_or_default();
    format!("{name} {version}").trim().to_string()
}

pub fn profile() -> Value {
    let shell = if cfg!(windows) {
        "powershell".to_string()
    } else {
        shell::default_shell()
            .rsplit('/')
            .next()
            .unwrap_or("sh")
            .to_string()
    };
    json!({ "os": os(), "arch": std::env::consts::ARCH, "shell": shell, "tools": tools(), "projects": projects() })
}
