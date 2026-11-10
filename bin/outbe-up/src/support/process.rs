use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    process::{Command, Output, Stdio},
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

pub fn capture_in(program: impl AsRef<Path>, args: &[&str], directory: &Path) -> Result<String> {
    let program = program.as_ref();
    let output = Command::new(program)
        .args(args)
        .current_dir(directory)
        .output()
        .wrap_err_with(|| format!("failed to execute {}", program.display()))?;
    output_text(program, output)
}

pub fn command_succeeds(program: impl AsRef<Path>, args: &[&str]) -> bool {
    Command::new(program.as_ref())
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn write_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    if temporary.exists() {
        std::fs::remove_file(&temporary)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

pub fn chown(path: &Path, uid: u32, gid: u32) -> Result<()> {
    rustix::fs::chown(
        path,
        Some(rustix::process::Uid::from_raw(uid)),
        Some(rustix::process::Gid::from_raw(gid)),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| format!("failed to change ownership of {}", path.display()))
}

pub fn systemctl(args: &[&str]) -> Result<()> {
    capture("systemctl", args).map(|_| ())
}

pub fn systemctl_is_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", unit])
        .status()
        .is_ok_and(|status| status.success())
}

fn output_text(program: &Path, output: Output) -> Result<String> {
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = match (stdout.trim(), stderr.trim()) {
            ("", stderr) => stderr.to_owned(),
            (stdout, "") => stdout.to_owned(),
            (stdout, stderr) => format!("{stderr}\n{stdout}"),
        };
        bail!(
            "{} exited with {}: {}",
            program.display(),
            output.status,
            detail
        );
    }
    String::from_utf8(output.stdout).wrap_err("command output is not UTF-8")
}
