//! Execution of prepared `ssh` and `scp` commands and mapping of their output to results.

use std::{
    io::{self, ErrorKind, Write},
    process::{Child, ChildStdin, Command, Output, Stdio},
};

use eyre::{bail, ensure, Context, Result};

/// Run `command` remotely and return its raw stdout, or fail with its trimmed stderr.
pub(crate) fn capture_raw_with(mut process: Command, label: &str, command: &str) -> Result<String> {
    let output = process.arg(command).output().wrap_err("run ssh")?;
    if !output.status.success() {
        bail!(
            "SSH command failed on {label}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run `command` remotely with `stdin_bytes` on its stdin and return its raw stdout.
pub(crate) fn capture_stdin_with(
    mut process: Command,
    label: &str,
    command: &str,
    stdin_bytes: &[u8],
) -> Result<String> {
    let child = process
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err("spawn ssh")?;
    run_child_with_stdin(child, label, stdin_bytes)
}

/// Run `command` remotely with inherited stdio.
pub(crate) fn run_with(mut process: Command, label: &str, command: &str) -> Result<()> {
    let status = process.arg(command).status().wrap_err("run ssh")?;
    ensure!(status.success(), "SSH command failed on {label}");
    Ok(())
}

/// Run `command` remotely and show its stdout and stderr only when it fails.
pub(crate) fn run_quiet_with(mut process: Command, label: &str, command: &str) -> Result<()> {
    let output = process.arg(command).output().wrap_err("run ssh")?;
    ensure_quiet_success(&output, label)
}

/// Run a prepared `scp` copy command.
pub(crate) fn run_scp(mut process: Command, label: &str) -> Result<()> {
    let status = process.status().wrap_err("run scp")?;
    ensure!(status.success(), "SCP failed for {label}");
    Ok(())
}

/// Fail with the joined non-empty stdout and stderr when the SSH command exited with an error.
pub(crate) fn ensure_quiet_success(output: &Output, label: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let details = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    bail!("SSH command failed on {label}:\n{details}");
}

/// Stream `stdin_bytes` to `child` on a scoped writer thread while draining stdout/stderr via
/// `wait_with_output`. Tolerates `BrokenPipe` if the remote exits before consuming stdin so the
/// child is always reaped and remote stderr is surfaced as the primary error.
pub(crate) fn run_child_with_stdin(
    mut child: Child,
    label: &str,
    stdin_bytes: &[u8],
) -> Result<String> {
    let stdin = child.stdin.take();
    let (write_res, output) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || write_stdin_tolerating_broken_pipe(stdin, stdin_bytes));
        let output = child.wait_with_output().wrap_err("wait for ssh");
        let write_res = writer
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("ssh stdin writer thread panicked")));
        (write_res, output)
    });

    let output = output?;
    if !output.status.success() {
        return Err(stdin_command_failure(&output, write_res, label));
    }
    write_res.wrap_err("write ssh stdin")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Write all bytes to the child stdin. A remote that exits early closes the pipe, so a
/// `BrokenPipe` error is not a failure here.
fn write_stdin_tolerating_broken_pipe(stdin: Option<ChildStdin>, bytes: &[u8]) -> io::Result<()> {
    let Some(mut pipe) = stdin else {
        return Ok(());
    };
    match pipe.write_all(bytes) {
        Err(err) if err.kind() != ErrorKind::BrokenPipe => Err(err),
        _ => Ok(()),
    }
}

/// Describe a failed SSH command that received stdin. Remote stderr comes first, then the stdin
/// write error, then remote stdout.
fn stdin_command_failure(output: &Output, write_res: io::Result<()>, label: &str) -> eyre::Report {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr.trim();
    if !detail.is_empty() {
        return eyre::eyre!("SSH command failed on {label}: {detail}");
    }
    if let Err(write_err) = write_res {
        return eyre::eyre!("SSH command failed on {label} (stdin write error: {write_err})");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    eyre::eyre!("SSH command failed on {label}: {}", stdout.trim())
}
