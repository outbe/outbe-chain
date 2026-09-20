//! Portable tar container; reading never extracts or opens a declared host path.

use std::io::{self, Read, Write};

use sha2::{Digest, Sha256};

use crate::{
    manifest::{DomainInventory, EntryKind, FileEntry, SnapshotManifestV1},
    provenance::SignatureEnvelope,
};

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
        "manifest.json",
        raw_manifest.len() as u64,
        0o644,
        false,
        raw_manifest,
    )?;
    let raw_signature = serde_json::to_vec(signature).map_err(invalid)?;
    append_member(
        &mut archive,
        "signature.json",
        raw_signature.len() as u64,
        0o644,
        false,
        raw_signature.as_slice(),
    )?;
    payload(&mut archive)?;
    archive.finish()
}

pub(crate) fn append_member<W: Write>(
    archive: &mut tar::Builder<W>,
    path: &str,
    size: u64,
    mode: u32,
    directory: bool,
    reader: impl Read,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(size);
    header.set_mode(mode);
    header.set_entry_type(if directory {
        tar::EntryType::Directory
    } else {
        tar::EntryType::Regular
    });
    header.set_cksum();
    archive.append_data(&mut header, path, reader)
}

pub(crate) fn payload_path(domain: &DomainInventory, entry: Option<&FileEntry>) -> String {
    let base = format!("payload/{}", domain.id);
    match entry {
        Some(entry) if !entry.path.is_empty() => format!("{base}/{}", entry.path),
        _ => base,
    }
}

/// Verify the signed inventory and stream each declared payload without extracting it.
pub fn read_archive_index(
    reader: impl Read,
    expected_key: Option<&[u8; 33]>,
) -> io::Result<ArchiveIndex> {
    let mut archive = tar::Archive::new(reader);
    let mut entries = archive.entries()?;
    let mut metadata = |name: &str, limit: u64| -> io::Result<Vec<u8>> {
        let mut entry = entries
            .next()
            .ok_or_else(|| invalid(format!("missing {name}")))??;
        if entry.path()?.as_ref() != std::path::Path::new(name)
            || !entry.header().entry_type().is_file()
            || entry.size() > limit
        {
            return Err(invalid(format!("invalid {name} archive member")));
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let raw_manifest = metadata("manifest.json", 256 * 1024 * 1024)?;
    let signature = SignatureEnvelope::from_bytes(&metadata("signature.json", 64 * 1024)?)?;
    signature.verify(&raw_manifest, expected_key)?;
    let manifest = SnapshotManifestV1::from_bytes(&raw_manifest).map_err(invalid)?;
    for domain in &manifest.domains {
        let wrapper = entries
            .next()
            .ok_or_else(|| invalid("missing payload domain"))??;
        if wrapper.path()?.as_ref() != std::path::Path::new(&payload_path(domain, None))
            || !wrapper.header().entry_type().is_dir()
            || wrapper.size() != 0
            || wrapper.header().mode()? != domain.mode
        {
            return Err(invalid("payload domain differs from manifest"));
        }
        for member in &domain.entries {
            if member.path.is_empty() && member.kind == EntryKind::Directory {
                if member.mode != domain.mode {
                    return Err(invalid("payload root mode differs from signed entry"));
                }
                continue;
            }
            let mut entry = entries
                .next()
                .ok_or_else(|| invalid("missing declared payload"))??;
            let kind_matches = match member.kind {
                EntryKind::File => entry.header().entry_type().is_file(),
                EntryKind::Directory => entry.header().entry_type().is_dir(),
            };
            if entry.path()?.as_ref() != std::path::Path::new(&payload_path(domain, Some(member)))
                || !kind_matches
                || entry.size() != member.size
                || entry.header().mode()? != member.mode
            {
                return Err(invalid("payload member differs from manifest"));
            }
            if member.kind == EntryKind::File {
                let mut digest = Sha256::new();
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    let n = entry.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    digest.update(&buffer[..n]);
                }
                if member.sha256.as_deref() != Some(hex::encode(digest.finalize()).as_str()) {
                    return Err(invalid(format!(
                        "payload checksum mismatch: {}",
                        member.path
                    )));
                }
            }
        }
    }
    if entries.next().transpose()?.is_some() {
        return Err(invalid("undeclared archive payload"));
    }
    let mut trailing = archive.into_inner();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = trailing.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if buffer[..n].iter().any(|byte| *byte != 0) {
            return Err(invalid("nonzero data after archive terminator"));
        }
    }
    Ok(ArchiveIndex {
        raw_manifest,
        signature,
        manifest,
    })
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
