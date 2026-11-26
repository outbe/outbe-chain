//! Bounded, sanitized extraction of a decompressed tar stream into a staging directory.

use std::{
    cell::RefCell,
    fs::{self, OpenOptions},
    io::{self, Read},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

use eyre::{bail, ensure, Context, Result};

use super::ensure_not_canceled;

const MAX_FOUNDER_TAR_ENTRIES: usize = 2048;

/// Maximum total decompressed byte budget across the entire founder tar stream (headers,
/// padding, GNU/PAX extended headers, and payloads).
pub(super) const MAX_FOUNDER_TAR_BYTES: u64 = 512 * 1024 * 1024;

/// Maximum decompressed metadata bytes (headers + GNU longname/longlink + PAX extended headers)
/// allowed before a single tar entry is yielded, preventing pre-yield OOM allocations.
pub(super) const MAX_TAR_HEADER_METADATA_BYTES: u64 = 1024 * 1024;

struct TarBudget {
    total_read: u64,
    max_total: u64,
    entry_meta_read: u64,
    max_entry_meta: u64,
    counting_meta: bool,
}

impl TarBudget {
    fn new(max_total: u64, max_entry_meta: u64) -> Self {
        Self {
            total_read: 0,
            max_total,
            entry_meta_read: 0,
            max_entry_meta,
            counting_meta: true,
        }
    }

    /// Count header metadata again and reset the per-entry metadata counter.
    fn start_next_entry(&mut self) {
        self.counting_meta = true;
        self.entry_meta_read = 0;
    }
}

/// Reader wrapper placed *below* `tar::Archive` so every decompressed byte — including tar
/// headers, padding, GNU longname/longlink extensions, PAX extended headers, and directory or
/// file payloads — is counted against the total budget and per-entry metadata cap *before*
/// `tar` can allocate or parse it.
struct BoundedTarReader<'a, R: Read> {
    inner: R,
    budget: Rc<RefCell<TarBudget>>,
    cancel: &'a AtomicBool,
}

impl<R: Read> Read for BoundedTarReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "founder tar extraction canceled by signal",
            ));
        }
        if buf.is_empty() {
            return Ok(0);
        }

        let allowed_cap = {
            let b = self.budget.borrow();
            let total_rem = b.max_total.saturating_sub(b.total_read);
            let limit = if b.counting_meta {
                let meta_rem = b.max_entry_meta.saturating_sub(b.entry_meta_read);
                total_rem.min(meta_rem)
            } else {
                total_rem
            };
            // Allow reading up to 1 byte past the cap so EOF at exact cap succeeds while any
            // byte beyond the cap immediately triggers the budget error without overshooting.
            let max_chunk = limit.saturating_add(1).min(buf.len() as u64) as usize;
            max_chunk.max(1)
        };

        let n = self.inner.read(&mut buf[..allowed_cap])?;
        if n == 0 {
            return Ok(0);
        }

        let mut b = self.budget.borrow_mut();
        b.total_read = b.total_read.saturating_add(n as u64);
        if b.total_read > b.max_total {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "founder tar decompressed stream exceeds maximum {} bytes",
                    b.max_total
                ),
            ));
        }
        if b.counting_meta {
            b.entry_meta_read = b.entry_meta_read.saturating_add(n as u64);
            if b.entry_meta_read > b.max_entry_meta {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "founder tar entry header/metadata exceeds maximum {} bytes",
                        b.max_entry_meta
                    ),
                ));
            }
        }
        Ok(n)
    }
}

pub(super) fn extract_tar_stream<R: Read>(
    reader: R,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    extract_tar_stream_with_limits(
        reader,
        destination,
        MAX_FOUNDER_TAR_BYTES,
        MAX_TAR_HEADER_METADATA_BYTES,
        cancel,
    )
}

pub(super) fn extract_tar_stream_with_limits<R: Read>(
    reader: R,
    destination: &Path,
    max_total_bytes: u64,
    max_entry_meta_bytes: u64,
    cancel: &AtomicBool,
) -> Result<()> {
    let budget = Rc::new(RefCell::new(TarBudget::new(
        max_total_bytes,
        max_entry_meta_bytes,
    )));
    let bounded = BoundedTarReader {
        inner: reader,
        budget: Rc::clone(&budget),
        cancel,
    };
    let mut archive = tar::Archive::new(bounded);
    let mut entry_count: usize = 0;

    for entry in archive.entries().wrap_err("failed to read founder tar")? {
        let mut entry = entry.wrap_err("failed to read founder tar entry")?;
        ensure_not_canceled(cancel, "founder tar extraction")?;
        entry_count += 1;
        ensure!(
            entry_count <= MAX_FOUNDER_TAR_ENTRIES,
            "founder tar archive exceeds maximum {MAX_FOUNDER_TAR_ENTRIES} entries"
        );
        extract_entry(&mut entry, destination, max_total_bytes, &budget)?;
        budget.borrow_mut().start_next_entry();
    }
    Ok(())
}

/// Path and header fields of one tar entry, read before its payload.
struct EntryMeta {
    raw_path: PathBuf,
    target: PathBuf,
    mode: u32,
    declared_size: u64,
}

fn extract_entry<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    destination: &Path,
    max_total_bytes: u64,
    budget: &RefCell<TarBudget>,
) -> Result<()> {
    let entry_type = entry.header().entry_type();
    let raw_path = entry.path()?.into_owned();
    let rel = sanitize_tar_relative_path(&raw_path)?;
    let declared_size = entry.size();
    if rel.as_os_str().is_empty() {
        ensure!(
            declared_size == 0,
            "refusing non-zero payload size ({declared_size} bytes) on root entry in founder tar"
        );
        return Ok(());
    }
    let meta = EntryMeta {
        target: destination.join(&rel),
        mode: entry.header().mode()? & 0o7777,
        raw_path,
        declared_size,
    };

    if entry_type.is_dir() {
        return extract_dir_entry(&meta);
    }
    ensure!(
        entry_type.is_file(),
        "refusing non-regular entry ({entry_type:?}) in founder tar: {}",
        meta.raw_path.display()
    );
    extract_file_entry(entry, &meta, max_total_bytes, budget)
}

fn extract_dir_entry(meta: &EntryMeta) -> Result<()> {
    let EntryMeta {
        raw_path,
        target,
        mode,
        declared_size,
    } = meta;
    ensure!(
        *declared_size == 0,
        "refusing directory entry with non-zero payload size ({declared_size} bytes) in founder tar: {}",
        raw_path.display()
    );
    ensure!(
        mode & 0o6000 == 0,
        "refusing setuid/setgid directory in founder tar: {}",
        raw_path.display()
    );
    fs::create_dir_all(target)?;
    fs::set_permissions(target, fs::Permissions::from_mode(mode & 0o777))?;
    Ok(())
}

fn extract_file_entry<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    meta: &EntryMeta,
    max_total_bytes: u64,
    budget: &RefCell<TarBudget>,
) -> Result<()> {
    let EntryMeta {
        raw_path,
        target,
        mode,
        declared_size,
    } = meta;
    let (mode, declared_size) = (*mode, *declared_size);
    ensure!(
        mode & 0o111 == 0,
        "refusing executable file (mode {mode:04o}) in founder tar: {}",
        raw_path.display()
    );
    ensure!(
        mode & 0o6000 == 0,
        "refusing setuid/setgid file (mode {mode:04o}) in founder tar: {}",
        raw_path.display()
    );
    ensure!(
        declared_size <= max_total_bytes,
        "founder tar entry exceeds maximum {max_total_bytes} bytes: {}",
        raw_path.display()
    );

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let file_mode = mode & 0o777;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(file_mode)
        .open(target)
        .wrap_err_with(|| format!("failed to extract {}", raw_path.display()))?;

    // Switch off per-entry header metadata cap while streaming regular file payload; the
    // total stream budget remains enforced on every read.
    budget.borrow_mut().counting_meta = false;
    let copied = io::copy(&mut entry.by_ref().take(declared_size), &mut out)
        .wrap_err_with(|| format!("failed to extract {}", raw_path.display()))?;
    budget.borrow_mut().start_next_entry();

    ensure!(
        copied == declared_size,
        "truncated entry in founder tar: {}",
        raw_path.display()
    );
    out.sync_all()?;
    fs::set_permissions(target, fs::Permissions::from_mode(file_mode))?;
    Ok(())
}

fn sanitize_tar_relative_path(raw: &Path) -> Result<PathBuf> {
    let mut clean = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => clean.push(part),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("unsafe path in founder tar archive: {}", raw.display());
            }
        }
    }
    Ok(clean)
}
