//! First-pass inventory with identities retained for the subsequent copy and binding checks.

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::{
    archive::digest::DigestReader,
    fs::{FileIdentity, SourceEntry, SourceRoot},
    manifest::{EntryKind, FileEntry, SnapshotManifestV1},
};

pub(super) struct DomainSource {
    pub(super) root: Option<SourceRoot>,
    pub(super) identities: Vec<FileIdentity>,
}

pub(super) fn capture_inventory(
    manifest: &mut SnapshotManifestV1,
    roots: &[PathBuf],
) -> io::Result<Vec<DomainSource>> {
    let mut sources = Vec::new();
    manifest.file_count = 0;
    manifest.total_bytes = 0;
    for (domain, path) in manifest.domains.iter_mut().zip(roots) {
        if domain.entries.is_empty() {
            sources.push(DomainSource {
                root: None,
                identities: Vec::new(),
            });
            continue;
        }
        let root = SourceRoot::open(path)?;
        domain.mode = root.open_entry(Path::new(""))?.identity.mode & 0o7777;
        let mut observed = Vec::new();
        for member in &mut domain.entries {
            let entry = observe_member(&root, member)?;
            if member.kind == EntryKind::File {
                manifest.file_count = manifest
                    .file_count
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("file count overflow"))?;
                manifest.total_bytes = manifest
                    .total_bytes
                    .checked_add(member.size)
                    .ok_or_else(|| io::Error::other("snapshot byte count overflow"))?;
            }
            entry.verify_unchanged()?;
            observed.push(entry.identity);
        }
        sources.push(DomainSource {
            root: Some(root),
            identities: observed,
        });
    }
    Ok(sources)
}

fn observe_member(root: &SourceRoot, member: &mut FileEntry) -> io::Result<SourceEntry> {
    let mut entry = root.open_entry(Path::new(&member.path))?;
    member.mode = entry.identity.mode & 0o7777;
    if entry.identity.is_directory {
        member.kind = EntryKind::Directory;
        member.size = 0;
        member.sha256 = None;
    } else {
        member.kind = EntryKind::File;
        member.size = entry.identity.size;
        let mut reader = DigestReader::new(&mut entry.file);
        io::copy(&mut reader, &mut io::sink())?;
        member.sha256 = Some(reader.finish());
    }
    Ok(entry)
}
