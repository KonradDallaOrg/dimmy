//! Run a local CLI (claude / codex / gemini) so it can never wedge the
//! caller: stdin is written and stdout + stderr are drained on threads of
//! their own (a child that fills a pipe nobody reads blocks forever), the
//! deadline holds, and a timeout kills the whole process tree. The tree
//! matters on Windows: an npm `.cmd` shim makes the child `cmd.exe`, and
//! killing it alone leaves `node` running with our pipes open.

use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// The child's process tree: a Job Object on Windows, the child's own
/// process group on Unix. Dropping it kills whatever is left of the tree.
pub struct Tree {
    #[cfg(target_os = "windows")]
    job: Option<windows::Win32::Foundation::HANDLE>,
    #[cfg(unix)]
    pgid: i32,
}

// SAFETY: a job HANDLE is a kernel object handle, valid from any thread.
#[cfg(target_os = "windows")]
unsafe impl Send for Tree {}

impl Tree {
    #[cfg(target_os = "windows")]
    fn adopt(child: &Child) -> Tree {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::{CloseHandle, HANDLE};
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        // SAFETY: plain Win32 calls on handles we own; `info` outlives the call.
        let job = unsafe {
            let Ok(job) = CreateJobObjectW(None, windows::core::PCWSTR::null()) else {
                crate::log("[cli_runner] CreateJobObject failed; timeout kills only the child");
                return Tree { job: None };
            };
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            let assigned = set.and_then(|_| {
                AssignProcessToJobObject(job, HANDLE(child.as_raw_handle() as *mut _))
            });
            if assigned.is_err() {
                crate::log(
                    "[cli_runner] AssignProcessToJobObject failed; timeout kills only the child",
                );
                let _ = CloseHandle(job);
                return Tree { job: None };
            }
            job
        };
        Tree { job: Some(job) }
    }

    #[cfg(unix)]
    fn adopt(child: &Child) -> Tree {
        Tree {
            pgid: child.id() as i32,
        }
    }

    #[cfg(not(any(unix, target_os = "windows")))]
    fn adopt(_child: &Child) -> Tree {
        Tree {}
    }

    pub fn kill_rest(&self) {
        #[cfg(target_os = "windows")]
        if let Some(job) = self.job {
            // SAFETY: `job` is open until Drop.
            let _ = unsafe { windows::Win32::System::JobObjects::TerminateJobObject(job, 1) };
        }
        #[cfg(unix)]
        // SAFETY: signals the group the child leads (process_group(0) at spawn).
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
        }
    }

    /// Kill the child and everything it started, then reap the child.
    pub fn kill(&self, child: &mut Child) {
        self.kill_rest();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.kill_rest();
        #[cfg(target_os = "windows")]
        if let Some(job) = self.job.take() {
            // SAFETY: closing the handle we created; KILL_ON_JOB_CLOSE ends the rest.
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(job) };
        }
    }
}

/// Spawn `cmd` with all three pipes, in a tree that [`Tree::kill`] can end.
pub fn spawn(cmd: &mut Command) -> std::io::Result<(Child, Tree)> {
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn()?;
    let tree = Tree::adopt(&child);
    Ok((child, tree))
}

/// Write `data` to the child's stdin on a thread, then close it (EOF).
pub fn feed_stdin(child: &mut Child, data: Vec<u8>) -> Result<(), String> {
    let mut stdin = child.stdin.take().ok_or("no stdin handle")?;
    std::thread::spawn(move || {
        if let Err(e) = stdin.write_all(&data) {
            crate::log(&format!("[cli_runner] stdin write: {}", e));
        }
    });
    Ok(())
}

/// Read a pipe to its end on a thread; the bytes arrive on the receiver.
pub fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// The first 200 characters of a child's stderr, for logs and errors.
pub fn excerpt(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr).chars().take(200).collect()
}

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
pub enum RunError {
    Spawn(String),
    Timeout,
}

/// Run `cmd` to completion with `stdin` as its input, within `timeout`.
pub fn run(cmd: &mut Command, stdin: &[u8], timeout: Duration) -> Result<Output, RunError> {
    assert!(!timeout.is_zero(), "cli_runner::run: zero timeout");
    let deadline = Instant::now() + timeout;
    let (mut child, tree) = spawn(cmd).map_err(|e| RunError::Spawn(e.to_string()))?;
    if let Err(e) = feed_stdin(&mut child, stdin.to_vec()) {
        tree.kill(&mut child);
        return Err(RunError::Spawn(e));
    }
    let out_rx = drain(child.stdout.take());
    let err_rx = drain(child.stderr.take());

    let status = wait_until(&mut child, &tree, deadline)?;
    // A grandchild that outlives the child would hold the pipes open, and
    // the drains with them. Its output is not ours to wait for.
    tree.kill_rest();
    let grace = deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_secs(1));
    let stdout = out_rx.recv_timeout(grace).map_err(|_| RunError::Timeout)?;
    let stderr = err_rx.recv_timeout(grace).map_err(|_| RunError::Timeout)?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// Wait for the child to exit; at `deadline`, kill its tree instead.
pub fn wait_until(
    child: &mut Child,
    tree: &Tree,
    deadline: Instant,
) -> Result<ExitStatus, RunError> {
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Ok(s),
            Ok(None) if Instant::now() >= deadline => {
                tree.kill(child);
                return Err(RunError::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                tree.kill(child);
                return Err(RunError::Spawn(format!("wait: {}", e)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    fn shell(script: &str) -> Command {
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command", script]);
        c
    }

    #[cfg(unix)]
    fn shell(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn a_child_that_floods_stdout_and_stderr_completes() {
        #[cfg(target_os = "windows")]
        let script = "$n = [Console]::In.ReadToEnd().Length; \
                      [Console]::Error.Write('e' * 1200000); \
                      [Console]::Out.Write('o' * 1200000); \
                      [Console]::Out.Write(\"|$n\")";
        #[cfg(unix)]
        let script = "n=$(wc -c | tr -d ' '); head -c 1200000 /dev/zero | tr '\\0' e >&2; \
                      head -c 1200000 /dev/zero | tr '\\0' o; printf '|%s' \"$n\"";
        let input = vec![b'i'; 1_200_000];
        let out = run(&mut shell(script), &input, Duration::from_secs(60)).expect("run");
        assert!(out.status.success());
        assert!(out.stderr.len() >= 1_200_000, "stderr {}", out.stderr.len());
        let stdout = String::from_utf8(out.stdout).expect("utf8");
        assert!(stdout.len() > 1_200_000, "stdout {}", stdout.len());
        assert!(stdout.ends_with("|1200000"), "stdin not fully read");
    }

    #[test]
    fn a_child_that_sleeps_is_killed_at_the_deadline() {
        #[cfg(target_os = "windows")]
        let script = "Start-Sleep -Seconds 60";
        #[cfg(unix)]
        let script = "sleep 60";
        let started = Instant::now();
        let r = run(&mut shell(script), b"", Duration::from_secs(2));
        assert!(matches!(r, Err(RunError::Timeout)), "{:?}", r.err());
        assert!(started.elapsed() < Duration::from_secs(15));
    }

    #[test]
    fn a_grandchild_holding_the_pipes_does_not_hold_the_caller() {
        #[cfg(target_os = "windows")]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/c", "start /b ping -n 60 127.0.0.1 & echo hi"]);
            c
        };
        #[cfg(unix)]
        let mut cmd = shell("sleep 60 & echo hi");
        let started = Instant::now();
        let out = run(&mut cmd, b"", Duration::from_secs(30)).expect("run");
        assert!(String::from_utf8_lossy(&out.stdout).contains("hi"));
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "waited {:?} for a grandchild",
            started.elapsed()
        );
    }
}
