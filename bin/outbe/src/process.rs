use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use eyre::{bail, ensure, Context, Result};

pub const ROOT: &str = "/opt/outbe-chain";
pub const STATE_ROOT: &str = "/var/lib/outbe/operator";
const LOCK_PATH: &str = "/run/lock/outbe-validator.lock";

pub struct LifecycleLock {
    path: PathBuf,
}

impl LifecycleLock {
    pub fn acquire() -> Result<Self> {
        require_root()?;
        let path = PathBuf::from(LOCK_PATH);
        for _ in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id())?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let pid = fs::read_to_string(&path)
                        .ok()
                        .and_then(|value| value.trim().parse::<u32>().ok());
                    if pid.is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists()) {
                        bail!("another validator lifecycle command is running");
                    }
                    fs::remove_file(&path).wrap_err("failed to remove stale lifecycle lock")?;
                }
                Err(error) => return Err(error).wrap_err("failed to acquire lifecycle lock"),
            }
        }
        bail!("failed to acquire lifecycle lock")
    }
}

impl Drop for LifecycleLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
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
