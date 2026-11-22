//! What a terminal session may do (`RovaTerminalPolicy` on the phone and the old Mac app): commands that never run, what counts as read-only, where
//! a path may point, and the credentials masked before output leaves the computer.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// The person turned on full access for this computer: the terminal runs what they would run themselves, and only what nothing can undo is refused.
static FULL_ACCESS: AtomicBool = AtomicBool::new(false);

pub fn set_full_access(on: bool) {
    FULL_ACCESS.store(on, Ordering::Relaxed);
}

pub fn full_access() -> bool {
    FULL_ACCESS.load(Ordering::Relaxed)
}

/// What stays refused under full access: a disk or its boot, a whole drive or the home folder, the keychain, a fork bomb, shutting the computer down
/// (the work would end with it). Installers piped to a shell, sudo, launchctl, elevation and execution policy are allowed there.
fn catastrophic() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        compiled(&[
            r"\brm\s+(-[a-zA-Z]*[rf][a-zA-Z]*\s+)+(/|~|\$HOME|\*)(\s|$|/\s|/\*)",
            r"\b(mkfs|fdisk|diskutil\s+(erase|partition|zero)|dd\s+if=)",
            r"\b(shutdown|reboot|halt|csrutil|nvram|kextload)\b",
            r":\(\)\s*\{\s*:\|:&\s*\};:",
            r"\bsecurity\s+(dump-keychain|find-generic-password|find-internet-password)",
            r"(?i)\b(format-volume|clear-disk|initialize-disk|diskpart|stop-computer|restart-computer)\b",
            r"(?i)\bremove-item\b[^;|]*-recurse[^;|]*\s([a-z]:\\?|\\|~|\$env:userprofile)(\s|$)",
            r"(?i)\breg\s+delete\s+hk(lm|cr)",
        ])
    })
}

pub fn is_forbidden(command: &str) -> bool {
    let patterns = if full_access() {
        catastrophic()
    } else {
        forbidden()
    };
    patterns.iter().any(|pattern| pattern.is_match(command))
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
    // Windows: inspection cmdlets and the tools cmd.exe has; reading the state of the computer needs no approval, as `pmset -g batt` does on a Mac.
    "get-item",
    "get-itemproperty",
    "get-help",
    "get-computerinfo",
    "get-service",
    "get-command",
    "get-filehash",
    "get-member",
    "get-ciminstance",
    "get-wmiobject",
    "get-volume",
    "get-disk",
    "get-physicaldisk",
    "get-psdrive",
    "get-netipaddress",
    "get-netipconfiguration",
    "get-netadapter",
    "get-netconnectionprofile",
    "get-nettcpconnection",
    "get-clipboard",
    "get-timezone",
    "get-culture",
    "get-hotfix",
    "get-appxpackage",
    "get-executionpolicy",
    "get-random",
    "select-string",
    "select-object",
    "where-object",
    "sort-object",
    "measure-object",
    "format-table",
    "format-list",
    "out-string",
    "convertto-json",
    "write-output",
    "test-path",
    "test-connection",
    "resolve-dnsname",
    "tasklist",
    "systeminfo",
    "findstr",
    "where",
    "ver",
    "netstat",
    // macOS and Linux extras.
    "printenv",
    "id",
    "who",
    "w",
    "last",
    "nproc",
    "locale",
    "getconf",
    "lspci",
    "lsusb",
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
        "winget" => &["list", "search", "show", "source"],
        "diskutil" => &["list", "info"],
        "launchctl" => &["list", "print"],
        "softwareupdate" => &["-l", "--list"],
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

/// PowerShell inspection pipelines such as `Get-CimInstance Win32_Battery | Select EstimatedChargeRemaining` or
/// `(Get-CimInstance Win32_OperatingSystem).LastBootUpTime`. They use `$_`, braces and parentheses, which the plain rule keeps out of a read, so they are
/// judged here: every stage is a known inspection cmdlet, and nothing in the text may change, start, stop, write or call a method.
fn is_powershell_read(command: &str) -> bool {
    static RULES: OnceLock<(Regex, Regex, Regex, Regex)> = OnceLock::new();
    let (mutating, method, assign, outside) = RULES.get_or_init(|| {
        (
            Regex::new(r"(?i)\b(?:invoke|start|stop|restart|remove|set|new|add|clear|copy|move|rename|export|import|register|unregister|enable|disable|install|uninstall|update|tee|expand|compress|save|send|push|mount|dismount|initialize|repair|reset|resume|suspend|grant|revoke|block|unblock|restore|enter|exit|connect|disconnect|use|out-file|out-printer|out-gridview|write-error|write-eventlog)-|\b(?:iex|icm|saps|sal|sv|si|ni|ri|rm|rd|del|erase|cp|mv|mi|rni|ac|sc|kill|spps|spsv|sasv|sleep|start|call)\b").unwrap(),
            Regex::new(r"\.\s*[A-Za-z_]\w*\s*\(|::|\[\s*system\.|\bnew-object\b").unwrap(),
            Regex::new(r"\$[\w:]+\s*[+\-*/]?=").unwrap(),
            Regex::new(r"(^|[\s\x22\x27=])/|[A-Za-z]:[\\/]|\\\\|\.\.|~|\$[A-Za-z]").unwrap(),
        )
    });
    const STAGES: &[&str] = &[
        "select",
        "select-object",
        "where",
        "where-object",
        "sort-object",
        "measure",
        "measure-object",
        "ft",
        "format-table",
        "fl",
        "format-list",
        "out-string",
        "convertto-json",
        "write-output",
        "first",
        "gps",
        "gcim",
        "gwmi",
        "gsv",
        "gc",
        "gci",
        "gi",
        "gp",
        "gcm",
        "gtz",
    ];
    const FILE_READERS: &[&str] = &[
        "get-content",
        "gc",
        "type",
        "cat",
        "get-childitem",
        "gci",
        "ls",
        "dir",
        "get-item",
        "gi",
        "get-itemproperty",
        "gp",
        "get-filehash",
        "select-string",
        "test-path",
    ];
    let lead = command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();
    if !(lead.starts_with("get-")
        || lead.starts_with('(')
        || STAGES.contains(&lead.as_str())
        || matches!(lead.as_str(), "test-connection" | "resolve-dnsname"))
    {
        return false;
    }
    if command.contains(['`', '<', '>', '&', '%', ';'])
        || mutating.is_match(command)
        || method.is_match(command)
        || assign.is_match(command)
    {
        return false;
    }
    // A group or script block may only start with a value, an operator, a variable or another inspection cmdlet: `(taskkill ...)` or `{ cmd /c ... }` would
    // run a program from inside an argument, which is what the plain rule keeps out of a read.
    let opens_known = |word: &str| {
        let word = word.to_lowercase();
        word.is_empty()
            || word.starts_with(['$', '-', '\'', '"', '@', '!', '[', ')', '}'])
            || word.chars().next().is_some_and(|c| c.is_ascii_digit())
            || word.starts_with("get-")
            || STAGES.contains(&word.as_str())
            || READ_ONLY_PROGRAMS.contains(&word.as_str())
    };
    for (at, _) in command.match_indices(['(', '{']) {
        let word = command[at + 1..]
            .trim_start()
            .split(|c: char| c.is_whitespace() || "(){}|,".contains(c))
            .next()
            .unwrap_or("");
        if !opens_known(word) {
            return false;
        }
    }
    // Quotes inside a pipe stage are fine, but a pipe inside braces or parentheses is not split here: its stages would not start with a cmdlet and the read is refused.
    let stages: Vec<String> = command
        .split('|')
        .map(|stage| stage.trim().trim_start_matches(['(', ' ']).to_string())
        .filter(|stage| !stage.is_empty())
        .collect();
    if stages.is_empty() {
        return false;
    }
    stages.iter().all(|stage| {
        let first = stage.split_whitespace().next().unwrap_or("").to_lowercase();
        let first = first.trim_start_matches('(');
        let known = first.starts_with("get-")
            || STAGES.contains(&first)
            || READ_ONLY_PROGRAMS.contains(&first);
        // A cmdlet that opens files keeps the strict rule: no path outside the session folder, no variable that could name one.
        known && !(FILE_READERS.contains(&first) && outside.is_match(stage))
    })
}

/// Conservative: anything that is not clearly an inspection needs a "full" grant.
pub fn is_read_only(command: &str) -> bool {
    // `/all` looks like an absolute path to the rule below, so the two harmless spellings of the Windows command are named here.
    if matches!(
        command.trim().to_lowercase().as_str(),
        "ipconfig" | "ipconfig /all"
    ) || is_powershell_read(command)
    {
        return true;
    }
    static SHAPES: OnceLock<(Regex, Regex, Regex)> = OnceLock::new();
    let (absolute, redirect_ok, flags) = SHAPES.get_or_init(|| {
        (
            Regex::new(r#"(^|[\s"'=])/|[A-Za-z]:[\\/]|\\\\"#).unwrap(),
            Regex::new(r">\s*/dev/null|>&1").unwrap(),
            Regex::new(r"^-(delete|exec|execdir|ok|okdir|fprint)").unwrap(),
        )
    });
    let text = command.replace("2>&1", "");
    // Read grants accept relative paths only: inspection commands cannot name an outside path.
    if text.contains("..") || text.contains('~') || text.contains('$') || absolute.is_match(&text) {
        return false;
    }
    // zsh glob qualifiers `*(e:'cmd':)`, `=(cmd)` and PowerShell `(Cmd)` run code from inside an argument; a read never needs a group or a brace.
    if text.contains(['(', ')', '{', '}', '%']) {
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
        let program = first.to_lowercase();
        let rest = &words[1..];
        match program.as_str() {
            "find" => !rest.iter().any(|word| flags.is_match(word)),
            // Spotlight content search: read-only only when kept to the session folder.
            "mdfind" => rest.contains(&"-onlyin"),
            "sort" => rest.iter().all(|word| match *word {
                "-" | "--" | "--reverse" | "--numeric-sort" | "--unique" | "--ignore-case"
                | "--stable" | "--check" | "-k" | "-t" => true,
                _ => {
                    if let Some(flags) = word.strip_prefix('-') {
                        flags
                            .bytes()
                            .all(|flag| b"bCcdfghiMnRrsuVz".contains(&flag))
                    } else {
                        // Quoting, escapes and globs can change the arguments the shell passes.
                        word.bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"._/,+".contains(&byte))
                    }
                }
            }),
            "git" => {
                let Some(sub) = rest.first() else {
                    return false;
                };
                if !GIT_READ.contains(sub)
                    || rest.iter().any(|word| {
                        matches!(
                            word.split('=').next(),
                            Some("--output" | "--ext-diff" | "--textconv")
                        )
                    })
                {
                    return false;
                }
                match *sub {
                    "config" => rest
                        .iter()
                        .any(|word| matches!(*word, "--get" | "--list" | "-l")),
                    "branch" => rest[1..].iter().all(|word| {
                        matches!(
                            *word,
                            "-a" | "-r"
                                | "-v"
                                | "-vv"
                                | "--all"
                                | "--remotes"
                                | "--list"
                                | "--show-current"
                        )
                    }),
                    "tag" => rest[1..]
                        .iter()
                        .all(|word| matches!(*word, "-l" | "--list")),
                    "remote" => rest[1..]
                        .iter()
                        .all(|word| matches!(*word, "-v" | "--verbose")),
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
    #[test]
    fn shared_terminal_policy_vectors() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Vector {
            target: String,
            command: String,
            read_only: bool,
        }
        let vectors: Vec<Vector> = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/terminal-policy.json"
        ))
        .unwrap();
        assert!(!vectors.is_empty());
        for vector in vectors {
            assert_eq!(
                is_read_only(&vector.command),
                vector.read_only,
                "{}: {}",
                vector.target,
                vector.command
            );
        }
    }

    #[test]
    fn a_read_never_runs_code_from_inside_an_argument() {
        for command in [
            "ls *(e:'touch pwned':)",
            "cat =(touch pwned)",
            "echo (New-Item pwned)",
            "echo {a,b}",
        ] {
            assert!(!is_read_only(command), "{command}");
        }
        assert!(is_read_only("ls -la"));
    }

    #[test]
    fn powershell_inspection_is_a_read_and_anything_that_changes_is_not() {
        for command in [
            "Get-CimInstance Win32_Battery | Select EstimatedChargeRemaining,BatteryStatus",
            "(Get-CimInstance Win32_OperatingSystem).LastBootUpTime",
            "Get-Process | Where-Object { $_.MainWindowTitle } | Select Name,MainWindowTitle",
            "Get-Process | Sort-Object CPU -Descending | Select-Object -First 5",
            "Get-PSDrive -PSProvider FileSystem | Select Name,Used,Free",
            "Get-NetIPConfiguration",
            "Get-Service | Where-Object Status -eq Running",
            "Get-Content notes.txt | Select-String error",
            "winget list",
            "ipconfig /all",
            "tasklist",
            "systeminfo",
        ] {
            assert!(is_read_only(command), "{command}");
        }
        for command in [
            "Get-Process | ForEach-Object { taskkill /f /im x }",
            "Get-Process | Where-Object { taskkill /f /im x }",
            "Get-Content (cmd /c del x)",
            "Get-Process | Stop-Process",
            "(Get-Item a.txt).Delete()",
            "Get-ChildItem | Remove-Item",
            "Get-Date; Remove-Item a",
            "Get-Content $env:USERPROFILE\\secret.txt",
            "Get-Content C:\\Users\\x\\a.txt",
            "Get-Process > out.txt",
            "Get-Process | Out-File out.txt",
            "[IO.File]::Delete('a')",
            "$x = Get-Date",
            "winget install x",
            "ipconfig /release",
        ] {
            assert!(!is_read_only(command), "{command}");
        }
    }

    #[test]
    fn sort_only_accepts_literal_read_only_arguments() {
        assert!(is_read_only("sort -nr --unique input.txt"));
        assert!(is_read_only("cat input.txt | sort -u"));
        for command in [
            "sort -o output.txt input.txt",
            "sort -ro output.txt input.txt",
            "sort '--output' output.txt input.txt",
            "sort --out=output.txt input.txt",
            "sort *",
        ] {
            assert!(!is_read_only(command), "{command}");
        }
    }

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
