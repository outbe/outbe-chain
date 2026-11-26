//! Isolated GPT + ext4 data disk container format expected by dstack guests.
//!
//! Layout matches `dstack-prepare.sh` (`sgdisk -n 1:1MiB:0 -c 1:dstack-data -t 1:8300`)
//! and `dstack-util/src/system_setup.rs` (`dstack.storage_encrypted=false`,
//! `dstack.storage_fs=ext4`), where an existing ext4 partition labelled `dstack-data`
//! is checked with `e2fsck -f -p`, resized with `resize2fs`, and mounted at
//! `/dstack/persistent` without reformatting.

pub(crate) mod subprocess;

use std::{
    env,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};

use eyre::{bail, ensure, Context, Result};
use rustix::{
    fd::OwnedFd,
    fs::{
        fchmod, flock, openat, renameat_with, FlockOperation, Mode, OFlags, RenameFlags,
        SeekFrom as RustixSeekFrom, CWD,
    },
    io::Errno,
};

/// Standard 512-byte sector size for GPT LBA calculations.
pub(crate) const SECTOR_SIZE: u64 = 512;
/// 4 KiB ext4 block size.
pub(crate) const EXT4_BLOCK_SIZE: u64 = 4096;
/// 1 MiB start alignment (LBA 2048), matching `dstack-prepare.sh`.
pub(crate) const PARTITION_START_SECTOR: u64 = 2048;
/// Byte offset of partition 1 inside the raw disk image.
pub(crate) const PARTITION_OFFSET_BYTES: u64 = PARTITION_START_SECTOR * SECTOR_SIZE;
/// Sectors reserved at the end of the disk for the backup GPT header and table.
const GPT_BACKUP_SECTORS: u64 = 33;
/// GPT partition label scanned by `dstack-prepare.sh`.
pub(crate) const PARTITION_LABEL: &str = "dstack-data";
/// Linux filesystem data partition type GUID (`8300`).
pub(crate) const LINUX_FS_TYPE_GUID: &str = "0FC63DAF-8483-4772-8E79-3D69D8477DE4";
/// Fixed past timestamp passed via `E2FSPROGS_FAKE_TIME` (2023-11-14T22:13:20Z).
const FIXED_TIMESTAMP: &str = "1700000000";

/// Deterministic identifiers for one disk image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiskIds {
    pub(crate) disk_guid: String,
    pub(crate) part_guid: String,
    pub(crate) fs_uuid: String,
    pub(crate) hash_seed: String,
}

/// One entry to populate inside the ext4 filesystem as `uid=0, gid=0`.
#[derive(Clone, Debug)]
pub(crate) enum FsEntry {
    Dir {
        /// Path inside the filesystem without leading slash (e.g. `outbe-chain/keys`).
        target: String,
        mode: u32,
    },
    File {
        source: PathBuf,
        target: String,
        mode: u32,
    },
}

/// Filesystem entries plus the deterministic identifiers derived from their content.
#[derive(Clone, Debug)]
pub(crate) struct DiskContents {
    pub(crate) ids: DiskIds,
    pub(crate) entries: Vec<FsEntry>,
}

/// Resolved external tools needed to build the container image.
pub(crate) struct Tools {
    pub(crate) sfdisk: PathBuf,
    pub(crate) mke2fs: PathBuf,
    pub(crate) debugfs: PathBuf,
}

impl Tools {
    pub(crate) fn resolve() -> Result<Self> {
        Ok(Self {
            sfdisk: find_in_path("sfdisk", "util-linux / fdisk")?,
            mke2fs: find_in_path("mke2fs", "e2fsprogs")?,
            debugfs: find_in_path("debugfs", "e2fsprogs")?,
        })
    }
}

/// Compute the aligned partition size in sectors and bytes for a raw disk of `disk_bytes`.
pub(crate) fn partition_geometry(disk_bytes: u64) -> Result<(u64, u64)> {
    ensure!(
        disk_bytes.is_multiple_of(SECTOR_SIZE),
        "disk size must be a multiple of {SECTOR_SIZE} bytes"
    );
    let total_sectors = disk_bytes / SECTOR_SIZE;
    let min_sectors = PARTITION_START_SECTOR + GPT_BACKUP_SECTORS + 8 * 1024; // >= 4 MiB fs
    ensure!(
        total_sectors >= min_sectors,
        "disk size {disk_bytes} bytes is too small for a GPT + ext4 data disk"
    );
    let last_usable = total_sectors - 1 - GPT_BACKUP_SECTORS;
    let raw_part_sectors = last_usable - PARTITION_START_SECTOR + 1;
    let sectors_per_block = EXT4_BLOCK_SIZE / SECTOR_SIZE;
    let part_sectors = (raw_part_sectors / sectors_per_block) * sectors_per_block;
    Ok((part_sectors, part_sectors * SECTOR_SIZE))
}

/// Refuse any existing path at `output`, including a dangling symlink.
pub(crate) fn ensure_output_path_absent(output: &Path) -> Result<()> {
    match fs::symlink_metadata(output) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).wrap_err_with(|| format!("failed to stat {}", output.display())),
        Ok(_) => Err(eyre::eyre!(
            "output disk image already exists: {}",
            output.display()
        )),
    }
}

/// Exclusive flock on a private `.outbe-vm-disk-*` staging directory.
///
/// Field order ensures `dir` drops before `_lock`.
struct LockedWorkDir {
    dir: tempfile::TempDir,
    _lock: OwnedFd,
}

impl LockedWorkDir {
    /// Create the staging directory in `parent` and hold an exclusive directory flock on it for
    /// its whole life. A brief shared flock on `parent` serializes creation against
    /// `--cleanup-stale`, so concurrent cleanup never deletes a live disk-build staging directory.
    fn create(parent: &Path) -> Result<Self> {
        let parent_fd = open_dir(parent, OFlags::empty())
            .wrap_err_with(|| format!("open staging parent {}", parent.display()))?;
        flock(&parent_fd, FlockOperation::LockShared)
            .map_err(io::Error::from)
            .wrap_err_with(|| format!("lock staging parent {}", parent.display()))?;
        let dir = tempfile::Builder::new()
            .prefix(".outbe-vm-disk-")
            .tempdir_in(parent)?;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
        let lock = open_dir(dir.path(), OFlags::NOFOLLOW)
            .wrap_err_with(|| format!("open staging dir {} for flock", dir.path().display()))?;
        flock(&lock, FlockOperation::LockExclusive)
            .map_err(io::Error::from)
            .wrap_err_with(|| format!("flock staging dir {}", dir.path().display()))?;
        drop(parent_fd);
        Ok(Self { dir, _lock: lock })
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Open a directory read-only with `O_CLOEXEC` plus `extra_flags`.
fn open_dir(path: &Path, extra_flags: OFlags) -> io::Result<OwnedFd> {
    openat(
        CWD,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | extra_flags | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
}

/// Write a deterministic sparse raw GPT + ext4 disk image to `output` with mode `0600` from
/// creation, atomic no-clobber publication (`RENAME_NOREPLACE`), and cooperative signal
/// cancellation.
pub(crate) fn write_disk_image(
    tools: &Tools,
    output: &Path,
    disk_bytes: u64,
    contents: &DiskContents,
    cancel: &AtomicBool,
) -> Result<()> {
    ensure_not_canceled(cancel, "disk build")?;
    ensure_output_path_absent(output)?;
    let (part_sectors, part_bytes) = partition_geometry(disk_bytes)?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let work = LockedWorkDir::create(parent)?;
    let work_path = work.path();

    // 1. Build a deterministic sparse ext4 filesystem in a 0600 standalone partition file.
    ensure_not_canceled(cancel, "disk build")?;
    let part_path = work_path.join("part.ext4");
    create_sparse_file(&part_path, part_bytes)?;
    format_ext4(tools, &part_path, &contents.ids, cancel)?;
    populate_ext4(tools, &part_path, work_path, &contents.entries, cancel)?;

    // 2. Create the 0600 sparse raw disk image and write its deterministic GPT table.
    ensure_not_canceled(cancel, "disk build")?;
    let disk_path = work_path.join("disk.img");
    create_sparse_file(&disk_path, disk_bytes)?;
    write_gpt(tools, &disk_path, part_sectors, &contents.ids, cancel)?;

    // 3. Sparse-copy the populated ext4 partition into the disk image at 1 MiB.
    ensure_not_canceled(cancel, "disk build")?;
    copy_sparse_into(&part_path, &disk_path, PARTITION_OFFSET_BYTES, cancel)?;
    seal_disk_file(&disk_path)?;

    // 4. Atomically publish the finished image without replacing any existing file or symlink.
    ensure_not_canceled(cancel, "disk publish")?;
    publish_no_replace(&disk_path, output)?;
    if let Ok(dir_handle) = File::open(parent) {
        let _ = dir_handle.sync_all();
    }
    Ok(())
}

/// Re-assert mode 0600 and fsync before atomic no-clobber publication.
fn seal_disk_file(disk_path: &Path) -> Result<()> {
    let disk_file =
        File::open(disk_path).wrap_err_with(|| format!("open {} for sync", disk_path.display()))?;
    fchmod(&disk_file, Mode::from_raw_mode(0o600))
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("fchmod 0600 on {}", disk_path.display()))?;
    disk_file
        .sync_all()
        .wrap_err_with(|| format!("fsync {}", disk_path.display()))
}

fn already_exists_error(dst: &Path) -> eyre::Report {
    eyre::eyre!("output disk image already exists: {}", dst.display())
}

fn publish_no_replace(src: &Path, dst: &Path) -> Result<()> {
    match renameat_with(CWD, src, CWD, dst, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(Errno::EXIST) => Err(already_exists_error(dst)),
        Err(Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) => publish_by_hard_link(src, dst),
        Err(err) => Err(io::Error::from(err))
            .wrap_err_with(|| format!("failed to publish disk image to {}", dst.display())),
    }
}

/// Fallback for filesystems without `RENAME_NOREPLACE`: `hard_link` is POSIX-atomic `EEXIST`
/// even when `dst` is a dangling symlink.
fn publish_by_hard_link(src: &Path, dst: &Path) -> Result<()> {
    match fs::hard_link(src, dst) {
        Ok(()) => {
            let _ = fs::remove_file(src);
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Err(already_exists_error(dst)),
        Err(err) => {
            Err(err).wrap_err_with(|| format!("failed to publish disk image to {}", dst.display()))
        }
    }
}

fn ensure_not_canceled(cancel: &AtomicBool, stage: &str) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "{stage} canceled by signal"
    );
    Ok(())
}

fn find_in_path(tool: &str, package_hint: &str) -> Result<PathBuf> {
    let path_var = env::var_os("PATH").unwrap_or_default();
    env::split_paths(&path_var)
        .map(|dir| dir.join(tool))
        .find(|candidate| is_executable_file(candidate))
        .ok_or_else(|| {
            eyre::eyre!("required tool '{tool}' not found in PATH (install {package_hint})")
        })
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn create_sparse_file(path: &Path, size: u64) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .wrap_err_with(|| format!("failed to create {}", path.display()))?;
    fchmod(&file, Mode::from_raw_mode(0o600))
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("failed to set mode 0600 on {}", path.display()))?;
    file.set_len(size)
        .wrap_err_with(|| format!("failed to size {} to {size} bytes", path.display()))?;
    Ok(())
}

/// Build an e2fsprogs command with the fixed `E2FSPROGS_FAKE_TIME` for deterministic output.
fn e2fsprogs_command(program: &Path, args: &[&OsStr]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args).env("E2FSPROGS_FAKE_TIME", FIXED_TIMESTAMP);
    cmd
}

fn format_ext4(tools: &Tools, part_path: &Path, ids: &DiskIds, cancel: &AtomicBool) -> Result<()> {
    // Disable orphan_file for compatibility with older guest e2fsck builds.
    let extended = format!(
        "root_owner=0:0,hash_seed={},lazy_itable_init=0,lazy_journal_init=0",
        ids.hash_seed
    );
    let block_size = EXT4_BLOCK_SIZE.to_string();
    let cmd = e2fsprogs_command(
        &tools.mke2fs,
        &[
            OsStr::new("-q"),
            OsStr::new("-t"),
            OsStr::new("ext4"),
            OsStr::new("-b"),
            OsStr::new(&block_size),
            OsStr::new("-O"),
            OsStr::new("^orphan_file"),
            OsStr::new("-U"),
            OsStr::new(&ids.fs_uuid),
            OsStr::new("-E"),
            OsStr::new(&extended),
            part_path.as_os_str(),
        ],
    );
    run_tool_cancellable(cmd, None, "mke2fs", cancel)?;
    Ok(())
}

impl FsEntry {
    /// Render the debugfs commands that create this entry as `uid=0, gid=0` with its mode.
    fn debugfs_commands(&self) -> Result<String> {
        match self {
            Self::Dir { target, mode } => {
                ensure_safe_debugfs_arg(target)?;
                let mode_octal = 0o040000 | (mode & 0o7777);
                Ok(format!(
                    "mkdir \"{target}\"\n\
                     set_inode_field \"{target}\" uid 0\n\
                     set_inode_field \"{target}\" gid 0\n\
                     set_inode_field \"{target}\" mode 0{mode_octal:o}\n"
                ))
            }
            Self::File {
                source,
                target,
                mode,
            } => {
                let source_str = source
                    .to_str()
                    .ok_or_else(|| eyre::eyre!("non-UTF-8 source path: {}", source.display()))?;
                ensure_safe_debugfs_arg(source_str)?;
                ensure_safe_debugfs_arg(target)?;
                let mode_octal = 0o100000 | (mode & 0o7777);
                Ok(format!(
                    "write \"{source_str}\" \"{target}\"\n\
                     set_inode_field \"{target}\" uid 0\n\
                     set_inode_field \"{target}\" gid 0\n\
                     set_inode_field \"{target}\" mode 0{mode_octal:o}\n"
                ))
            }
        }
    }
}

fn populate_ext4(
    tools: &Tools,
    part_path: &Path,
    work_dir: &Path,
    entries: &[FsEntry],
    cancel: &AtomicBool,
) -> Result<()> {
    let mut script = String::new();
    for entry in entries {
        script.push_str(&entry.debugfs_commands()?);
    }

    let script_path = work_dir.join("populate.debugfs");
    let mut script_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&script_path)?;
    fchmod(&script_file, Mode::from_raw_mode(0o600)).map_err(io::Error::from)?;
    script_file.write_all(script.as_bytes())?;
    script_file.sync_all()?;
    drop(script_file);

    let cmd = e2fsprogs_command(
        &tools.debugfs,
        &[
            OsStr::new("-w"),
            OsStr::new("-f"),
            script_path.as_os_str(),
            part_path.as_os_str(),
        ],
    );
    let stderr = run_tool_cancellable(cmd, None, "debugfs", cancel)?;
    ensure_debugfs_succeeded(&stderr)
}

/// debugfs prints a banner on stderr ("debugfs 1.47.0 ...") even on success, so fail only on
/// other non-empty lines.
fn ensure_debugfs_succeeded(stderr: &str) -> Result<()> {
    let error_line = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("debugfs "));
    if let Some(trimmed) = error_line {
        bail!("debugfs reported an error: {trimmed}");
    }
    Ok(())
}

fn ensure_safe_debugfs_arg(value: &str) -> Result<()> {
    ensure!(
        !value.contains(['"', '\n', '\r']),
        "path contains characters unsupported by debugfs script: {value}"
    );
    Ok(())
}

fn write_gpt(
    tools: &Tools,
    disk_path: &Path,
    part_sectors: u64,
    ids: &DiskIds,
    cancel: &AtomicBool,
) -> Result<()> {
    let script = format!(
        "label: gpt\n\
         label-id: {}\n\
         first-lba: {PARTITION_START_SECTOR}\n\
         start={PARTITION_START_SECTOR}, size={part_sectors}, type={LINUX_FS_TYPE_GUID}, uuid={}, name=\"{PARTITION_LABEL}\"\n",
        ids.disk_guid, ids.part_guid
    );
    let mut cmd = Command::new(&tools.sfdisk);
    cmd.args([OsStr::new("--quiet"), disk_path.as_os_str()]);
    run_tool_cancellable(cmd, Some(script.as_bytes()), "sfdisk", cancel)?;
    Ok(())
}

/// Run `cmd` in its own process group with stdout discarded, optional stdin bytes, and captured
/// stderr. Terminate the process group if `cancel` is set. Return stderr text on success.
fn run_tool_cancellable(
    mut cmd: Command,
    stdin_payload: Option<&[u8]>,
    label: &str,
    cancel: &AtomicBool,
) -> Result<String> {
    ensure_not_canceled(cancel, label)?;
    cmd.stdin(if stdin_payload.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .process_group(0);
    let mut child = cmd
        .spawn()
        .wrap_err_with(|| format!("failed to execute {label}"))?;

    if let (Some(bytes), Some(mut stdin)) = (stdin_payload, child.stdin.take()) {
        stdin.write_all(bytes)?;
    }
    subprocess::wait_cancellable(child, label, cancel)
}

fn copy_sparse_into(
    source_path: &Path,
    dest_path: &Path,
    dest_offset: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut src = File::open(source_path)?;
    let mut dst = OpenOptions::new().write(true).open(dest_path)?;
    let len = src.metadata()?.len();
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];

    while offset < len {
        ensure_not_canceled(cancel, "sparse disk copy")?;
        let data_start = match rustix::fs::seek(&src, RustixSeekFrom::Data(offset)) {
            Ok(pos) => pos,
            Err(rustix::io::Errno::NXIO) => break,
            Err(_) => {
                // Fallback when filesystem does not support SEEK_DATA: scan sequentially.
                return copy_skipping_zeroes(&mut src, &mut dst, dest_offset, len, cancel);
            }
        };
        if data_start >= len {
            break;
        }
        let hole_start = match rustix::fs::seek(&src, RustixSeekFrom::Hole(data_start)) {
            Ok(pos) => pos.min(len),
            Err(_) => len,
        };
        src.seek(SeekFrom::Start(data_start))?;
        dst.seek(SeekFrom::Start(dest_offset + data_start))?;
        copy_exact(
            &mut src,
            &mut dst,
            hole_start - data_start,
            &mut buffer,
            cancel,
        )?;
        offset = hole_start;
    }
    dst.flush()?;
    Ok(())
}

/// Copy exactly `count` bytes from the current position of `src` to the current position of `dst`.
fn copy_exact(
    src: &mut File,
    dst: &mut File,
    count: u64,
    buffer: &mut [u8],
    cancel: &AtomicBool,
) -> Result<()> {
    let mut remaining = count;
    while remaining > 0 {
        ensure_not_canceled(cancel, "sparse disk copy")?;
        let chunk = remaining.min(buffer.len() as u64) as usize;
        src.read_exact(&mut buffer[..chunk])?;
        dst.write_all(&buffer[..chunk])?;
        remaining -= chunk as u64;
    }
    Ok(())
}
fn copy_skipping_zeroes(
    src: &mut File,
    dst: &mut File,
    dest_offset: u64,
    len: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    src.seek(SeekFrom::Start(0))?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut pos = 0_u64;
    while pos < len {
        ensure_not_canceled(cancel, "sparse disk copy")?;
        let chunk = (len - pos).min(buffer.len() as u64) as usize;
        src.read_exact(&mut buffer[..chunk])?;
        if buffer[..chunk].iter().any(|byte| *byte != 0) {
            dst.seek(SeekFrom::Start(dest_offset + pos))?;
            dst.write_all(&buffer[..chunk])?;
        }
        pos += chunk as u64;
    }
    dst.flush()?;
    Ok(())
}
