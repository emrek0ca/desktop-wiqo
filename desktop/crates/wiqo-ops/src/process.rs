//! Running another program with a time limit. A program that does not finish is stopped, never waited for.

use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A command that never flashes a console window on Windows (Wiqo is a windowed app; every console program would open one).
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Runs `command` and returns its output, or `None` if it could not start or did not finish within `limit` (then it is killed).
pub(crate) fn run_with_timeout(mut command: Command, limit: Duration) -> Option<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    // Output is drained on threads, so a program that writes more than a pipe holds cannot block itself.
    let out = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });
    let err = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait().ok()? {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    Some(Output {
        status,
        stdout: out.join().ok()?,
        stderr: err.join().ok()?,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_finished_program_returns_and_a_stuck_one_is_stopped() {
        let mut echo = Command::new("/bin/echo");
        echo.arg("merhaba");
        assert_eq!(
            String::from_utf8_lossy(
                &run_with_timeout(echo, Duration::from_secs(5))
                    .unwrap()
                    .stdout
            )
            .trim(),
            "merhaba"
        );
        let mut sleeper = Command::new("/bin/sleep");
        sleeper.arg("30");
        let started = Instant::now();
        assert!(run_with_timeout(sleeper, Duration::from_millis(200)).is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
