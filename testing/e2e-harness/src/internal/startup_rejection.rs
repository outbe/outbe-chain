//! Evidence for one owned negative startup, armed before the child is spawned.
//! TCP polling can detect a transient listener but cannot prove the absence of
//! an arbitrarily short one; acceptance also relies on the production guard's
//! source ordering before the node's networking launch.

use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eyre::{ensure, eyre, Result, WrapErr as _};

use super::proc::ChildGuard;

#[derive(Debug)]
pub(crate) struct StartupRejectionProbe {
    path: PathBuf,
    log: File,
    offset: u64,
    stop: Arc<AtomicBool>,
    monitor: Option<JoinHandle<Result<bool>>>,
}

impl StartupRejectionProbe {
    pub(crate) fn arm(path: &Path, address: SocketAddr) -> Result<Self> {
        let log = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;
        let offset = log.metadata()?.len();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let (ready, armed) = mpsc::sync_channel(1);
        let monitor = thread::Builder::new()
            .name("startup-rpc-probe".into())
            .spawn(move || {
                // The first observation completes before arm() returns, so an
                // already occupied port cannot masquerade as this child's service.
                let initial = tcp_open(address);
                ready
                    .send(initial.as_ref().copied().map_err(|error| error.to_string()))
                    .map_err(|_| eyre!("startup probe readiness receiver disappeared"))?;
                if initial? {
                    return Ok(true);
                }
                while !stopped.load(Ordering::Acquire) {
                    if tcp_open(address)? {
                        return Ok(true);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(false)
            })?;
        let probe = Self {
            path: path.to_owned(),
            log,
            offset,
            stop,
            monitor: Some(monitor),
        };
        let opened = armed
            .recv()
            .wrap_err("arm startup RPC monitor")?
            .map_err(eyre::Report::msg)?;
        ensure!(
            !opened,
            "negative startup RPC port {address} was already open"
        );
        Ok(probe)
    }

    pub(crate) fn wait(
        mut self,
        child: &mut ChildGuard,
        expected: &str,
        timeout: Duration,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.exit_status()? {
                break status;
            }
            ensure!(
                Instant::now() < deadline,
                "negative startup child {} remained running past deadline",
                child.pid()
            );
            thread::sleep(Duration::from_millis(10));
        };
        let opened = self.finish_monitor()?;
        ensure!(
            !opened,
            "negative startup opened its RPC port before exiting"
        );
        ensure!(
            !status.success(),
            "negative startup exited successfully instead of rejecting: {status}"
        );
        let log = self.current_launch_log()?;
        require_terminal_guardrail(&log, expected)
    }

    fn current_launch_log(&mut self) -> Result<String> {
        let original = self.log.metadata()?;
        let current = fs::metadata(&self.path)
            .wrap_err_with(|| format!("read required startup log {}", self.path.display()))?;
        ensure!(
            original.dev() == current.dev() && original.ino() == current.ino(),
            "startup log was replaced after launch"
        );
        ensure!(
            original.len() >= self.offset,
            "startup log was truncated after launch"
        );
        self.log.seek(SeekFrom::Start(self.offset))?;
        let mut log = String::new();
        self.log.read_to_string(&mut log)?;
        Ok(log)
    }

    fn finish_monitor(&mut self) -> Result<bool> {
        self.stop.store(true, Ordering::Release);
        self.monitor
            .take()
            .ok_or_else(|| eyre!("startup monitor already consumed"))?
            .join()
            .map_err(|_| eyre!("startup RPC monitor panicked"))?
    }
}

impl Drop for StartupRejectionProbe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
    }
}

fn tcp_open(address: SocketAddr) -> Result<bool> {
    match TcpStream::connect_timeout(&address, Duration::from_millis(50)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => Ok(false),
        Err(error) => Err(error).wrap_err_with(|| format!("observe startup RPC port {address}")),
    }
}

fn require_terminal_guardrail(log: &str, expected: &str) -> Result<()> {
    let lines = log
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let errors = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("Error:"))
        .collect::<Vec<_>>();
    ensure!(
        errors.len() == 1,
        "current startup log must contain exactly one terminal Error, found {}",
        errors.len()
    );
    let index = errors[0].0;
    ensure!(
        lines[index] == "Error: execution node failed"
            && lines.get(index + 1) == Some(&"Caused by:")
            && lines.get(index + 2) == Some(&expected),
        "negative startup did not terminate on exact guardrail {expected:?}:\n{log}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::process::{Command, Stdio};

    const GUARD: &str =
        "mandatory GramineDirectDev ChainSpec requires --tee-enclave-socket before node startup";

    fn unused_address() -> SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
    }

    fn child(path: &Path, success: bool, diagnostic: &str) -> ChildGuard {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf 'Error: execution node failed\\n\\nCaused by:\\n    %s\\n' \"$1\"; exit \"$2\"",
            "startup-fixture",
            diagnostic,
            if success { "0" } else { "1" },
        ]);
        command.stdout(Stdio::from(
            OpenOptions::new().append(true).open(path).unwrap(),
        ));
        ChildGuard::spawn("startup fixture", command).unwrap()
    }

    #[test]
    fn startup_rejection_requires_unsuccessful_owned_exit_and_exact_terminal_cause() {
        for (success, diagnostic, accepted) in [
            (false, GUARD, true),
            (true, GUARD, false),
            (false, "unrelated CLI failure", false),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("node.log");
            let probe = StartupRejectionProbe::arm(&path, unused_address()).unwrap();
            let mut owned = child(&path, success, diagnostic);
            assert_eq!(
                probe
                    .wait(&mut owned, GUARD, Duration::from_secs(5))
                    .is_ok(),
                accepted
            );
        }
    }

    #[test]
    fn startup_rejection_does_not_accept_a_previous_launch_diagnostic() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("node.log");
        fs::write(
            &path,
            format!("Error: execution node failed\nCaused by:\n{GUARD}\n"),
        )
        .unwrap();
        let probe = StartupRejectionProbe::arm(&path, unused_address()).unwrap();
        let mut owned = child(&path, false, "unrelated CLI failure");
        assert!(probe
            .wait(&mut owned, GUARD, Duration::from_secs(5))
            .is_err());
    }

    #[test]
    fn startup_rejection_requires_the_current_log_file_to_remain_available() {
        for replace in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("node.log");
            let probe = StartupRejectionProbe::arm(&path, unused_address()).unwrap();
            let mut owned = child(&path, false, GUARD);
            fs::remove_file(&path).unwrap();
            if replace {
                fs::write(
                    &path,
                    format!("Error: execution node failed\nCaused by:\n{GUARD}\n"),
                )
                .unwrap();
            }
            assert!(probe
                .wait(&mut owned, GUARD, Duration::from_secs(5))
                .is_err());
        }
    }

    #[test]
    fn startup_rejection_latches_a_listener_even_after_it_closes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("node.log");
        let address = unused_address();
        let probe = StartupRejectionProbe::arm(&path, address).unwrap();
        let listener = TcpListener::bind(address).unwrap();
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "monitor never observed listener");
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("accept monitor connection: {error}"),
            }
        }
        drop(listener);
        let mut owned = child(&path, false, GUARD);
        let error = probe
            .wait(&mut owned, GUARD, Duration::from_secs(5))
            .unwrap_err();
        assert!(error.to_string().contains("opened its RPC port"));
    }
}
