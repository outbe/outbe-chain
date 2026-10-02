//! Authenticate metadata before streaming the declared payload and archive tail.

use std::{
    io::{self, Read},
    path::Path,
};

use super::{digest::stream_sha256, invalid, payload_path, ArchiveIndex};
use crate::{
    manifest::{DomainInventory, EntryKind, FileEntry, SnapshotManifestV1},
    provenance::SignatureEnvelope,
};

/// Verify the signed inventory and stream each declared payload without extracting it.
pub fn read_archive_index(
    reader: impl Read,
    expected_key: Option<&[u8; 33]>,
) -> io::Result<ArchiveIndex> {
    let mut archive = tar::Archive::new(reader);
    let mut entries = archive.entries()?;
    let index = read_signed_index(&mut entries, expected_key)?;
    verify_payload(&mut entries, &index.manifest)?;
    if entries.next().transpose()?.is_some() {
        return Err(invalid("undeclared archive payload"));
    }
    verify_tail(archive.into_inner())?;
    Ok(index)
}

fn read_signed_index<R: Read>(
    entries: &mut tar::Entries<'_, R>,
    expected_key: Option<&[u8; 33]>,
) -> io::Result<ArchiveIndex> {
    let raw_manifest = read_metadata(entries, "manifest.json", 256 * 1024 * 1024)?;
    let signature =
        SignatureEnvelope::from_bytes(&read_metadata(entries, "signature.json", 64 * 1024)?)?;
    signature.verify(&raw_manifest, expected_key)?;
    let manifest = SnapshotManifestV1::from_bytes(&raw_manifest).map_err(invalid)?;
    Ok(ArchiveIndex {
        raw_manifest,
        signature,
        manifest,
    })
}

fn read_metadata<R: Read>(
    entries: &mut tar::Entries<'_, R>,
    name: &str,
    limit: u64,
) -> io::Result<Vec<u8>> {
    let mut entry = entries
        .next()
        .ok_or_else(|| invalid(format!("missing {name}")))??;
    if entry.path()?.as_ref() != Path::new(name)
        || !entry.header().entry_type().is_file()
        || entry.size() > limit
    {
        return Err(invalid(format!("invalid {name} archive member")));
    }
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn verify_payload<R: Read>(
    entries: &mut tar::Entries<'_, R>,
    manifest: &SnapshotManifestV1,
) -> io::Result<()> {
    for domain in &manifest.domains {
        verify_domain(entries, domain)?;
        for member in &domain.entries {
            verify_member(entries, domain, member)?;
        }
    }
    Ok(())
}

fn verify_domain<R: Read>(
    entries: &mut tar::Entries<'_, R>,
    domain: &DomainInventory,
) -> io::Result<()> {
    let wrapper = entries
        .next()
        .ok_or_else(|| invalid("missing payload domain"))??;
    if wrapper.path()?.as_ref() != Path::new(&payload_path(domain, None))
        || !wrapper.header().entry_type().is_dir()
        || wrapper.size() != 0
        || wrapper.header().mode()? != domain.mode
    {
        return Err(invalid("payload domain differs from manifest"));
    }
    Ok(())
}

fn verify_member<R: Read>(
    entries: &mut tar::Entries<'_, R>,
    domain: &DomainInventory,
    member: &FileEntry,
) -> io::Result<()> {
    if member.path.is_empty() && member.kind == EntryKind::Directory {
        if member.mode != domain.mode {
            return Err(invalid("payload root mode differs from signed entry"));
        }
        return Ok(());
    }
    let mut entry = entries
        .next()
        .ok_or_else(|| invalid("missing declared payload"))??;
    let kind_matches = match member.kind {
        EntryKind::File => entry.header().entry_type().is_file(),
        EntryKind::Directory => entry.header().entry_type().is_dir(),
    };
    if entry.path()?.as_ref() != Path::new(&payload_path(domain, Some(member)))
        || !kind_matches
        || entry.size() != member.size
        || entry.header().mode()? != member.mode
    {
        return Err(invalid("payload member differs from manifest"));
    }
    if member.kind == EntryKind::File
        && member.sha256.as_deref() != Some(stream_sha256(&mut entry)?.as_str())
    {
        return Err(invalid(format!(
            "payload checksum mismatch: {}",
            member.path
        )));
    }
    Ok(())
}

fn verify_tail(mut reader: impl Read) -> io::Result<()> {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if buffer[..n].iter().any(|byte| *byte != 0) {
            return Err(invalid("nonzero data after archive terminator"));
        }
    }
    Ok(())
}
