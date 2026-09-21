//! Select native public files; identity and signing authority remain local.

use std::{
    fs,
    path::{Path, PathBuf},
};

use outbe_snapshot::{
    layout::{validate_layout, ProtectedPaths, ResolvedDomain},
    manifest::{DomainKind, NativeRoot},
};

use super::config::NativeLayout;

#[derive(Debug)]
pub(crate) struct NativeDomain {
    pub kind: DomainKind,
    pub native_root: NativeRoot,
    pub root: PathBuf,
    /// Paths relative to root, including native directories and pending records.
    pub members: Vec<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct NativeInventory {
    /// Empty selections describe absent optional populations without creating them.
    pub domains: Vec<NativeDomain>,
}

pub(crate) fn enumerate_native_files(layout: &NativeLayout) -> eyre::Result<NativeInventory> {
    use DomainKind::*;
    let fixed = [
        (ExecutionDb, NativeRoot::Chain, &layout.chain_root, "db"),
        (
            StaticFiles,
            NativeRoot::StaticFiles,
            &layout.static_files_root,
            "",
        ),
        (
            ExecutionRocksDb,
            NativeRoot::ExecutionRocksDb,
            &layout.execution_rocksdb_root,
            "",
        ),
        (
            Ce,
            NativeRoot::Chain,
            &layout.chain_root,
            "compressed_entities/smt",
        ),
        (
            OffchainProjection,
            NativeRoot::Offchain,
            &layout.offchain_root,
            "",
        ),
        (
            MarshalFinalizations,
            NativeRoot::Consensus,
            &layout.consensus_root,
            "",
        ),
        (
            MarshalBlocks,
            NativeRoot::Consensus,
            &layout.consensus_root,
            "",
        ),
        (
            MarshalMetadata,
            NativeRoot::Consensus,
            &layout.consensus_root,
            "outbe-marshal-application-metadata",
        ),
        (
            MarshalCache,
            NativeRoot::Consensus,
            &layout.consensus_root,
            "",
        ),
        (
            ParentCertificates,
            NativeRoot::Consensus,
            &layout.consensus_root,
            "finalized_parent_certs",
        ),
        (
            OcompRetention,
            NativeRoot::Consensus,
            &layout.consensus_root,
            "ocomp_retention",
        ),
        (
            ClosureCheckpoint,
            NativeRoot::Ocomp,
            &layout.ocomp_root,
            "exporter-v1/discovery/closure-checkpoint-v1",
        ),
        (Discovery, NativeRoot::Ocomp, &layout.ocomp_root, ""),
        (
            ProtocolBundles,
            NativeRoot::Ocomp,
            &layout.ocomp_root,
            "protocol-bundles-v1",
        ),
        (
            CasObjects,
            NativeRoot::Ocomp,
            &layout.ocomp_root,
            "cas-v1/objects",
        ),
        (
            InputReferences,
            NativeRoot::Ocomp,
            &layout.ocomp_root,
            "exporter-v1/input-refs",
        ),
        (
            ExportReceipts,
            NativeRoot::Ocomp,
            &layout.ocomp_root,
            "exporter-v1/receipts",
        ),
        (
            ExportBindings,
            NativeRoot::Ocomp,
            &layout.ocomp_root,
            "supervisor-v1/export-bindings",
        ),
        (JobPublicRecords, NativeRoot::Ocomp, &layout.ocomp_root, ""),
        (
            MaterializationReferences,
            NativeRoot::Ocomp,
            &layout.ocomp_root,
            "supervisor-v1/materialization-references",
        ),
        (
            LocalResults,
            NativeRoot::Ocomp,
            &layout.ocomp_root,
            "node-v1/local-results",
        ),
        (
            ExexCheckpoint,
            NativeRoot::Ocomp,
            &layout.ocomp_root,
            "node-v1/exex-checkpoint",
        ),
        (
            FatalEvidence,
            NativeRoot::Ocomp,
            &layout.ocomp_root,
            "node-v1/fatal-evidence",
        ),
    ];
    let mut domains = Vec::new();
    let mut selected_roots = Vec::new();
    for (kind, native_root, root, path) in fixed {
        let paths = match kind {
            MarshalFinalizations | MarshalBlocks => {
                let class = if kind == MarshalFinalizations {
                    "finalizations"
                } else {
                    "blocks"
                };
                [
                    "metadata",
                    "freezer-table",
                    "freezer-key",
                    "freezer-value",
                    "ordinal",
                ]
                .into_iter()
                .map(|suffix| PathBuf::from(format!("outbe-marshal-{class}-{suffix}")))
                .collect()
            }
            MarshalCache => children(root)?
                .into_iter()
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| {
                            name == "outbe-marshal-cache"
                                || name.starts_with("outbe-marshal-cache-")
                        })
                })
                .collect(),
            Discovery => children(&root.join("exporter-v1/discovery"))?
                .into_iter()
                .filter(|name| name != Path::new("closure-checkpoint-v1"))
                .map(|name| PathBuf::from("exporter-v1/discovery").join(name))
                .collect(),
            JobPublicRecords => children(&root.join("supervisor-v1/jobs"))?
                .into_iter()
                .flat_map(|job| {
                    [
                        "admissions",
                        "contributor-payout-v1.bin",
                        "contributor-payout-v1.bin.tmp",
                    ]
                    .into_iter()
                    .map(move |name| PathBuf::from("supervisor-v1/jobs").join(&job).join(name))
                })
                .collect(),
            _ => vec![PathBuf::from(path)],
        };
        let mut members = Vec::new();
        for path in paths {
            let source = root.join(&path);
            match fs::symlink_metadata(&source) {
                Ok(_) => {
                    selected_roots.push(ResolvedDomain {
                        name: format!("{kind:?}:{}", path.display()),
                        root: source,
                    });
                    walk(root, &path, &mut members)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    eyre::ensure!(
                        !matches!(
                            kind,
                            ExecutionDb | Ce | OffchainProjection | ClosureCheckpoint
                        ),
                        "missing required native domain: {}",
                        source.display()
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        domains.push(NativeDomain {
            kind,
            native_root,
            root: root.clone(),
            members,
        });
    }
    validate_layout(&selected_roots, &protected_native_paths(layout)?, &[])?;
    Ok(NativeInventory { domains })
}

fn children(root: &Path) -> eyre::Result<Vec<PathBuf>> {
    match fs::read_dir(root) {
        Ok(entries) => entries
            .map(|entry| Ok(PathBuf::from(entry?.file_name())))
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn walk(root: &Path, relative: &Path, members: &mut Vec<PathBuf>) -> eyre::Result<()> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    eyre::ensure!(
        metadata.is_file() || metadata.is_dir(),
        "unsupported native file: {}",
        path.display()
    );
    members.push(relative.to_path_buf());
    if metadata.is_dir() {
        for child in children(&path)? {
            walk(root, &relative.join(child), members)?;
        }
    }
    Ok(())
}

fn protected_native_paths(layout: &NativeLayout) -> eyre::Result<ProtectedPaths> {
    let mut protected = layout.protected.clone();
    for name in children(&layout.consensus_root)? {
        if name
            .to_str()
            .is_some_and(|name| name.starts_with("outbe-simplex-"))
        {
            protected.0.push(layout.consensus_root.join(name));
        }
    }
    for name in [
        "dkg_share.hex",
        "dkg_polynomial.hex",
        "dkg_output.hex",
        "dkg_pending_share.hex",
        "dkg_pending_polynomial.hex",
        "dkg_pending_output.hex",
        "dkg_pending_boundary.bin",
        "dkg_pending_boundary.bin.tmp",
        "dkg_dealer_retry.hex",
        "dkg_player_retry.hex",
    ] {
        protected.0.push(layout.consensus_root.join(name));
    }
    for name in [
        "ocomp-evm-key.hex",
        "ocomp-key-v1.hex",
        "supervisor-v1/sign-once",
        "supervisor-v1/vote-submissions",
        "supervisor-v1/materialization-submissions",
        "supervisor-v1/payout-submissions",
    ] {
        protected.0.push(layout.ocomp_root.join(name));
    }
    Ok(protected)
}
