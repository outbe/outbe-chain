//! Anchored access to stopped native files and publication of one archive.

use std::{
    ffi::OsString,
    fs::{File, Metadata},
    io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use rustix::fs::{
    openat, openat2, renameat_with, unlinkat, AtFlags, Mode, OFlags, RenameFlags, ResolveFlags, CWD,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    dev: u64,
    ino: u64,
    links: u64,
    pub size: u64,
    pub mode: u32,
    pub is_directory: bool,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileIdentity {
    fn read(metadata: &Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            links: metadata.nlink(),
            size: metadata.len(),
            mode: metadata.mode(),
            is_directory: metadata.is_dir(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

pub struct SourceRoot {
    directory: File,
    path: PathBuf,
    identity: FileIdentity,
}

impl SourceRoot {
    pub fn open(path: &Path) -> io::Result<Self> {
        let fd = openat2(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS,
        )?;
        let directory = File::from(fd);
        let identity = FileIdentity::read(&directory.metadata()?);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        Ok(Self {
            directory,
            path,
            identity,
        })
    }

    pub fn verify_unchanged(&self) -> io::Result<()> {
        if Self::open(&self.path)?.identity != self.identity {
            return Err(io::Error::other(format!(
                "snapshot source root changed: {}",
                self.path.display()
            )));
        }
        Ok(())
    }

    /// The member comes from native enumeration, never from a received manifest.
    pub fn open_entry(&self, member: &Path) -> io::Result<SourceEntry> {
        let member = if member.as_os_str().is_empty() {
            Path::new(".")
        } else {
            member
        };
        let fd = openat2(
            &self.directory,
            member,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS,
        )?;
        let file = File::from(fd);
        let metadata = file.metadata()?;
        if !(metadata.is_file() || metadata.is_dir())
            || (metadata.is_file() && metadata.nlink() != 1)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot source is not a regular single-link file or directory",
            ));
        }
        Ok(SourceEntry {
            file,
            identity: FileIdentity::read(&metadata),
        })
    }

    pub fn reopen(&self, member: &Path, expected: &FileIdentity) -> io::Result<SourceEntry> {
        let entry = self.open_entry(member)?;
        if &entry.identity != expected {
            return Err(io::Error::other(format!(
                "snapshot source changed: {}",
                member.display()
            )));
        }
        Ok(entry)
    }
}

pub struct SourceEntry {
    pub file: File,
    pub identity: FileIdentity,
}

impl SourceEntry {
    pub fn verify_unchanged(&self) -> io::Result<()> {
        if FileIdentity::read(&self.file.metadata()?) != self.identity {
            return Err(io::Error::other("snapshot source changed while reading"));
        }
        Ok(())
    }
}

static NEXT_PENDING: AtomicU64 = AtomicU64::new(0);

pub struct PendingArchive {
    pub file: File,
    parent: File,
    pending: OsString,
    target: OsString,
    output: PathBuf,
    published: bool,
}

impl PendingArchive {
    pub fn new(output: &Path) -> io::Result<Self> {
        let target = output
            .file_name()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "archive output needs a filename",
                )
            })?
            .to_os_string();
        let parent_path = output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let parent = SourceRoot::open(parent_path)?.directory;
        let pending = loop {
            let candidate: OsString = format!(
                ".outbe-snapshot-{}-{}.pending",
                std::process::id(),
                NEXT_PENDING.fetch_add(1, Ordering::Relaxed)
            )
            .into();
            if candidate != target {
                break candidate;
            }
        };
        let fd = openat(
            &parent,
            &pending,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?;
        Ok(Self {
            file: File::from(fd),
            parent,
            pending,
            target,
            output: output.to_path_buf(),
            published: false,
        })
    }

    pub fn publish(mut self) -> io::Result<()> {
        self.file.sync_all()?;
        if !self.owns_pending()? {
            return Err(io::Error::other("pending snapshot file was replaced"));
        }
        renameat_with(
            &self.parent,
            &self.pending,
            &self.parent,
            &self.target,
            RenameFlags::NOREPLACE,
        )?;
        self.published = true;
        self.parent.sync_all().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "snapshot published at {}, but directory durability is uncertain: {error}",
                    self.output.display()
                ),
            )
        })
    }

    fn owns_pending(&self) -> io::Result<bool> {
        let fd = openat(
            &self.parent,
            &self.pending,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )?;
        let named = File::from(fd).metadata()?;
        let opened = self.file.metadata()?;
        Ok(named.dev() == opened.dev() && named.ino() == opened.ino())
    }
}

impl Drop for PendingArchive {
    fn drop(&mut self) {
        if !self.published && self.owns_pending().unwrap_or(false) {
            let _ = unlinkat(&self.parent, &self.pending, AtFlags::empty());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn output_may_use_the_pending_naming_pattern() {
        let temp = tempfile::tempdir().unwrap();
        let next = super::NEXT_PENDING.load(std::sync::atomic::Ordering::Relaxed);
        let output = temp.path().join(format!(
            ".outbe-snapshot-{}-{next}.pending",
            std::process::id()
        ));
        let pending = super::PendingArchive::new(&output).unwrap();
        assert!(!output.exists());
        pending.publish().unwrap();
        assert!(output.exists());
    }
}
