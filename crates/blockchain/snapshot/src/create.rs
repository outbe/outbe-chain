//! Two-pass copy of stopped native files into one signed archive.

use std::{
    io,
    path::{Path, PathBuf},
};

use crate::{
    archive::write_archive,
    fs::PendingArchive,
    layout::{validate_layout, ProtectedPaths},
    manifest::SnapshotManifestV1,
    provenance::SignatureEnvelope,
};

mod inventory;
mod payload;

use inventory::capture_inventory;
use payload::{verify_bindings, write_payload};

/// `roots` match the native domains in order. Inventory paths come from local native enumeration.
pub fn create_snapshot(
    output: &Path,
    mut manifest: SnapshotManifestV1,
    roots: &[PathBuf],
    sign: impl FnOnce(&[u8]) -> io::Result<SignatureEnvelope>,
    check_inventory: impl FnOnce() -> io::Result<()>,
) -> io::Result<SnapshotManifestV1> {
    validate_output(output, &manifest, roots)?;
    let sources = capture_inventory(&mut manifest, roots)?;
    manifest.validate().map_err(io::Error::other)?;
    let raw = serde_json::to_vec(&manifest)?;
    let signature = sign(&raw)?;
    signature.verify(&raw, None)?;
    let mut pending = PendingArchive::new(output)?;
    write_archive(&mut pending.file, &raw, &signature, |archive| {
        write_payload(archive, &manifest, &sources)
    })?;
    verify_bindings(&manifest, &sources)?;
    check_inventory()?;
    pending.publish()?;
    Ok(manifest)
}

fn validate_output(
    output: &Path,
    manifest: &SnapshotManifestV1,
    roots: &[PathBuf],
) -> io::Result<()> {
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
    Ok(())
}
