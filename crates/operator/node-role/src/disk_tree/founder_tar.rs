//! Founder tar archive input: compression detection (`zstd`, `gzip`, plain) for a file or a
//! stream, decompression, and root resolution of the extracted tree.

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
};

use eyre::{bail, ensure, Context, Result};

use super::{
    child::{terminate_process_group, wait_child_cancellable, CancellableChildStdout},
    ensure_not_canceled,
    tar_extract::{extract_tar_stream, MAX_FOUNDER_TAR_BYTES},
};

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Compression of a founder tar archive, detected from its leading magic bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArchiveFormat {
    Zstd,
    Gzip,
    Plain,
}

impl ArchiveFormat {
    fn detect(magic: &[u8]) -> Self {
        if magic.starts_with(&ZSTD_MAGIC) {
            Self::Zstd
        } else if magic.starts_with(&GZIP_MAGIC) {
            Self::Gzip
        } else {
            Self::Plain
        }
    }
}

/// Input side of a `gzip -dc` child.
#[derive(Clone, Copy)]
enum GzipInput<'a> {
    /// `gzip` reads the archive file directly.
    File(&'a Path),
    /// The caller writes the compressed stream into the child stdin.
    Pipe,
}

pub(super) fn extract_founder_tar(
    archive_path: &Path,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    ensure_not_canceled(cancel, "founder tar extraction")?;
    let mut file = File::open(archive_path)
        .wrap_err_with(|| format!("failed to open founder archive: {}", archive_path.display()))?;
    let mut magic = [0_u8; 4];
    let n = file.read(&mut magic)?;
    file.seek(SeekFrom::Start(0))?;

    match ArchiveFormat::detect(&magic[..n]) {
        ArchiveFormat::Zstd => {
            let decoder =
                zstd::Decoder::new(file).wrap_err("failed to open zstd founder tar archive")?;
            extract_tar_stream(decoder, destination, cancel)
        }
        ArchiveFormat::Gzip => {
            drop(file);
            extract_gzip_file(archive_path, destination, cancel)
        }
        ArchiveFormat::Plain => extract_tar_stream(file, destination, cancel),
    }
}

fn extract_gzip_file(archive_path: &Path, destination: &Path, cancel: &AtomicBool) -> Result<()> {
    let mut child = spawn_gzip(GzipInput::File(archive_path))?;
    let stdout = take_cancellable_stdout(&mut child, cancel)?;
    let extract_result = extract_tar_stream(stdout, destination, cancel);
    let source = format!("for {}", archive_path.display());
    finish_gzip(&mut child, extract_result, cancel, &source)
}

fn read_magic_prefix<R: Read>(reader: &mut R, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let mut prefix = Vec::with_capacity(4);
    let mut buf = [0_u8; 4];
    while prefix.len() < 4 {
        ensure_not_canceled(cancel, "founder tar extraction")?;
        let remaining = 4 - prefix.len();
        match reader.read(&mut buf[..remaining]) {
            Ok(0) => break,
            Ok(n) => prefix.extend_from_slice(&buf[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err).wrap_err("failed to read founder tar stream header"),
        }
    }
    Ok(prefix)
}

pub(super) fn extract_founder_tar_reader<R: Read + Send>(
    mut reader: R,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    ensure_not_canceled(cancel, "founder tar extraction")?;
    let magic = read_magic_prefix(&mut reader, cancel)?;
    ensure!(!magic.is_empty(), "founder tar stream is empty");
    let format = ArchiveFormat::detect(&magic);
    let chained = io::Cursor::new(magic).chain(reader);

    match format {
        ArchiveFormat::Zstd => {
            let decoder =
                zstd::Decoder::new(chained).wrap_err("failed to open zstd founder tar stream")?;
            extract_tar_stream(decoder, destination, cancel)
        }
        ArchiveFormat::Gzip => extract_gzip_stream(chained, destination, cancel),
        ArchiveFormat::Plain => extract_tar_stream(chained, destination, cancel),
    }
}

/// Decompress a gzip stream through a `gzip -dc` child. A scoped thread feeds the child stdin
/// while this thread extracts the child stdout.
fn extract_gzip_stream<R: Read + Send>(
    compressed: R,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut child = spawn_gzip(GzipInput::Pipe)?;
    let child_pid = child.id();
    let stdout = take_cancellable_stdout(&mut child, cancel)?;
    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| eyre::eyre!("gzip stdin unavailable"))?;
    let extract_result = thread::scope(|scope| {
        let feeder = scope.spawn(move || feed_gzip_stdin(compressed, child_stdin, cancel));
        let res = extract_tar_stream(stdout, destination, cancel);
        if res.is_err() {
            terminate_process_group(child_pid);
        }
        let feed_res = feeder
            .join()
            .map_err(|_| eyre::eyre!("gzip stdin feeder thread panicked"))?;
        res?;
        feed_res.wrap_err("failed to stream gzip founder tar to gzip -dc")?;
        Ok(())
    });
    finish_gzip(&mut child, extract_result, cancel, "on stdin stream")
}

/// Copy the compressed stream into the `gzip -dc` stdin. Stop on cancel, on end of input, or
/// when gzip closes its stdin. Refuse input above the founder tar byte budget.
fn feed_gzip_stdin<R: Read>(
    mut src: R,
    mut child_stdin: ChildStdin,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut buf = [0_u8; 16 * 1024];
    let mut total_fed: u64 = 0;
    while !cancel.load(Ordering::Relaxed) {
        let n = match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        total_fed = total_fed.saturating_add(n as u64);
        if total_fed > MAX_FOUNDER_TAR_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "compressed founder tar stream exceeds maximum {MAX_FOUNDER_TAR_BYTES} bytes"
                ),
            ));
        }
        match child_stdin.write_all(&buf[..n]) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::BrokenPipe => break,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Spawn `gzip -dc` in its own process group with a piped stdout.
fn spawn_gzip(input: GzipInput<'_>) -> Result<Child> {
    let mut cmd = Command::new("gzip");
    cmd.args(["-dc"]);
    let spawn_context = match input {
        GzipInput::File(archive_path) => {
            cmd.arg(archive_path).stdin(Stdio::null());
            "failed to spawn gzip -dc for founder tar.gz archive"
        }
        GzipInput::Pipe => {
            cmd.stdin(Stdio::piped());
            "failed to spawn gzip -dc for founder tar.gz stream"
        }
    };
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    cmd.spawn().wrap_err(spawn_context)
}

/// Take the gzip stdout as a cancellable non-blocking reader. Stop the child if that fails.
fn take_cancellable_stdout<'a>(
    child: &mut Child,
    cancel: &'a AtomicBool,
) -> Result<CancellableChildStdout<'a>> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| eyre::eyre!("gzip stdout unavailable"))?;
    CancellableChildStdout::new(stdout, child.id(), cancel).inspect_err(|_| abort_child(child))
}

/// Close the child stdin, stop the child process group, and reap the child.
fn abort_child(child: &mut Child) {
    drop(child.stdin.take());
    terminate_process_group(child.id());
    let _ = child.wait();
}

/// Reap `gzip -dc` after extraction. Stop the child first when extraction failed. `source` names
/// the input in the error text (for example `for <path>` or `on stdin stream`).
fn finish_gzip(
    child: &mut Child,
    extract_result: Result<()>,
    cancel: &AtomicBool,
    source: &str,
) -> Result<()> {
    if extract_result.is_err() {
        abort_child(child);
        return extract_result;
    }
    let status = wait_child_cancellable(child, cancel, "gzip -dc")?;
    ensure!(status.success(), "gzip -dc failed {source} ({status})");
    Ok(())
}

pub(super) fn resolve_extracted_founder_root(extract_root: &Path) -> Result<PathBuf> {
    if extract_root.join("genesis.json").is_file() {
        return Ok(extract_root.to_path_buf());
    }
    let entries: Vec<PathBuf> = fs::read_dir(extract_root)?
        .map(|entry| Ok(entry?.path()))
        .collect::<Result<_>>()?;
    if entries.len() == 1 && entries[0].is_dir() && entries[0].join("genesis.json").is_file() {
        return Ok(entries[0].clone());
    }
    bail!(
        "founder tar archive must contain genesis.json at root or inside a single top-level directory"
    );
}
