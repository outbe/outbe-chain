//! Second-pass signed payload streaming and final native path binding checks.

use std::{
    io::{self, Write},
    path::Path,
};

use super::inventory::DomainSource;
use crate::{
    archive::{append_member, digest::DigestReader, payload_path, ArchiveMember},
    fs::{FileIdentity, SourceRoot},
    manifest::{DomainInventory, EntryKind, FileEntry, SnapshotManifestV1},
};

pub(super) fn write_payload<W: Write>(
    archive: &mut tar::Builder<W>,
    manifest: &SnapshotManifestV1,
    sources: &[DomainSource],
) -> io::Result<()> {
    for (domain, source) in manifest.domains.iter().zip(sources) {
        append_member(
            archive,
            ArchiveMember::directory(&payload_path(domain, None), domain.mode),
            io::empty(),
        )?;
        let Some(root) = &source.root else {
            continue;
        };
        for (member, identity) in domain.entries.iter().zip(&source.identities) {
            write_member(archive, domain, member, root, identity)?;
        }
    }
    Ok(())
}

fn write_member<W: Write>(
    archive: &mut tar::Builder<W>,
    domain: &DomainInventory,
    member: &FileEntry,
    root: &SourceRoot,
    identity: &FileIdentity,
) -> io::Result<()> {
    let mut entry = root.reopen(Path::new(&member.path), identity)?;
    if member.kind == EntryKind::Directory {
        if !member.path.is_empty() {
            append_member(
                archive,
                ArchiveMember::directory(&payload_path(domain, Some(member)), member.mode),
                io::empty(),
            )?;
        }
    } else {
        let mut reader = DigestReader::new(&mut entry.file);
        append_member(
            archive,
            ArchiveMember::file(
                &payload_path(domain, Some(member)),
                member.size,
                member.mode,
            ),
            &mut reader,
        )?;
        if member.sha256.as_deref() != Some(reader.finish().as_str()) {
            return Err(io::Error::other(format!(
                "snapshot source checksum changed: {}",
                member.path
            )));
        }
    }
    entry.verify_unchanged()?;
    Ok(())
}

pub(super) fn verify_bindings(
    manifest: &SnapshotManifestV1,
    sources: &[DomainSource],
) -> io::Result<()> {
    // Check path bindings and directory metadata after streaming, not just open descriptors.
    for (domain, source) in manifest.domains.iter().zip(sources) {
        if let Some(root) = &source.root {
            root.verify_unchanged()?;
            for (member, identity) in domain.entries.iter().zip(&source.identities) {
                root.reopen(Path::new(&member.path), identity)?;
            }
        }
    }
    Ok(())
}
