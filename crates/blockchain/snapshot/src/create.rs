//! Two-pass copy of stopped native files into one signed archive.

use std::{
    io::{self, Read},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::{
    archive::{append_member, payload_path, write_archive},
    fs::{PendingArchive, SourceRoot},
    layout::{validate_layout, ProtectedPaths},
    manifest::{EntryKind, SnapshotManifestV1},
    provenance::SignatureEnvelope,
};

/// `roots` match the native domains in order. Inventory paths come from local native enumeration.
pub fn create_snapshot(
    output: &Path,
    mut manifest: SnapshotManifestV1,
    roots: &[PathBuf],
    sign: impl FnOnce(&[u8]) -> io::Result<SignatureEnvelope>,
    check_inventory: impl FnOnce() -> io::Result<()>,
) -> io::Result<SnapshotManifestV1> {
    if manifest.domains.len() != roots.len() {
        return Err(io::Error::other("native domain/root count differs"));
    }
    validate_layout(
        &[],
        &ProtectedPaths(roots.to_vec()),
        &[output.to_path_buf()],
    )?;
    if output.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "snapshot output already exists",
        ));
    }
    let mut opened_roots = Vec::new();
    let mut identities = Vec::new();
    manifest.file_count = 0;
    manifest.total_bytes = 0;
    for (domain, path) in manifest.domains.iter_mut().zip(roots) {
        if domain.entries.is_empty() {
            opened_roots.push(None);
            identities.push(Vec::new());
            continue;
        }
        let root = SourceRoot::open(path)?;
        domain.mode = root.open_entry(Path::new(""))?.identity.mode & 0o7777;
        let mut observed = Vec::new();
        for member in &mut domain.entries {
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
        opened_roots.push(Some(root));
        identities.push(observed);
    }
    manifest.validate().map_err(io::Error::other)?;
    let raw = serde_json::to_vec(&manifest)?;
    let signature = sign(&raw)?;
    signature.verify(&raw, None)?;
    let mut pending = PendingArchive::new(output)?;
    write_archive(&mut pending.file, &raw, &signature, |archive| {
        for ((domain, root), observed) in
            manifest.domains.iter().zip(&opened_roots).zip(&identities)
        {
            append_member(
                archive,
                &payload_path(domain, None),
                0,
                domain.mode,
                true,
                io::empty(),
            )?;
            let Some(root) = root else {
                continue;
            };
            for (member, identity) in domain.entries.iter().zip(observed) {
                let mut entry = root.reopen(Path::new(&member.path), identity)?;
                if member.kind == EntryKind::Directory {
                    if !member.path.is_empty() {
                        append_member(
                            archive,
                            &payload_path(domain, Some(member)),
                            0,
                            member.mode,
                            true,
                            io::empty(),
                        )?;
                    }
                } else {
                    let mut reader = DigestReader::new(&mut entry.file);
                    append_member(
                        archive,
                        &payload_path(domain, Some(member)),
                        member.size,
                        member.mode,
                        false,
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
            }
        }
        Ok(())
    })?;
    // Check path bindings and directory metadata after streaming, not just open descriptors.
    for ((domain, root), observed) in manifest.domains.iter().zip(&opened_roots).zip(&identities) {
        if let Some(root) = root {
            root.verify_unchanged()?;
            for (member, identity) in domain.entries.iter().zip(observed) {
                root.reopen(Path::new(&member.path), identity)?;
            }
        }
    }
    check_inventory()?;
    pending.publish()?;
    Ok(manifest)
}

struct DigestReader<R> {
    inner: R,
    digest: Sha256,
}
impl<R> DigestReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            digest: Sha256::new(),
        }
    }
    fn finish(self) -> String {
        hex::encode(self.digest.finalize())
    }
}
impl<R: Read> Read for DigestReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buffer)?;
        self.digest.update(&buffer[..n]);
        Ok(n)
    }
}
