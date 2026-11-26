//! Cancellable external tool processes that run in their own process group.

use std::{
    io::Read,
    process::Child,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use rustix::process::{kill_process_group, test_kill_process_group, Pid, Signal};

const POLL_INTERVAL: Duration = Duration::from_millis(25);
const TERM_GRACE: Duration = Duration::from_millis(500);

/// Wait for `child` (spawned with `process_group(0)` and piped stderr) while it polls `cancel`.
///
/// On cancel, terminate the child process group and fail. On a non-zero exit, fail with the
/// trimmed stderr text. On success, return the captured stderr text.
pub(crate) fn wait_cancellable(
    mut child: Child,
    label: &str,
    cancel: &AtomicBool,
) -> Result<String> {
    let child_pid = child.id();
    let stderr_pipe = child.stderr.take();
    let stderr_handle = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = stderr_pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });

    let status = loop {
        if let Some(status) = child
            .try_wait()
            .wrap_err_with(|| format!("poll {label} subprocess"))?
        {
            break status;
        }
        if cancel.load(Ordering::Relaxed) {
            terminate_process_group(child_pid);
            let _ = child.wait();
            let _ = stderr_handle.join();
            bail!("{label} interrupted by signal; terminated child process group {child_pid}");
        }
        thread::sleep(POLL_INTERVAL);
    };

    let stderr_bytes = stderr_handle.join().unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
    ensure!(status.success(), "{label} failed: {}", stderr.trim());
    Ok(stderr)
}

/// Send `SIGTERM` to a process group and escalate to `SIGKILL` if it outlives a short grace time.
fn terminate_process_group(pgid_raw: u32) {
    let Ok(pgid_i32) = i32::try_from(pgid_raw) else {
        return;
    };
    let Some(pgid) = Pid::from_raw(pgid_i32) else {
        return;
    };
    let _ = kill_process_group(pgid, Signal::TERM);
    let escalate_at = Instant::now() + TERM_GRACE;
    while Instant::now() < escalate_at {
        if test_kill_process_group(pgid).is_err() {
            return;
        }
        thread::sleep(POLL_INTERVAL);
    }
    let _ = kill_process_group(pgid, Signal::KILL);
}
