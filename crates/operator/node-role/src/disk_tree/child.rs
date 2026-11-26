//! Cancellable control of child process groups (`gzip -dc`, `outbe-keygen`).

use std::{
    io::{self, Read},
    process::{Child, ChildStdout},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use eyre::{bail, Context, Result};
use rustix::{
    fs::{fcntl_getfl, fcntl_setfl, OFlags},
    process::{kill_process_group, test_kill_process_group, Pid, Signal},
};

/// Non-blocking wrapper around a child process's stdout pipe so stalled subprocesses (e.g. a
/// blocked `gzip -dc`) never prevent `SIGINT`/`SIGTERM` cancellation from terminating the child
/// process group promptly.
pub(super) struct CancellableChildStdout<'a> {
    stdout: ChildStdout,
    child_pid: u32,
    cancel: &'a AtomicBool,
}

impl<'a> CancellableChildStdout<'a> {
    pub(super) fn new(stdout: ChildStdout, child_pid: u32, cancel: &'a AtomicBool) -> Result<Self> {
        let flags = fcntl_getfl(&stdout)
            .map_err(io::Error::from)
            .wrap_err("read gzip stdout descriptor flags")?;
        fcntl_setfl(&stdout, flags | OFlags::NONBLOCK)
            .map_err(io::Error::from)
            .wrap_err("set O_NONBLOCK on gzip stdout pipe")?;
        Ok(Self {
            stdout,
            child_pid,
            cancel,
        })
    }
}

impl Read for CancellableChildStdout<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                terminate_process_group(self.child_pid);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "gzip -dc canceled by signal",
                ));
            }
            match self.stdout.read(buf) {
                Ok(n) => return Ok(n),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

pub(super) fn wait_child_cancellable(
    child: &mut Child,
    cancel: &AtomicBool,
    label: &str,
) -> Result<std::process::ExitStatus> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            terminate_process_group(child.id());
            let _ = child.wait();
            bail!("{label} canceled by signal");
        }
        if let Some(status) = child
            .try_wait()
            .wrap_err_with(|| format!("wait for {label}"))?
        {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub(crate) fn terminate_process_group(pid: u32) {
    let Some(pgid) = Pid::from_raw(pid as i32) else {
        return;
    };
    let _ = kill_process_group(pgid, Signal::TERM);
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if test_kill_process_group(pgid).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let _ = kill_process_group(pgid, Signal::KILL);
}
