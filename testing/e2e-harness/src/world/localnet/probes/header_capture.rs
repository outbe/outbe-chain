//! Read-only canonical header capture from a stopped node's storage.

use std::fs::{self, File};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use eyre::{ensure, Result, WrapErr};

use crate::internal::proc::ChildGuard;

#[derive(Debug)]
pub(super) struct HeaderCaptureTimeout {
    pub(super) status: ExitStatus,
    pub(super) stdout: PathBuf,
    pub(super) stderr: PathBuf,
}

impl std::fmt::Display for HeaderCaptureTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "canonical header read exceeded its deadline; exit={}; stdout={}; stderr={}",
            self.status,
            self.stdout.display(),
            self.stderr.display()
        )
    }
}

impl std::error::Error for HeaderCaptureTimeout {}

pub(super) fn header_capture_timeout(
    status: ExitStatus,
    stdout: tempfile::NamedTempFile,
    stderr: tempfile::NamedTempFile,
) -> Result<Output> {
    let (_, stdout) = stdout.keep().wrap_err("preserve timed-out header stdout")?;
    let (_, stderr) = stderr.keep().wrap_err("preserve timed-out header stderr")?;
    Err(HeaderCaptureTimeout {
        status,
        stdout,
        stderr,
    }
    .into())
}

pub(super) fn header_stream_before(file: &mut File, deadline: Instant) -> Result<Option<Vec<u8>>> {
    file.seek(SeekFrom::Start(0))?;
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Ok(None);
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(Some(output));
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

/// Tempfile-backed capture avoids pipe backpressure. Timeout preserves streams
/// on disk without reading beyond the acceptance budget. Owned reap may need
/// its existing cleanup allowance, but can never extend a successful proof.
pub(super) fn capture_header_command_until(
    mut command: Command,
    deadline: Instant,
) -> Result<Output> {
    ensure!(
        Instant::now() < deadline,
        "canonical header read deadline already expired"
    );
    let mut stdout = tempfile::NamedTempFile::new()?;
    let mut stderr = tempfile::NamedTempFile::new()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.as_file().try_clone()?))
        .stderr(Stdio::from(stderr.as_file().try_clone()?));
    let mut child = ChildGuard::spawn("readonly-canonical-header", command)?;
    let status = loop {
        let observed = child.exit_status()?;
        if Instant::now() >= deadline {
            let status = match observed {
                Some(status) => status,
                None => child
                    .fault_and_reap()
                    .wrap_err("reap timed-out owned canonical-header reader")?,
            };
            return header_capture_timeout(status, stdout, stderr);
        }
        if let Some(status) = observed {
            break status;
        }
        sleep(Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())));
    };
    let Some(captured_stdout) = header_stream_before(stdout.as_file_mut(), deadline)? else {
        return header_capture_timeout(status, stdout, stderr);
    };
    let Some(captured_stderr) = header_stream_before(stderr.as_file_mut(), deadline)? else {
        return header_capture_timeout(status, stdout, stderr);
    };
    if Instant::now() >= deadline {
        return header_capture_timeout(status, stdout, stderr);
    }
    Ok(Output {
        status,
        stdout: captured_stdout,
        stderr: captured_stderr,
    })
}

pub(super) fn stopped_header_command(
    binary: &Path,
    data: &Path,
    genesis: &Path,
    height: u64,
) -> Result<Command> {
    for relative in ["db", "static_files", "rocksdb"] {
        ensure!(
            fs::symlink_metadata(data.join(relative))?.is_dir(),
            "canonical header storage is not a directory: {}",
            data.join(relative).display()
        );
    }
    // Reth's readonly CLI creates RocksDB when CURRENT is missing. Refuse to
    // invoke it in that state: collecting evidence must never repair storage.
    ensure!(
        fs::symlink_metadata(data.join("rocksdb/CURRENT"))?.is_file(),
        "canonical header storage lacks regular rocksdb/CURRENT"
    );
    ensure!(
        fs::symlink_metadata(genesis)?.is_file(),
        "canonical header genesis is not a regular file"
    );
    // The DB subcommand does not raise the node's file-descriptor limit itself.
    // Set only this reader child's soft limit so RocksDB can open its files
    // without a performance warning. Preserve the hard limit and strict parser.
    let mut command = Command::new("sh");
    command
        .args([
            "-c",
            "ulimit -Sn 131072 && exec \"$@\"",
            "outbe-header-reader",
        ])
        .arg(binary)
        .arg("db")
        .arg("--datadir")
        .arg(data)
        .arg("--chain")
        .arg(genesis)
        .args([
            "--color",
            "never",
            "--log.file.max-files",
            "0",
            "get",
            "static-file",
            "headers",
        ])
        .arg(height.to_string());
    Ok(command)
}
