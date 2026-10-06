//! Portable tar container. The reader never extracts or opens a declared host path.

use std::io::{self, Read, Write};

use crate::{
    manifest::{DomainInventory, EntryKind, FileEntry, SnapshotManifestV1},
    provenance::SignatureEnvelope,
};

pub(crate) mod digest;
mod reader;

pub use reader::read_archive_index;

pub struct ArchiveIndex {
    pub raw_manifest: Vec<u8>,
    pub signature: SignatureEnvelope,
    pub manifest: SnapshotManifestV1,
}

pub fn write_archive<W: Write>(
    writer: W,
    raw_manifest: &[u8],
    signature: &SignatureEnvelope,
    payload: impl FnOnce(&mut tar::Builder<W>) -> io::Result<()>,
) -> io::Result<()> {
    signature.verify(raw_manifest, None)?;
    let mut archive = tar::Builder::new(writer);
    append_member(
        &mut archive,
        ArchiveMember::file("manifest.json", raw_manifest.len() as u64, 0o644),
        raw_manifest,
    )?;
    let raw_signature = serde_json::to_vec(signature).map_err(invalid)?;
    append_member(
        &mut archive,
        ArchiveMember::file("signature.json", raw_signature.len() as u64, 0o644),
        raw_signature.as_slice(),
    )?;
    payload(&mut archive)?;
    archive.finish()
}

pub(crate) struct ArchiveMember<'a> {
    path: &'a str,
    size: u64,
    mode: u32,
    kind: EntryKind,
}

impl<'a> ArchiveMember<'a> {
    pub(crate) fn file(path: &'a str, size: u64, mode: u32) -> Self {
        Self {
            path,
            size,
            mode,
            kind: EntryKind::File,
        }
    }

    pub(crate) fn directory(path: &'a str, mode: u32) -> Self {
        Self {
            path,
            size: 0,
            mode,
            kind: EntryKind::Directory,
        }
    }
}

pub(crate) fn append_member<W: Write>(
    archive: &mut tar::Builder<W>,
    member: ArchiveMember<'_>,
    reader: impl Read,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(member.size);
    header.set_mode(member.mode);
    header.set_entry_type(match member.kind {
        EntryKind::Directory => tar::EntryType::Directory,
        EntryKind::File => tar::EntryType::Regular,
    });
    header.set_cksum();
    archive.append_data(&mut header, member.path, reader)
}

pub(crate) fn payload_path(domain: &DomainInventory, entry: Option<&FileEntry>) -> String {
    let base = format!("payload/{}", domain.id);
    match entry {
        Some(entry) if !entry.path.is_empty() => format!("{base}/{}", entry.path),
        _ => base,
    }
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
