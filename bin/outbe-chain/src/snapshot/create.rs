//! Compose ordinary native readers and the standalone signed archive writer.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use eyre::WrapErr;
use outbe_evm::OutbeEvmSigner;
use outbe_snapshot::{
    layout::{validate_layout, ProtectedPaths},
    manifest::{DomainInventory, EntryKind, FileEntry, SnapshotManifestV1, MANIFEST_VERSION},
    provenance::{signing_digest, SignatureEnvelope},
};

use super::{
    config::{parse_node_inputs, resolve_layout},
    inventory::{enumerate_native_files, NativeInventory},
    native::inspect_stopped_stores,
};

pub(crate) fn create(
    output: &Path,
    signing_key: &Path,
    creator: Option<String>,
    source: Option<String>,
    node_args: Vec<OsString>,
) -> eyre::Result<(SnapshotManifestV1, [u8; 33])> {
    let signer =
        OutbeEvmSigner::from_file(signing_key).wrap_err("load existing snapshot signing key")?;
    let inputs = parse_node_inputs(node_args)?;
    let mut layout = resolve_layout(&inputs)?;
    layout.protected.0.push(signing_key.to_path_buf());
    let mut protected = layout.protected.clone();
    protected.0.extend([
        layout.chain_root.clone(),
        layout.consensus_root.clone(),
        layout.ocomp_root.clone(),
        layout.offchain_root.clone(),
        layout.static_files_root.clone(),
        layout.execution_rocksdb_root.clone(),
    ]);
    validate_layout(&[], &ProtectedPaths(protected.0), &[output.to_path_buf()])?;
    eyre::ensure!(
        !output.try_exists()?,
        "snapshot output already exists: {}",
        output.display()
    );
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let scratch = tempfile::Builder::new()
        .prefix(".outbe-snapshot-inspection-")
        .tempdir_in(parent)?;
    let progress = inspect_stopped_stores(&layout, scratch.path())?;
    scratch
        .close()
        .wrap_err("remove offline inspection scratch")?;
    let inventory = enumerate_native_files(&layout)?;
    let mut domains = Vec::new();
    let mut roots = Vec::new();
    for domain in &inventory.domains {
        let kind = serde_json::to_value(domain.kind)?;
        let id = kind
            .as_str()
            .ok_or_else(|| eyre::eyre!("invalid native domain label"))?
            .to_owned();
        let entries = domain
            .members
            .iter()
            .map(|path| {
                Ok(FileEntry {
                    path: path
                        .to_str()
                        .ok_or_else(|| {
                            eyre::eyre!("native member path is not UTF-8: {}", path.display())
                        })?
                        .to_owned(),
                    kind: EntryKind::File,
                    size: 0,
                    sha256: None,
                    mode: 0,
                })
            })
            .collect::<eyre::Result<Vec<_>>>()?;
        domains.push(DomainInventory {
            id,
            kind: domain.kind,
            native_root: domain.native_root,
            native_path: String::new(),
            mode: 0o700,
            entries,
        });
        roots.push(domain.root.clone());
    }
    let manifest = SnapshotManifestV1 {
        version: MANIFEST_VERSION,
        chain_id: layout.chain.chain().id(),
        genesis_hash: hex::encode(layout.chain.genesis_hash()),
        created_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        creator,
        source,
        progress,
        domains,
        file_count: 0,
        total_bytes: 0,
    };
    let mut public_key = None;
    let manifest = outbe_snapshot::create::create_snapshot(
        output,
        manifest,
        &roots,
        |raw| {
            let digest = alloy_primitives::B256::from(signing_digest(raw));
            let signature = signer.sign_hash(&digest).map_err(io::Error::other)?;
            let recovered = outbe_primitives::tee_signatures::recover_signer(&digest, &signature)
                .map_err(|error| io::Error::other(format!("{error:?}")))?;
            if recovered != signer.address() {
                return Err(io::Error::other(
                    "snapshot signature does not match loaded key",
                ));
            }
            let envelope = SignatureEnvelope::from_signature(raw, signature)?;
            public_key = Some(envelope.verify(raw, None)?);
            Ok(envelope)
        },
        || {
            let after = enumerate_native_files(&layout)
                .map_err(|error| io::Error::other(format!("{error:#}")))?;
            if !same_members(&inventory, &after) {
                return Err(io::Error::other(
                    "native inventory changed during snapshot creation",
                ));
            }
            Ok(())
        },
    )?;
    Ok((
        manifest,
        public_key.ok_or_else(|| eyre::eyre!("snapshot signer was not invoked"))?,
    ))
}

fn same_members(before: &NativeInventory, after: &NativeInventory) -> bool {
    before.domains.len() == after.domains.len()
        && before.domains.iter().zip(&after.domains).all(|(a, b)| {
            a.kind == b.kind
                && a.native_root == b.native_root
                && a.root == b.root
                && a.members.iter().collect::<BTreeSet<_>>()
                    == b.members.iter().collect::<BTreeSet<_>>()
        })
}
