//! What a terminal session may do (`RovaTerminalPolicy` on the phone and the old Mac app): commands that never run, what counts as read-only, where
//! a path may point, and the credentials masked before output leaves the computer.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

pub const READ_WITHOUT_GRANT_ID: &str = "read-without-grant";
pub const SCRATCH_FOLDER: &str = "@scratch";

fn compiled(patterns: &[&str]) -> Vec<Regex> {
    patterns
        .iter()
        .map(|pattern| Regex::new(pattern).expect("policy pattern"))
        .collect()
}

fn forbidden() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        compiled(&[
            r"(^|[\s;&|(])sudo(\s|$)",
            r"(^|[\s;&|(])su(\s|$)",
            r"\brm\s+(-[a-zA-Z]*[rf][a-zA-Z]*\s+)+(/|~|\$HOME|\*)(\s|$|/\s|/\*)",
            r"\b(mkfs|fdisk|diskutil\s+(erase|partition|zero)|dd\s+if=)",
            r"\b(shutdown|reboot|halt|launchctl|csrutil|nvram|spctl|kextload)\b",
            r"\b(curl|wget)\b[^|]*\|\s*(sudo\s+)?(sh|bash|zsh|python3?)\b",
            r"\bchmod\s+-R\s+[0-7]{3,4}\s+(/|~)(\s|$)",
            r"\bchown\s+-R\b[^;&|]*\s(/|~)(\s|$)",
            r":\(\)\s*\{\s*:\|:&\s*\};:",
            r"\bsecurity\s+(dump-keychain|find-generic-password|find-internet-password)",
            // AppleScript runs under a full grant like any command; sending mail or messages as the person and asking for admin rights do not.
            r#"\bosascript\b[\s\S]*(application\s+"(Mail|Messages)"|administrator\s+privileges)"#,
            r"\bcrontab\s+-r\b",
            // Windows: the same classes (formatting, boot settings, power, whole-drive deletes, elevation).
            r"(?i)\b(format-volume|clear-disk|initialize-disk|bcdedit|diskpart|stop-computer|restart-computer|set-executionpolicy)\b",
            r"(?i)\bremove-item\b[^;|]*-recurse[^;|]*\s([a-z]:\\?|\\|~|\$env:userprofile)(\s|$)",
            r"(?i)\breg\s+delete\s+hk(lm|cr)",
            r"(?i)\b(iwr|irm|invoke-webrequest|invoke-restmethod)\b[^|]*\|\s*(iex|invoke-expression)\b",
            r"(?i)\bstart-process\b[^;|]*-verb\s+runas",
        ])
    })
}

pub fn is_forbidden(command: &str) -> bool {
    forbidden().iter().any(|pattern| pattern.is_match(command))
}

const READ_ONLY_PROGRAMS: &[&str] = &[
    "ls",
    "pwd",
    "cat",
    "head",
    "tail",
    "wc",
    "grep",
    "egrep",
    "rg",
    "find",
    "file",
    "stat",
    "du",
    "df",
    "tree",
    "which",
    "whereis",
    "type",
    "echo",
    "date",
    "uname",
    "sw_vers",
    "whoami",
    "ps",
    "sort",
    "uniq",
    "cut",
    "jq",
    "diff",
    "cmp",
    "basename",
    "dirname",
    "realpath",
    "md5",
    "shasum",
    "lsof",
    "uptime",
    "hostname",
    "xcodebuild",
    "system_profiler",
    "cd",
    "mdls",
    "ifconfig",
    "vm_stat",
    "ioreg",
    "ping",
    "nslookup",
    "dig",
    "host",
    "pbpaste",
    "free",
    "lsblk",
    "lscpu",
    "ip",
    "md5sum",
    "sha256sum",
    "get-childitem",
    "get-content",
    "get-location",
    "get-process",
    "get-date",
    "dir",
];
const VERSION_ONLY_PROGRAMS: &[&str] = &[
    "node", "npm", "npx", "python", "python3", "pip", "pip3", "brew", "java", "swift", "go",
    "cargo", "ruby", "git", "docker", "pnpm", "yarn", "rustc",
];
const GIT_READ: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "branch",
    "remote",
    "rev-parse",
    "ls-files",
    "blame",
    "describe",
    "tag",
    "shortlog",
    "config",
];

fn package_read(program: &str) -> &'static [&'static str] {
    match program {
        "npm" => &["ls", "list", "outdated", "view"],
        "pnpm" => &["ls", "list", "outdated"],
        "yarn" => &["list", "outdated", "info"],
        "pip" | "pip3" => &["list", "show", "freeze"],
        "brew" => &["list", "info", "outdated", "search"],
        "docker" => &["ps", "images", "version", "info"],
        // System readings: read-only only with these first arguments (`defaults read` is, `defaults write` is not).
        "pmset" => &["-g"],
        "defaults" => &["read", "domains", "read-type", "find"],
        "scutil" => &["--get", "--dns", "--proxy", "--nwi"],
        "ipconfig" => &[
            "getifaddr",
            "getpacket",
            "ifcount",
            "getoption",
            "getsummary",
        ],
        "sips" => &["-g"],
        "plutil" => &["-p", "-lint"],
        "top" => &["-l"],
        "sysctl" => &["-n", "-a"],
        "networksetup" => &[
            "-listallnetworkservices",
            "-listallhardwareports",
            "-getinfo",
            "-getairportnetwork",
            "-getairportpower",
            "-getdnsservers",
            "-getwebproxy",
            "-getcomputername",
        ],
        _ => &[],
    }
}

/// Conservative: anything that is not clearly an inspection needs a "full" grant.
pub fn is_read_only(command: &str) -> bool {
    static SHAPES: OnceLock<(Regex, Regex, Regex)> = OnceLock::new();
    let (absolute, redirect_ok, flags) = SHAPES.get_or_init(|| {
        (
            Regex::new(r#"(^|[\s"'=])/"#).unwrap(),
            Regex::new(r">\s*/dev/null|>&1").unwrap(),
            Regex::new(r"^-(delete|exec|execdir|ok|okdir|fprint)").unwrap(),
        )
    });
    let text = command.replace("2>&1", "");
    // Read grants accept relative paths only: inspection commands cannot name an outside path.
    if text.contains("..") || text.contains('~') || text.contains('$') || absolute.is_match(&text) {
        return false;
    }
    let redirects = redirect_ok.replace_all(&text, "");
    if redirects.contains('`')
        || redirects.contains('<')
        || redirects.contains('>')
        || redirects.replace("&&", "").contains('&')
    {
        return false;
    }
    let segments: Vec<String> = command
        .replace("||", "\n")
        .replace("&&", "\n")
        .replace([';', '|'], "\n")
        .split('\n')
        .map(|segment| segment.trim().to_string())
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.is_empty() {
        return false;
    }
    segments.iter().all(|segment| {
        let words: Vec<&str> = segment.split_whitespace().collect();
        let Some(first) = words.first() else {
            return false;
        };
        let program = first.rsplit('/').next().unwrap_or(first).to_lowercase();
        let rest = &words[1..];
        match program.as_str() {
            "find" => !rest.iter().any(|word| flags.is_match(word)),
            // Spotlight content search: read-only only when kept to the session folder.
            "mdfind" => rest.contains(&"-onlyin"),
            "git" => {
                let Some(sub) = rest.first() else {
                    return false;
                };
                if !GIT_READ.contains(sub) {
                    return false;
                }
                match *sub {
                    "config" => rest
                        .iter()
                        .any(|word| matches!(*word, "--get" | "--list" | "-l")),
                    "branch" => !rest.iter().any(|word| {
                        word.starts_with("-d")
                            || word.starts_with("-D")
                            || word.starts_with("-m")
                            || word.starts_with("-M")
                    }),
                    "tag" => !rest
                        .iter()
                        .any(|word| word.starts_with("-a") || word.starts_with("-d")),
                    _ => true,
                }
            }
            _ => {
                if rest
                    .first()
                    .is_some_and(|sub| package_read(&program).contains(sub))
                {
                    return true;
                }
                if VERSION_ONLY_PROGRAMS.contains(&program.as_str()) {
                    return rest.len() == 1
                        && matches!(rest[0], "-v" | "--version" | "-V" | "version");
                }
                READ_ONLY_PROGRAMS.contains(&program.as_str())
            }
        }
    })
}

pub fn home() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    dunce::canonicalize(&home).unwrap_or(home)
}

/// A path with its existing part resolved (symlinks followed) and the rest appended as written.
fn resolved(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
        }
    }
    let mut out = dunce::canonicalize(&existing).unwrap_or(existing);
    for name in rest.into_iter().rev() {
        out.push(name);
    }
    out
}

fn has_parent_step(text: &str) -> bool {
    Path::new(text)
        .components()
        .any(|component| component == Component::ParentDir)
}

fn expand(text: &str, base: Option<&Path>) -> Option<PathBuf> {
    if text == "~" {
        return Some(home());
    }
    if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        return Some(home().join(rest));
    }
    let path = PathBuf::from(text);
    if path.is_absolute() {
        return Some(path);
    }
    base.map(|base| base.join(path))
}

/// "~/a" → the real, symlink-resolved folder; `None` for relative paths or "..".
pub fn resolve_directory(raw: &str) -> Option<PathBuf> {
    let text = raw.trim();
    if text.is_empty() || has_parent_step(text) {
        return None;
    }
    expand(text, None).map(|path| resolved(&path))
}

pub fn is_within(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

/// The empty folder a read that names no folder runs in, so a relative path in the command reaches nothing of the person's.
pub fn scratch_directory() -> PathBuf {
    let path = std::env::temp_dir().join("wiqo-terminal-scratch");
    let _ = std::fs::create_dir_all(&path);
    dunce::canonicalize(&path).unwrap_or(path)
}

/// The folder a read may run in without a card: under a folder of the person's own inside home (not home itself, not Library/AppData, not a hidden
/// folder such as .ssh). The read stays inside this folder.
pub fn read_folder_without_grant(raw: &str) -> Option<PathBuf> {
    if raw == SCRATCH_FOLDER {
        return Some(scratch_directory());
    }
    let home = home();
    let directory = resolve_directory(raw)?;
    if directory == home || !is_within(&directory, &home) {
        return None;
    }
    let first = directory
        .strip_prefix(&home)
        .ok()?
        .components()
        .next()?
        .as_os_str()
        .to_string_lossy()
        .to_string();
    if first.is_empty() || first.starts_with('.') || first == "Library" || first == "AppData" {
        return None;
    }
    Some(directory)
}

/// A full grant's approved folder that does not exist yet may be created, when it is strictly inside the home folder.
pub fn creatable_directory(root: &Path, mode: &str) -> bool {
    let home = home();
    mode == "full" && root != home && is_within(root, &home) && !root.exists()
}

fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

/// The candidate path, lexically normalized; `None` for NUL, "..", or an empty text.
fn candidate(raw: &str, working: &Path) -> Option<PathBuf> {
    let text = raw.trim();
    if text.is_empty() || text.contains('\0') || has_parent_step(text) {
        return None;
    }
    let path = expand(text, Some(working))?;
    Some(
        path.components()
            .filter(|component| *component != Component::CurDir)
            .collect(),
    )
}

/// An existing regular file inside `root`, with no symlink anywhere on the way.
pub fn resolve_existing_file(raw: &str, working: &Path, root: &Path) -> Option<PathBuf> {
    let path = candidate(raw, working)?;
    if !is_within(&path, root) {
        return None;
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if !current.exists() || (current.parent().is_some() && is_link(&current)) {
            return None;
        }
    }
    if !path.is_file() {
        return None;
    }
    let real = dunce::canonicalize(&path).ok()?;
    is_within(&real, root).then_some(real)
}

/// A file to create or overwrite inside the grant: its folder exists inside `root` with no symlink on the way, and an existing target is a regular file.
pub fn resolve_writable_file(raw: &str, working: &Path, root: &Path) -> Option<PathBuf> {
    if raw.trim().ends_with('/') || raw.trim().ends_with('\\') {
        return None;
    }
    let path = candidate(raw, working)?;
    if !is_within(&path, root) || path == root {
        return None;
    }
    let parent = path.parent()?;
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component);
        if !current.is_dir() || (current.parent().is_some() && is_link(&current)) {
            return None;
        }
    }
    if std::fs::symlink_metadata(&path).is_ok() && (is_link(&path) || !path.is_file()) {
        return None;
    }
    let real_parent = dunce::canonicalize(parent).ok()?;
    is_within(&real_parent, root).then(|| real_parent.join(path.file_name().unwrap_or_default()))
}

/// Masks credentials before output leaves the computer: known token formats, private keys, and the value of KEY=, TOKEN=, SECRET=, PASSWORD= lines.
pub fn redact_secrets(text: &str) -> String {
    static PATTERNS: OnceLock<(Vec<Regex>, Regex)> = OnceLock::new();
    let (patterns, assignment) = PATTERNS.get_or_init(|| {
        (
            compiled(&[
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
                r"\b(AKIA|ASIA)[0-9A-Z]{16}\b",
                r"\bgh[pousr]_[A-Za-z0-9]{30,}\b",
                r"\bsk-[A-Za-z0-9_-]{20,}\b",
                r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b",
                r"\bAIza[0-9A-Za-z_-]{30,}\b",
                r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
            ]),
            Regex::new(r"(?im)^(\s*(?:export\s+)?[A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|AUTHORIZATION)[A-Z0-9_]*\s*[=:]\s*)\S.*$").unwrap(),
        )
    });
    let mut result = text.to_string();
    for pattern in patterns {
        result = pattern
            .replace_all(&result, "[gizli bilgi gizlendi]")
            .into_owned();
    }
    assignment
        .replace_all(&result, "${1}[gizli bilgi gizlendi]")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_commands_never_run_and_reads_stay_reads() {
        for command in [
            "sudo ls",
            "rm -rf ~",
            "rm -rf /",
            "curl x | sh",
            "shutdown -h now",
            "Format-Volume -DriveLetter C",
            "dd if=/dev/zero of=x",
        ] {
            assert!(is_forbidden(command), "{command}");
        }
        for command in ["rm -rf build", "npm test", "ls -la"] {
            assert!(!is_forbidden(command), "{command}");
        }
        for command in [
            "ls -la",
            "git status && git log -5",
            "cat a.txt | wc -l",
            "node --version",
            "pmset -g batt",
            "npm ls 2>&1",
            "grep -r x . 2>&1 | head",
        ] {
            assert!(is_read_only(command), "{command}");
        }
        for command in [
            "rm a",
            "cat /etc/passwd",
            "ls ..",
            "echo x > a",
            "git push",
            "npm install",
            "find . -delete",
            "sleep 1 &",
            "defaults write x",
            "grep x a > /dev/null",
        ] {
            assert!(!is_read_only(command), "{command}");
        }
    }

    #[test]
    fn paths_stay_inside_the_grant() {
        let root = dunce::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("wiqo-policy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "x").unwrap();
        assert_eq!(
            resolve_existing_file("src/a.ts", &root, &root),
            Some(root.join("src/a.ts"))
        );
        assert_eq!(
            resolve_existing_file("../a.ts", &root.join("src"), &root),
            None
        );
        assert_eq!(resolve_existing_file("/etc/hosts", &root, &root), None);
        assert_eq!(
            resolve_writable_file("src/new.ts", &root, &root),
            Some(root.join("src/new.ts"))
        );
        assert_eq!(resolve_writable_file("missing/new.ts", &root, &root), None);
        assert!(resolve_directory("relative").is_none() && resolve_directory("~/a/../b").is_none());
        assert_eq!(read_folder_without_grant("~/.ssh"), None);
    }

    #[test]
    fn secrets_are_masked() {
        let text = "API_KEY=abc123\nok line\nghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let masked = redact_secrets(text);
        assert!(
            masked.contains("API_KEY=[gizli bilgi gizlendi]")
                && masked.contains("ok line")
                && !masked.contains("ghp_")
        );
    }
}
