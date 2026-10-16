//! Owned enclave launch and positive startup readiness.

use std::fs::{self, OpenOptions};
use std::net::{SocketAddr, TcpStream};
use std::process::{ExitStatus, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use eyre::{bail, Result, WrapErr};

use super::enclave_launch::{
    build_enclave_command, build_native_command, EnclaveLaunch, EnclaveSpec,
};
use super::{base_cmd, docker_rm, redact_args_for_log, wait_tcp, ChildGuard, DockerGuard};
use crate::internal::launch_log::LaunchLog;

/// An owned enclave: the foreground child (killed on drop) plus, for the
/// containerized profile, a `docker rm -f` backstop for the container itself.
/// Field order matters. Drop releases the `docker run` client first, and then
/// the backstop force-removes the container. A native host enclave has no container, so the
/// child guard alone owns its whole lifetime.
#[derive(Debug)]
pub(crate) struct EnclaveGuard {
    pub(super) child: ChildGuard,
    pub(super) docker: Option<DockerGuard>,
}

impl EnclaveGuard {
    #[cfg(test)]
    pub(crate) fn from_test_child(child: ChildGuard) -> Self {
        Self {
            child,
            docker: None,
        }
    }

    /// PID of the owned foreground launcher (Docker client for containerized SGX).
    pub(crate) fn pid(&self) -> u32 {
        self.child.pid()
    }

    pub(crate) fn exit_status(&mut self) -> Result<Option<ExitStatus>> {
        self.child.exit_status()
    }

    pub(crate) fn stop_and_reap(&mut self) -> Result<ExitStatus> {
        self.child.stop_and_reap()
    }
}

pub(super) fn enclave_listener_ready(
    status: Option<ExitStatus>,
    log: &str,
    port: u16,
    reachable: bool,
) -> Result<bool> {
    if let Some(status) = status {
        bail!("owned enclave child exited before readiness: {status}");
    }
    let prefix = format!("outbe-tee-enclave: listening on tcp://127.0.0.1:{port} (attestation: ");
    Ok(reachable
        && log.split_inclusive('\n').any(|line| {
            line.starts_with(&prefix)
                && line.contains("; enclave identity: ")
                && line.ends_with(")\n")
        }))
}

/// Positive startup only: observe this process's successful bind, not merely
/// an open port. Authentication and sealed recovery remain NodeHost operations.
pub(crate) fn spawn_enclave_ready(spec: EnclaveSpec, deadline: Instant) -> Result<EnclaveGuard> {
    eyre::ensure!(
        Instant::now() < deadline,
        "enclave startup deadline already expired"
    );
    let address = ([127, 0, 0, 1], spec.tee_port).into();
    refuse_occupied_endpoint(address, deadline)?;
    let mut log = LaunchLog::arm(&spec.log_path)?;
    let path = spec.log_path.clone();
    let name = spec.name.clone();
    let port = spec.tee_port;
    let started = Instant::now();
    let mut child = spawn_enclave_before(spec, Some(deadline))?;
    let evidence = path.with_file_name(format!("enclave-startup-{}.json", child.child.pid()));
    let starting = serde_json::json!({"phase": "starting", "name": name,
        "pid": child.child.pid(), "endpoint": address.to_string(),
        "budget_ms": deadline.saturating_duration_since(started).as_millis(), "log": path});
    fs::write(&evidence, serde_json::to_vec_pretty(&starting)?)?;
    let result = wait_until_listening(&mut child, &mut log, address, deadline);
    // Capture original status and this attempt's logs before Drop can cause
    // termination. A cleanup signal is not evidence of a spontaneous crash.
    let final_status = child.child.exit_status();
    let status_description = format!("{final_status:?}");
    let result = result.and_then(|()| {
        enclave_listener_ready(final_status?, "", port, false)?;
        eyre::ensure!(
            Instant::now() < deadline,
            "enclave startup deadline expired before handoff"
        );
        Ok(())
    });
    let mut report = starting;
    report["phase"] = serde_json::json!(if result.is_ok() {
        "listening"
    } else {
        "failed"
    });
    report["elapsed_ms"] = serde_json::json!(started.elapsed().as_millis());
    report["child_status_before_cleanup"] = serde_json::json!(status_description);
    if let Err(error) = &result {
        record_startup_failure(&mut report, error, &child, &mut log);
    }
    if let Err(error) = fs::write(&evidence, serde_json::to_vec_pretty(&report)?) {
        eprintln!(
            "cannot retain enclave startup evidence {}: {error}",
            evidence.display()
        );
        result?;
        return Err(error).wrap_err("retain enclave startup evidence");
    }
    result.wrap_err_with(|| {
        format!(
            "enclave {name} startup failed; diagnostics: {}",
            evidence.display()
        )
    })?;
    Ok(child)
}

fn refuse_occupied_endpoint(address: SocketAddr, deadline: Instant) -> Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    eyre::ensure!(
        !remaining.is_zero(),
        "enclave startup deadline already expired"
    );
    match TcpStream::connect_timeout(&address, remaining.min(Duration::from_millis(100))) {
        Ok(_) => {
            bail!("enclave endpoint {address} is already occupied; refusing to replace its owner");
        }
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => Ok(()),
        Err(error) => Err(error).wrap_err("probe enclave endpoint before launch"),
    }
}

fn wait_until_listening(
    child: &mut EnclaveGuard,
    log: &mut LaunchLog,
    address: SocketAddr,
    deadline: Instant,
) -> Result<()> {
    let port = address.port();
    loop {
        let status = child.child.exit_status()?;
        enclave_listener_ready(status, "", port, false)?;
        eyre::ensure!(
            Instant::now() < deadline,
            "owned enclave remained alive without readiness until deadline"
        );
        let log = log.read()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("enclave startup deadline expired before TCP probe");
        }
        let reachable = probe_listener(address, remaining)?;
        let status = child.child.exit_status()?;
        eyre::ensure!(
            Instant::now() < deadline,
            "enclave became observable after its startup deadline"
        );
        if enclave_listener_ready(status, &log, port, reachable)? {
            return Ok(());
        }
        sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(100)),
        );
    }
}

fn probe_listener(address: SocketAddr, remaining: Duration) -> Result<bool> {
    match TcpStream::connect_timeout(&address, remaining.min(Duration::from_millis(100))) {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error).wrap_err("probe current enclave listener"),
    }
}

fn record_startup_failure(
    report: &mut serde_json::Value,
    error: &eyre::Report,
    child: &EnclaveGuard,
    log: &mut LaunchLog,
) {
    report["error"] = serde_json::json!(format!("{error:#}"));
    report["container_state_before_cleanup"] = match &child.docker {
        Some(container) => match enclave_container_state(container) {
            Ok(value) => value,
            Err(error) => serde_json::json!({"unavailable": format!("{error:#}")}),
        },
        None => serde_json::Value::Null,
    };
    report["log_tail"] = match log.read() {
        Ok(text) => {
            let mut lines = text.lines().rev().take(40).collect::<Vec<_>>();
            lines.reverse();
            serde_json::json!(lines.join("\n"))
        }
        Err(error) => serde_json::json!({"unavailable": format!("{error:#}")}),
    };
}

fn enclave_container_state(container: &DockerGuard) -> Result<serde_json::Value> {
    let mut command = base_cmd("docker", container.sudo);
    command
        .args(["inspect", "--format", "{{json .State}}", &container.name])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("container state observation timed out");
        }
        sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output()?;
    eyre::ensure!(
        output.status.success(),
        "container state unavailable: {}",
        output.status
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Launch the enclave in the **foreground** as an owned child, returning a guard
/// that kills it (and, under Gramine, `docker rm -f`s its container) on drop.
/// Positive launches use [`spawn_enclave_ready`]; rejection probes keep their
/// separate fail-closed observation of the expected policy error.
pub(crate) fn spawn_enclave(spec: EnclaveSpec) -> Result<EnclaveGuard> {
    spawn_enclave_before(spec, None)
}

pub(super) fn spawn_enclave_before(
    spec: EnclaveSpec,
    deadline: Option<Instant>,
) -> Result<EnclaveGuard> {
    let (mut cmd, docker) = match &spec.launch {
        EnclaveLaunch::Gramine { image_id } => {
            // Remove any stale container of the same name first.
            docker_rm(&spec.name, spec.sudo);
            let image_id = image_id.clone();
            (
                build_enclave_command(&spec, &image_id)?,
                Some(DockerGuard::new(spec.name.clone(), spec.sudo)),
            )
        }
        EnclaveLaunch::NativeHost => {
            // A container has a name to force-remove; a host process does not.
            // A re-bootstrap that orphaned the previous run's enclave would
            // leave it bound here, and the node would silently attach to a
            // stale identity and die on offer-key divergence. Fail closed
            // instead of killing a process this run does not own.
            if wait_tcp(spec.tee_port, 1) {
                bail!(
                    "127.0.0.1:{} is already bound - an enclave from an earlier run is still \
                     listening. Run `outbe-e2e localnet stop` (or kill it) before starting.",
                    spec.tee_port
                );
            }
            (build_native_command(&spec)?, None)
        }
    };

    // Foreground: own the child, stream its logs to <node>/enclave.log.
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&spec.log_path)
        .wrap_err_with(|| format!("open {}", spec.log_path.display()))?;
    let log2 = log.try_clone()?;
    cmd.stdout(Stdio::from(log))
        .stderr(Stdio::from(log2))
        .stdin(Stdio::null());

    if spec.debug {
        let prog = cmd.get_program().to_string_lossy().into_owned();
        let rest: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let rest = redact_args_for_log(&rest);
        eprintln!(
            "[localnet] enclave {} (tee {}): {prog} {}",
            spec.name,
            spec.tee_port,
            rest.join(" ")
        );
        eprintln!("           log: {}", spec.log_path.display());
    }

    if let Some(deadline) = deadline {
        eyre::ensure!(
            Instant::now() < deadline,
            "enclave startup deadline expired during launch preparation"
        );
    }
    let child = ChildGuard::spawn(format!("enclave {}", spec.name), cmd)?;
    Ok(EnclaveGuard { child, docker })
}
