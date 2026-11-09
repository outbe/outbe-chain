use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::{Command, Output},
};

use eyre::{bail, ensure, Context, Result};

pub const ROOT: &str = "/opt/outbe-chain";
pub const STATE_ROOT: &str = "/var/lib/outbe/operator";
const LOCK_PATH: &str = "/run/lock/outbe-validator.lock";

pub struct LifecycleLock {
    _file: File,
}

impl LifecycleLock {
    pub fn acquire() -> Result<Self> {
        require_root()?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(LOCK_PATH)
            .wrap_err("failed to open lifecycle lock")?;
        if let Err(error) =
            rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        {
            let error = std::io::Error::from(error);
            if error.kind() == std::io::ErrorKind::WouldBlock {
                bail!("another validator lifecycle command is running");
            }
            return Err(error).wrap_err("failed to acquire lifecycle lock");
        }
        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        file.sync_data()?;
        Ok(Self { _file: file })
    }
}

pub fn require_root() -> Result<()> {
    ensure!(
        rustix::process::geteuid().is_root(),
        "validator lifecycle commands must run as root"
    );
    Ok(())
}

pub fn run(program: impl AsRef<Path>, args: &[&str]) -> Result<()> {
    let program = program.as_ref();
    let status = Command::new(program)
        .args(args)
        .status()
        .wrap_err_with(|| format!("failed to execute {}", program.display()))?;
    ensure!(
        status.success(),
        "{} exited with {status}",
        program.display()
    );
    Ok(())
}

pub fn capture(program: impl AsRef<Path>, args: &[&str]) -> Result<String> {
    let program = program.as_ref();
    let output = Command::new(program)
        .args(args)
        .output()
        .wrap_err_with(|| format!("failed to execute {}", program.display()))?;
    output_text(program, output)
}

pub fn systemctl(args: &[&str]) -> Result<()> {
    run("systemctl", args)
}

pub fn systemctl_is_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .status()
        .is_ok_and(|status| status.success())
}

fn output_text(program: &Path, output: Output) -> Result<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{} exited with {}: {}",
            program.display(),
            output.status,
            stderr.trim()
        );
    }
    String::from_utf8(output.stdout).wrap_err("command output is not UTF-8")
}
