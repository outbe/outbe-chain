//! Isolated GPT + ext4 data disk container format expected by dstack guests.
//!
//! Layout matches `dstack-prepare.sh` (`sgdisk -n 1:1MiB:0 -c 1:dstack-data -t 1:8300`)
//! and `dstack-util/src/system_setup.rs` (`dstack.storage_encrypted=false`,
//! `dstack.storage_fs=ext4`), where an existing ext4 partition labelled `dstack-data`
//! is checked with `e2fsck -f -p`, resized with `resize2fs`, and mounted at
//! `/dstack/persistent` without reformatting.

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use eyre::{bail, ensure, Context, Result};
use rustix::fs::SeekFrom as RustixSeekFrom;

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

/// Write a deterministic sparse raw GPT + ext4 disk image to `output`.
pub(crate) fn write_disk_image(
    tools: &Tools,
    output: &Path,
    disk_bytes: u64,
    ids: &DiskIds,
    entries: &[FsEntry],
) -> Result<()> {
    let (part_sectors, part_bytes) = partition_geometry(disk_bytes)?;
    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let work = tempfile::Builder::new()
        .prefix(".outbe-vm-disk-")
        .tempdir_in(parent)?;

    // 1. Build a deterministic sparse ext4 filesystem in a standalone partition file.
    let part_path = work.path().join("part.ext4");
    create_sparse_file(&part_path, part_bytes)?;
    format_ext4(tools, &part_path, ids)?;
    populate_ext4(tools, &part_path, work.path(), entries)?;

    // 2. Create the sparse raw disk image and write its deterministic GPT table.
    let disk_path = work.path().join("disk.img");
    create_sparse_file(&disk_path, disk_bytes)?;
    write_gpt(tools, &disk_path, part_sectors, ids)?;

    // 3. Sparse-copy the populated ext4 partition into the disk image at 1 MiB.
    copy_sparse_into(&part_path, &disk_path, PARTITION_OFFSET_BYTES)?;

    // 4. Atomically publish the finished image (refusing to clobber an existing file).
    ensure!(
        !output.exists(),
        "output disk image already exists: {}",
        output.display()
    );
    fs::rename(&disk_path, output)
        .wrap_err_with(|| format!("failed to publish disk image to {}", output.display()))?;
    Ok(())
}

fn find_in_path(tool: &str, package_hint: &str) -> Result<PathBuf> {
    let path_var = env::var_os("PATH").unwrap_or_default();
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(tool);
        if let Ok(meta) = fs::metadata(&candidate) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Ok(candidate);
            }
        }
    }
    bail!("required tool '{tool}' not found in PATH (install {package_hint})");
}

fn create_sparse_file(path: &Path, size: u64) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .wrap_err_with(|| format!("failed to create {}", path.display()))?;
    file.set_len(size)
        .wrap_err_with(|| format!("failed to size {} to {size} bytes", path.display()))?;
    Ok(())
}

fn format_ext4(tools: &Tools, part_path: &Path, ids: &DiskIds) -> Result<()> {
    // Disable orphan_file for compatibility with older guest e2fsck builds.
    let extended = format!(
        "root_owner=0:0,hash_seed={},lazy_itable_init=0,lazy_journal_init=0",
        ids.hash_seed
    );
    let output = Command::new(&tools.mke2fs)
        .env("E2FSPROGS_FAKE_TIME", FIXED_TIMESTAMP)
        .args([
            "-q",
            "-t",
            "ext4",
            "-b",
            &EXT4_BLOCK_SIZE.to_string(),
            "-O",
            "^orphan_file",
            "-U",
            &ids.fs_uuid,
            "-E",
            &extended,
        ])
        .arg(part_path)
        .output()
        .wrap_err("failed to execute mke2fs")?;
    ensure!(
        output.status.success(),
        "mke2fs failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn populate_ext4(
    tools: &Tools,
    part_path: &Path,
    work_dir: &Path,
    entries: &[FsEntry],
) -> Result<()> {
    let mut script = String::new();
    for entry in entries {
        match entry {
            FsEntry::Dir { target, mode } => {
                ensure_safe_debugfs_arg(target)?;
                let mode_octal = 0o040000 | (mode & 0o7777);
                script.push_str(&format!(
                    "mkdir \"{target}\"\n\
                     set_inode_field \"{target}\" uid 0\n\
                     set_inode_field \"{target}\" gid 0\n\
                     set_inode_field \"{target}\" mode 0{mode_octal:o}\n"
                ));
            }
            FsEntry::File {
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
                script.push_str(&format!(
                    "write \"{source_str}\" \"{target}\"\n\
                     set_inode_field \"{target}\" uid 0\n\
                     set_inode_field \"{target}\" gid 0\n\
                     set_inode_field \"{target}\" mode 0{mode_octal:o}\n"
                ));
            }
        }
    }

    let script_path = work_dir.join("populate.debugfs");
    fs::write(&script_path, &script)?;

    let output = Command::new(&tools.debugfs)
        .env("E2FSPROGS_FAKE_TIME", FIXED_TIMESTAMP)
        .args(["-w", "-f"])
        .arg(&script_path)
        .arg(part_path)
        .output()
        .wrap_err("failed to execute debugfs")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(output.status.success(), "debugfs failed: {}", stderr.trim());
    // debugfs prints banner on stderr ("debugfs 1.47.0 ...") even on success; fail on real errors.
    for line in stderr.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("debugfs ") {
            continue;
        }
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

fn write_gpt(tools: &Tools, disk_path: &Path, part_sectors: u64, ids: &DiskIds) -> Result<()> {
    let script = format!(
        "label: gpt\n\
         label-id: {}\n\
         first-lba: {PARTITION_START_SECTOR}\n\
         start={PARTITION_START_SECTOR}, size={part_sectors}, type={LINUX_FS_TYPE_GUID}, uuid={}, name=\"{PARTITION_LABEL}\"\n",
        ids.disk_guid, ids.part_guid
    );
    let mut child = Command::new(&tools.sfdisk)
        .arg("--quiet")
        .arg(disk_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .wrap_err("failed to execute sfdisk")?;
    child
        .stdin
        .as_mut()
        .expect("stdin is piped")
        .write_all(script.as_bytes())?;
    let output = child.wait_with_output()?;
    ensure!(
        output.status.success(),
        "sfdisk failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

fn copy_sparse_into(source_path: &Path, dest_path: &Path, dest_offset: u64) -> Result<()> {
    let mut src = File::open(source_path)?;
    let mut dst = OpenOptions::new().write(true).open(dest_path)?;
    let len = src.metadata()?.len();
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];

    while offset < len {
        let data_start = match rustix::fs::seek(&src, RustixSeekFrom::Data(offset)) {
            Ok(pos) => pos,
            Err(rustix::io::Errno::NXIO) => break,
            Err(_) => {
                // Fallback when filesystem does not support SEEK_DATA: scan sequentially.
                return copy_skipping_zeroes(&mut src, &mut dst, dest_offset, len);
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
        let mut remaining = hole_start - data_start;
        while remaining > 0 {
            let chunk = remaining.min(buffer.len() as u64) as usize;
            src.read_exact(&mut buffer[..chunk])?;
            dst.write_all(&buffer[..chunk])?;
            remaining -= chunk as u64;
        }
        offset = hole_start;
    }
    dst.flush()?;
    Ok(())
}

fn copy_skipping_zeroes(src: &mut File, dst: &mut File, dest_offset: u64, len: u64) -> Result<()> {
    src.seek(SeekFrom::Start(0))?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut pos = 0_u64;
    while pos < len {
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
