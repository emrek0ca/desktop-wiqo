//! Running one terminal command: a time limit, a stop that reaches the whole process group, live output for background jobs, an optional PTY for
//! interactive programs, and the folder and exported variables the shell ended with, so the next command of the session continues from there.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};

use super::policy::redact_secrets;

pub const OUTPUT_LIMIT: usize = 50 * 1024;
const CWD_MARKER: &str = "__WIQO_PWD__";
const ENV_MARKER: &str = "__WIQO_ENV__";
/// Shell bookkeeping that must not carry over to the next command.
const UNSHARED: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_", "__wiqo_rc", "TMPDIR"];

/// Where a command may write. Only macOS enforces it (sandbox-exec); elsewhere the grant's folder rule and the command policy are the boundary.
#[derive(Clone, Debug)]
pub enum WriteScope {
    ReadOnly,
    Folder(PathBuf),
}

/// What a running command can be asked from outside: its process id, a stop, input for a PTY, live output.
#[derive(Clone, Default)]
pub struct Control {
    pub pid: Arc<AtomicI64>,
    pub cancel: Arc<AtomicBool>,
    pub input: Option<Arc<Input>>,
    pub progress: Option<Arc<dyn Fn(Value) + Send + Sync>>,
}

/// The writing end of an interactive job's PTY. Writes are bounded and never retried: a partial write is uncertain.
#[derive(Default)]
pub struct Input {
    #[cfg(unix)]
    fd: Mutex<Option<std::os::fd::OwnedFd>>,
}

impl Input {
    pub fn send(&self, text: &str) -> bool {
        let bytes = text.as_bytes();
        if bytes.is_empty() || bytes.len() > 4096 {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let guard = self.fd.lock().expect("input lock");
            let Some(fd) = guard.as_ref() else {
                return false;
            };
            let written =
                unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
            written == bytes.len() as isize
        }
        #[cfg(not(unix))]
        false
    }

    #[cfg(unix)]
    fn attach(&self, fd: std::os::fd::OwnedFd) {
        *self.fd.lock().expect("input lock") = Some(fd);
    }

    fn close(&self) {
        #[cfg(unix)]
        {
            self.fd.lock().expect("input lock").take();
        }
    }
}

pub struct Snapshot {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
    pub timed_out: bool,
    pub truncated: bool,
    pub duration_ms: u64,
}

impl Snapshot {
    pub fn output(&self, language: &str) -> Value {
        json!({
            "language": language, "exitCode": if self.timed_out { -1 } else { self.exit_code }, "stdout": self.stdout, "stderr": self.stderr,
            "truncated": self.truncated, "durationMs": self.duration_ms,
        })
    }
}

/// Keeps the last `OUTPUT_LIMIT` bytes.
#[derive(Default)]
struct Buffer {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Buffer {
    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > OUTPUT_LIMIT {
            let excess = self.bytes.len() - OUTPUT_LIMIT;
            self.bytes.drain(..excess);
            self.truncated = true;
        }
    }
}

fn drain(
    mut reader: impl Read + Send + 'static,
    buffer: Arc<Mutex<Buffer>>,
) -> std::sync::mpsc::Receiver<()> {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(count) = reader.read(&mut chunk) {
            if count == 0 {
                break;
            }
            buffer.lock().expect("buffer lock").push(&chunk[..count]);
        }
        let _ = done.send(());
    });
    finished
}

fn stop(pid: i64, child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        if pid > 0 {
            libc::kill(-(pid as i32), libc::SIGTERM);
            std::thread::sleep(Duration::from_millis(150));
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = crate::process::command("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output();
    }
    let _ = child.kill();
}

/// Sends SIGINT to an interactive job's process group (Ctrl-C).
pub fn interrupt(pid: i64) {
    #[cfg(unix)]
    unsafe {
        if pid > 0 && libc::kill(-(pid as i32), libc::SIGINT) != 0 {
            libc::kill(pid as i32, libc::SIGINT);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// Stops a job's whole process group now.
pub fn terminate(pid: i64) {
    #[cfg(unix)]
    unsafe {
        if pid > 0 {
            libc::kill(-(pid as i32), libc::SIGTERM);
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        let _ = crate::process::command("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output();
    }
}

/// Runs a program to the end or the time limit, whichever comes first.
pub fn run(mut command: Command, seconds: u64, control: &Control) -> Snapshot {
    let started = Instant::now();
    let failed = |message: &str| Snapshot {
        stdout: String::new(),
        stderr: message.into(),
        exit_code: -1,
        timed_out: false,
        truncated: false,
        duration_ms: started.elapsed().as_millis() as u64,
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let stdout = Arc::new(Mutex::new(Buffer::default()));
    let stderr = Arc::new(Mutex::new(Buffer::default()));
    let mut readers = Vec::new();
    let mut child;
    #[cfg(unix)]
    let pty = control.input.as_ref().map(|_| open_pty());
    #[cfg(unix)]
    if let Some(pty) = pty {
        let Some((master, slave)) = pty else {
            return failed("Etkileşimli terminal açılamadı.");
        };
        let (Ok(a), Ok(b)) = (slave.try_clone(), slave.try_clone()) else {
            return failed("Etkileşimli terminal açılamadı.");
        };
        command
            .stdin(Stdio::from(slave))
            .stdout(Stdio::from(a))
            .stderr(Stdio::from(b));
        child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return failed("İşlem başlatılamadı."),
        };
        // The parent's copies of the slave are gone with `command`; reading the master ends when the program does.
        drop(command);
        let Ok(writer) = master.try_clone() else {
            return failed("Etkileşimli terminal açılamadı.");
        };
        control.input.as_ref().expect("input").attach(writer);
        readers.push(drain(std::fs::File::from(master), stdout.clone()));
    } else {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return failed("İşlem başlatılamadı."),
        };
        readers.push(drain(child.stdout.take().expect("stdout"), stdout.clone()));
        readers.push(drain(child.stderr.take().expect("stderr"), stderr.clone()));
    }
    #[cfg(not(unix))]
    {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return failed("İşlem başlatılamadı."),
        };
        readers.push(drain(child.stdout.take().expect("stdout"), stdout.clone()));
        readers.push(drain(child.stderr.take().expect("stderr"), stderr.clone()));
    }
    let pid = child.id() as i64;
    control.pid.store(pid, Ordering::SeqCst);
    let deadline = started + Duration::from_secs(seconds.max(1));
    let mut timed_out = false;
    let mut last_update = Instant::now();
    let mut published = 0usize;
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break Some(status);
        }
        let cancelled = control.cancel.load(Ordering::SeqCst);
        if Instant::now() >= deadline || cancelled {
            timed_out = !cancelled;
            stop(pid, &mut child);
            break child.wait().ok();
        }
        if let Some(progress) = &control.progress {
            let size = stdout.lock().expect("buffer").bytes.len()
                + stderr.lock().expect("buffer").bytes.len();
            if size != published && last_update.elapsed() >= Duration::from_millis(500) {
                published = size;
                last_update = Instant::now();
                let (out, err) = (
                    stdout.lock().expect("buffer"),
                    stderr.lock().expect("buffer"),
                );
                progress(json!({
                    "language": "shell", "exitCode": 0, "stdout": redact_secrets(&String::from_utf8_lossy(&out.bytes)),
                    "stderr": redact_secrets(&String::from_utf8_lossy(&err.bytes)), "truncated": out.truncated || err.truncated,
                    "durationMs": started.elapsed().as_millis() as u64,
                }));
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // A background child of the command may keep the pipes open: wait a moment for the output, never for the child.
    for reader in readers {
        let _ = reader.recv_timeout(Duration::from_secs(1));
    }
    if let Some(input) = &control.input {
        input.close();
    }
    control.pid.store(0, Ordering::SeqCst);
    let (out, err) = (
        stdout.lock().expect("buffer"),
        stderr.lock().expect("buffer"),
    );
    Snapshot {
        stdout: String::from_utf8_lossy(&out.bytes).to_string(),
        stderr: String::from_utf8_lossy(&err.bytes).to_string(),
        exit_code: status
            .and_then(|status| status.code())
            .map(i64::from)
            .unwrap_or(-1),
        timed_out,
        truncated: out.truncated || err.truncated,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}

#[cfg(unix)]
fn open_pty() -> Option<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::FromRawFd;
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: 40,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            // *mut on macOS, *const on Linux; a raw pointer fits both.
            std::ptr::addr_of_mut!(size),
        )
    };
    if opened != 0 {
        return None;
    }
    unsafe {
        Some((
            std::os::fd::OwnedFd::from_raw_fd(master),
            std::os::fd::OwnedFd::from_raw_fd(slave),
        ))
    }
}

/// The person's interactive-shell PATH (nvm, pyenv and conda add their tools in the rc file a login shell never reads), resolved once.
pub fn interactive_path() -> Option<String> {
    static PATH: OnceLock<Option<String>> = OnceLock::new();
    PATH.get_or_init(|| {
        if cfg!(windows) {
            return None;
        }
        let shell = default_shell();
        let mut command = crate::process::command(&shell);
        command
            .args(["-ilc", "printf '__WIQO_PATH__%s' \"$PATH\""])
            .env_clear()
            .env("HOME", super::policy::home())
            .env("TERM", "dumb");
        if let Ok(user) = std::env::var("USER") {
            command.env("USER", user);
        }
        let snapshot = run(command, 5, &Control::default());
        let text = snapshot.stdout;
        let value = text.rsplit_once("__WIQO_PATH__")?.1.trim().to_string();
        (!snapshot.timed_out && !value.is_empty()).then_some(value)
    })
    .clone()
}

pub fn default_shell() -> String {
    if cfg!(target_os = "macos") {
        "/bin/zsh".into()
    } else if Path::new("/bin/bash").exists() {
        "/bin/bash".into()
    } else {
        "/bin/sh".into()
    }
}

fn environment(working: &Path) -> HashMap<String, String> {
    if cfg!(windows) {
        let mut all: HashMap<String, String> = std::env::vars().collect();
        all.insert("PWD".into(), working.to_string_lossy().to_string());
        return all;
    }
    let mut values: HashMap<String, String> = ["PATH", "HOME", "USER", "LANG", "LC_ALL"]
        .iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| (key.to_string(), value))
        })
        .collect();
    let path = interactive_path()
        .or_else(|| values.get("PATH").cloned())
        .unwrap_or_else(|| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into());
    values.insert("PATH".into(), path);
    values
        .entry("LANG".into())
        .or_insert_with(|| "en_US.UTF-8".into());
    values.insert("PWD".into(), working.to_string_lossy().to_string());
    values
}

fn quote_profile(path: &str) -> String {
    format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A sandbox that allows everything a shell needs (reads, network, processes) and confines file writes to the scope.
pub fn sandbox_profile(scope: &WriteScope) -> String {
    let home = super::policy::home().to_string_lossy().to_string();
    let mut writable: Vec<String> = ["/private/tmp", "/private/var/folders", "/dev"]
        .iter()
        .map(|path| format!("(subpath {})", quote_profile(path)))
        .collect();
    if let WriteScope::Folder(root) = scope {
        writable.push(format!(
            "(subpath {})",
            quote_profile(&root.to_string_lossy())
        ));
        for cache in [
            "/Library/Caches",
            "/Library/Logs",
            "/.npm",
            "/.cache",
            "/.cargo",
            "/.rustup",
            "/.local",
            "/.config",
            "/go",
            "/.bun",
            "/.pnpm-store",
        ] {
            writable.push(format!(
                "(subpath {})",
                quote_profile(&format!("{home}{cache}"))
            ));
        }
        for path in ["/opt/homebrew", "/usr/local"] {
            writable.push(format!("(subpath {})", quote_profile(path)));
        }
    }
    format!(
        "(version 1) (allow default) (deny file-write*) (allow file-write* {})",
        writable.join(" ")
    )
}

/// The answer to one command, as `executeTerminalCommandTrackingDirectory` gives it.
pub struct Outcome {
    pub output: Value,
    pub error: Option<wiqo_core::wire::WireError>,
    pub end_directory: Option<PathBuf>,
    pub exports: Option<String>,
}

/// `export -p` output without the shell's own bookkeeping.
fn shared_exports(script: &str) -> String {
    script
        .lines()
        .filter(|line| {
            let declaration = line
                .split(' ')
                .find(|word| {
                    *word != "export"
                        && *word != "typeset"
                        && *word != "declare"
                        && !word.starts_with('-')
                })
                .unwrap_or("");
            let name = declaration.split('=').next().unwrap_or("");
            !name.is_empty() && !UNSHARED.contains(&name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Runs one command of a session. `shell` is "zsh"/"bash"/"default" on macOS and Linux, "powershell"/"cmd" on Windows.
pub fn run_tracked(
    command: &str,
    working: &Path,
    seconds: u64,
    scope: Option<&WriteScope>,
    exports: Option<&str>,
    shell: Option<&str>,
    control: &Control,
) -> Outcome {
    let interactive = control.input.is_some();
    if cfg!(windows) {
        return run_windows(command, working, seconds, shell, control);
    }
    let restore = exports
        .filter(|text| !text.is_empty())
        .map(|text| {
            format!(
                "eval \"$(printf %s '{}' | base64 --decode)\" 2>/dev/null\n",
                base64::engine::general_purpose::STANDARD.encode(text)
            )
        })
        .unwrap_or_default();
    let script = if interactive {
        format!("{restore}{command}")
    } else {
        format!(
            "{restore}{command}\n__wiqo_rc=$?; set +xv; printf '\\n{CWD_MARKER}%s\\n' \"$PWD\"; printf '{ENV_MARKER}%s\\n' \"$(export -p | base64 | tr -d '\\n')\"; exit $__wiqo_rc"
        )
    };
    let executable = match shell {
        Some("bash") if Path::new("/bin/bash").exists() => "/bin/bash".to_string(),
        _ => default_shell(),
    };
    let mut process = match (cfg!(target_os = "macos"), scope) {
        (true, Some(scope)) => {
            let mut sandboxed = crate::process::command("/usr/bin/sandbox-exec");
            sandboxed.args(["-p", &sandbox_profile(scope), &executable]);
            sandboxed
        }
        _ => crate::process::command(&executable),
    };
    process
        .args(["-lc", &script, "wiqo-terminal"])
        .current_dir(working)
        .env_clear()
        .envs(environment(working));
    // Live output never shows the bookkeeping lines, not even half of one.
    let visible = control.progress.clone().map(|callback| {
        let wrapped: Arc<dyn Fn(Value) + Send + Sync> = Arc::new(move |mut output: Value| {
            let stdout = output["stdout"].as_str().unwrap_or("").to_string();
            let before = stdout.split(CWD_MARKER).next().unwrap_or("");
            let complete = before.rfind('\n').map(|end| &before[..=end]).unwrap_or("");
            output["stdout"] = json!(complete);
            callback(output)
        });
        wrapped
    });
    let control = Control {
        progress: visible,
        ..control.clone()
    };
    let snapshot = run(process, seconds, &control);
    let mut stdout = snapshot.stdout.clone();
    let mut exported = None;
    let mut end_directory = None;
    if !interactive {
        if let Some(index) = stdout.rfind(ENV_MARKER) {
            let encoded = stdout[index + ENV_MARKER.len()..]
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            exported = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .map(|script| shared_exports(&script));
            stdout.truncate(index);
        }
        if let Some(index) = stdout
            .rfind(&format!("\n{CWD_MARKER}"))
            .or_else(|| stdout.rfind(CWD_MARKER))
        {
            let start = stdout[index..]
                .find(CWD_MARKER)
                .map(|offset| index + offset + CWD_MARKER.len())
                .unwrap_or(stdout.len());
            end_directory = stdout[start..]
                .lines()
                .next()
                .map(|line| PathBuf::from(line.trim()));
            stdout.truncate(index);
        }
    }
    finish(Snapshot { stdout, ..snapshot }, end_directory, exported)
}

fn run_windows(
    command: &str,
    working: &Path,
    seconds: u64,
    shell: Option<&str>,
    control: &Control,
) -> Outcome {
    let mut process = if shell == Some("cmd") {
        let mut cmd = crate::process::command("cmd");
        cmd.args([
            "/d",
            "/s",
            "/c",
            &format!("{command} & echo {CWD_MARKER}%CD%"),
        ]);
        cmd
    } else {
        let mut powershell = crate::process::command("powershell");
        let script = format!(
            "[Console]::OutputEncoding=[Text.Encoding]::UTF8; {command}\n$__wiqo_rc = if ($LASTEXITCODE) {{ $LASTEXITCODE }} elseif ($?) {{ 0 }} else {{ 1 }}; Write-Output \"`n{CWD_MARKER}$((Get-Location).Path)\"; exit $__wiqo_rc"
        );
        powershell.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ]);
        powershell
    };
    process.current_dir(working).envs(environment(working));
    let snapshot = run(process, seconds, control);
    let mut stdout = snapshot.stdout.clone();
    let mut end_directory = None;
    if let Some(index) = stdout.rfind(CWD_MARKER) {
        end_directory = stdout[index + CWD_MARKER.len()..]
            .lines()
            .next()
            .map(|line| PathBuf::from(line.trim()));
        stdout.truncate(index);
    }
    finish(Snapshot { stdout, ..snapshot }, end_directory, None)
}

fn finish(snapshot: Snapshot, end_directory: Option<PathBuf>, exports: Option<String>) -> Outcome {
    let snapshot = Snapshot {
        stdout: redact_secrets(&snapshot.stdout),
        stderr: redact_secrets(&snapshot.stderr),
        ..snapshot
    };
    let output = snapshot.output("shell");
    let error = if snapshot.timed_out {
        Some(wiqo_core::wire::WireError::new(
            "CODE_TIMEOUT",
            "Terminal komutu zaman aşımına uğradı; sonucu doğrulanamadı.",
            "timeout",
        ))
    } else if snapshot.exit_code != 0 {
        let detail: String = snapshot.stderr.trim().chars().take(300).collect();
        let message = if detail.is_empty() {
            format!(
                "Terminal komutu çıkış kodu {} ile sonlandı.",
                snapshot.exit_code
            )
        } else {
            format!("Terminal komutu hata verdi: {detail}")
        };
        Some(wiqo_core::wire::WireError::new(
            "CODE_FAILED",
            message,
            "tool_failure",
        ))
    } else {
        None
    };
    Outcome {
        output,
        error,
        end_directory,
        exports,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_session_carries_its_folder_and_exports_and_a_timeout_stops_the_group() {
        let dir = dunce::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("wiqo-shell-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let first = run_tracked(
            "cd sub && export WIQO_X=42 && echo hi",
            &dir,
            20,
            None,
            None,
            None,
            &Control::default(),
        );
        assert!(first.error.is_none(), "{:?}", first.output);
        assert_eq!(first.output["stdout"].as_str().unwrap().trim(), "hi");
        assert_eq!(
            first.end_directory.as_deref(),
            Some(dir.join("sub").as_path())
        );
        let second = run_tracked(
            "echo $WIQO_X",
            first.end_directory.as_deref().unwrap(),
            20,
            None,
            first.exports.as_deref(),
            None,
            &Control::default(),
        );
        assert_eq!(second.output["stdout"].as_str().unwrap().trim(), "42");
        let failed = run_tracked(
            "echo oops >&2; exit 3",
            &dir,
            20,
            None,
            None,
            None,
            &Control::default(),
        );
        assert_eq!(failed.output["exitCode"], 3);
        assert_eq!(failed.error.unwrap().code, "CODE_FAILED");
        let started = Instant::now();
        let slow = run_tracked(
            "sleep 30 & sleep 30",
            &dir,
            1,
            None,
            None,
            None,
            &Control::default(),
        );
        assert_eq!(slow.error.unwrap().code, "CODE_TIMEOUT");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn an_interactive_job_reads_its_input() {
        let dir = dunce::canonicalize(std::env::temp_dir()).unwrap();
        let input = Arc::new(Input::default());
        let control = Control {
            input: Some(input.clone()),
            ..Control::default()
        };
        let sender = input.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            sender.send("merhaba\n");
        });
        let outcome = run_tracked(
            "read line; echo \"got:$line\"",
            &dir,
            10,
            None,
            None,
            None,
            &control,
        );
        assert!(
            outcome.output["stdout"]
                .as_str()
                .unwrap()
                .contains("got:merhaba"),
            "{:?}",
            outcome.output
        );
    }
}
