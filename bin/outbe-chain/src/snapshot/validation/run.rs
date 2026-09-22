//! Resolve operator-selected checks before opening any native store.

use std::collections::BTreeSet;

use super::report::CheckName;

pub(crate) struct CheckSelection {
    pub checks: BTreeSet<CheckName>,
}

impl CheckSelection {
    pub(crate) fn resolve(
        requested: &str,
        has_artifact_metadata: bool,
        has_expected_signer: bool,
    ) -> eyre::Result<Self> {
        use CheckName::*;
        let mut checks = BTreeSet::new();
        if requested == "all" {
            checks.extend([Headers, Evm, Ce, Bodies, Ocomp]);
            if has_artifact_metadata {
                checks.extend([Files, Provenance]);
            }
        } else {
            for name in requested.split(',') {
                checks.insert(match name {
                    "files" => Files,
                    "provenance" => Provenance,
                    "headers" => Headers,
                    "evm" => Evm,
                    "ce" => Ce,
                    "bodies" => Bodies,
                    "ocomp" => Ocomp,
                    _ => eyre::bail!("unknown snapshot check {name:?}"),
                });
            }
        }
        if has_expected_signer || checks.contains(&Files) {
            checks.insert(Provenance);
        }
        if checks.contains(&Bodies) {
            checks.insert(Ce);
        }
        if checks.contains(&Ocomp) {
            checks.insert(Evm);
        }
        if checks.contains(&Evm) || checks.contains(&Ce) {
            checks.insert(Headers);
        }
        Ok(Self { checks })
    }

    pub(crate) fn needs_projection(&self) -> bool {
        [CheckName::Files, CheckName::Bodies, CheckName::Ocomp]
            .iter()
            .any(|check| self.checks.contains(check))
    }
}

/// Optional signed artifact inputs. Native check selection remains independent.
pub(crate) struct ValidationInputs {
    pub checks: String,
    pub manifest: Option<std::path::PathBuf>,
    pub signature: Option<std::path::PathBuf>,
    pub archive: Option<std::path::PathBuf>,
    pub expected_signer: Option<[u8; 33]>,
}

impl Default for ValidationInputs {
    fn default() -> Self {
        Self {
            checks: "all".into(),
            manifest: None,
            signature: None,
            archive: None,
            expected_signer: None,
        }
    }
}

/// Component evidence only: an archive result does not verify installed native files.
pub(crate) struct ArtifactAudit {
    pub metadata: eyre::Result<outbe_snapshot::manifest::SnapshotManifestV1>,
    pub provenance: super::report::ProvenanceObservation,
    pub provenance_result: eyre::Result<()>,
    pub archive_result: Option<eyre::Result<()>>,
}

const MANIFEST_LIMIT: u64 = 256 * 1024 * 1024;
const SIGNATURE_LIMIT: u64 = 64 * 1024;

/// Keep the native anchored descriptor and path identity throughout both passes.
struct ArtifactSource {
    root: outbe_snapshot::fs::SourceRoot,
    member: std::path::PathBuf,
    entry: outbe_snapshot::fs::SourceEntry,
}

impl ArtifactSource {
    fn open(path: &std::path::Path) -> eyre::Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let member = path
            .file_name()
            .ok_or_else(|| eyre::eyre!("artifact path has no filename"))?;
        let root = outbe_snapshot::fs::SourceRoot::open(parent).map_err(artifact_io)?;
        let entry = root
            .open_entry(std::path::Path::new(member))
            .map_err(artifact_io)?;
        eyre::ensure!(
            !entry.identity.is_directory,
            "artifact is not a regular file"
        );
        Ok(Self {
            root,
            member: member.into(),
            entry,
        })
    }

    fn verify_unchanged(&self) -> eyre::Result<()> {
        self.entry.verify_unchanged()?;
        self.root.verify_unchanged()?;
        self.root.reopen(&self.member, &self.entry.identity)?;
        Ok(())
    }
}

fn artifact_io(error: std::io::Error) -> eyre::Report {
    if error.kind() == std::io::ErrorKind::NotFound {
        eyre::Report::new(error).wrap_err(super::Incomplete(
            "required artifact metadata is missing".into(),
        ))
    } else {
        error.into()
    }
}

/// Reports own their diagnostics; preserve the typed unavailable-input distinction.
fn artifact_error(error: &eyre::Report) -> eyre::Report {
    if error.downcast_ref::<super::Incomplete>().is_some() {
        super::Incomplete(format!("{error:#}")).into()
    } else {
        eyre::eyre!("{error:#}")
    }
}

fn missing_artifact(name: &str) -> eyre::Report {
    super::Incomplete(format!("required artifact {name} is missing")).into()
}

fn read_sidecar(path: &std::path::Path, limit: u64) -> eyre::Result<Vec<u8>> {
    use std::io::Read;
    let mut source = ArtifactSource::open(path)?;
    eyre::ensure!(
        source.entry.identity.size <= limit,
        "artifact metadata exceeds size limit"
    );
    let mut bytes = Vec::new();
    (&mut source.entry.file)
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    eyre::ensure!(
        bytes.len() as u64 <= limit,
        "artifact metadata exceeds size limit"
    );
    source.verify_unchanged()?;
    Ok(bytes)
}

fn archive_prefix(source: &mut ArtifactSource) -> (eyre::Result<Vec<u8>>, eyre::Result<Vec<u8>>) {
    use std::io::Read;
    // Bound even tar extension processing before the two accepted metadata members.
    // No payload entry is requested during this pass.
    let reader = (&mut source.entry.file).take(MANIFEST_LIMIT + SIGNATURE_LIMIT + 4096);
    let mut archive = tar::Archive::new(reader);
    let mut entries = match archive.entries() {
        Ok(entries) => entries,
        Err(error) => {
            let error = eyre::Report::new(error);
            return (Err(artifact_error(&error)), Err(error));
        }
    };
    let mut metadata = |name: &str, limit| -> eyre::Result<Vec<u8>> {
        let mut entry = entries.next().ok_or_else(|| missing_artifact(name))??;
        eyre::ensure!(
            entry.path()?.as_ref() == std::path::Path::new(name)
                && entry.header().entry_type().is_file()
                && entry.size() <= limit,
            "invalid {name} archive member"
        );
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let raw = metadata("manifest.json", MANIFEST_LIMIT);
    let signature = match &raw {
        Ok(_) => metadata("signature.json", SIGNATURE_LIMIT),
        Err(error) => Err(artifact_error(error)),
    };
    (raw, signature)
}

pub(crate) fn audit_artifact(inputs: &ValidationInputs, files_requested: bool) -> ArtifactAudit {
    use outbe_snapshot::{manifest::SnapshotManifestV1, provenance::SignatureEnvelope};
    use std::io::{Seek, SeekFrom};

    let mut archive = inputs
        .archive
        .as_deref()
        .map(ArtifactSource::open)
        .transpose();
    let mut side_manifest = inputs
        .manifest
        .as_deref()
        .map(|path| read_sidecar(path, MANIFEST_LIMIT));
    let mut side_signature = inputs
        .signature
        .as_deref()
        .map(|path| read_sidecar(path, SIGNATURE_LIMIT));
    let (raw, signature) = match &mut archive {
        Ok(Some(source)) => archive_prefix(source),
        Err(error) => (Err(artifact_error(error)), Err(artifact_error(error))),
        Ok(None) => (
            side_manifest
                .take()
                .unwrap_or_else(|| Err(missing_artifact("manifest"))),
            side_signature
                .take()
                .unwrap_or_else(|| Err(missing_artifact("signature"))),
        ),
    };
    let agreement = (|| -> eyre::Result<()> {
        if inputs.archive.is_some() {
            if let Some(supplied) = &side_manifest {
                let supplied = supplied.as_ref().map_err(artifact_error)?;
                let raw = raw.as_ref().map_err(artifact_error)?;
                eyre::ensure!(
                    supplied == raw,
                    "detached manifest differs from exact archive manifest bytes"
                );
            }
        }
        Ok(())
    })();
    let mut metadata = (|| -> eyre::Result<SnapshotManifestV1> {
        agreement.as_ref().map_err(artifact_error)?;
        Ok(SnapshotManifestV1::from_bytes(
            raw.as_ref().map_err(artifact_error)?,
        )?)
    })();
    let mut provenance = super::report::ProvenanceObservation::default();
    let mut provenance_result = (|| -> eyre::Result<()> {
        let raw = raw.as_ref().map_err(artifact_error)?;
        let signature = signature.as_ref().map_err(artifact_error)?;
        provenance.signature_valid = Some(false);
        let signer = SignatureEnvelope::from_bytes(signature)?.verify(raw, None)?;
        provenance.signature_valid = Some(true);
        provenance.signer = Some(hex::encode(signer));
        agreement.as_ref().map_err(artifact_error)?;
        if inputs.archive.is_some() {
            if let Some(supplied) = &side_signature {
                let supplied = supplied.as_ref().map_err(artifact_error)?;
                let other = SignatureEnvelope::from_bytes(supplied)?.verify(raw, None)?;
                eyre::ensure!(
                    other == signer,
                    "detached signature has a different authenticated creator"
                );
            }
        }
        if let Some(expected) = inputs.expected_signer {
            provenance.expected_signer_match = Some(expected == signer);
            eyre::ensure!(
                expected == signer,
                "snapshot creator does not match expected public key"
            );
        }
        Ok(())
    })();
    let mut archive_result = if files_requested && inputs.archive.is_some() {
        Some((|| -> eyre::Result<()> {
            let source = archive
                .as_mut()
                .map_err(|e| artifact_error(e))?
                .as_mut()
                .ok_or_else(|| missing_artifact("archive"))?;
            let raw = raw.as_ref().map_err(artifact_error)?;
            // Preserve unavailable metadata as Incomplete; the full archive
            // reader otherwise reports a missing signature as InvalidData.
            signature.as_ref().map_err(artifact_error)?;
            source.entry.file.seek(SeekFrom::Start(0))?;
            let index = outbe_snapshot::archive::read_archive_index(&mut source.entry.file, None)?;
            eyre::ensure!(
                &index.raw_manifest == raw,
                "archive manifest changed between metadata and payload passes"
            );
            Ok(())
        })())
    } else {
        None
    };
    if let Ok(Some(source)) = &archive {
        if let Err(error) = source.verify_unchanged() {
            metadata = Err(artifact_error(&error));
            provenance_result = Err(artifact_error(&error));
            if let Some(result) = &mut archive_result {
                *result = Err(error);
            }
        }
    }
    ArtifactAudit {
        metadata,
        provenance,
        provenance_result,
        archive_result,
    }
}

/// Compare installed native files to a structurally valid inventory. All host
/// roots and member opens come from native enumeration, never donor locations.
/// This establishes file equality only, independently of native semantic checks.
pub(crate) fn verify_native_files(
    layout: &super::super::config::NativeLayout,
    manifest: &outbe_snapshot::manifest::SnapshotManifestV1,
) -> eyre::Result<()> {
    use k256::sha2::{Digest, Sha256};
    use outbe_snapshot::{fs::SourceRoot, manifest::EntryKind};
    use std::{collections::BTreeMap, io::Read, path::Path};

    manifest.validate()?;
    // The owner enforces required native populations even when omitted by a donor.
    let inventory = super::super::inventory::enumerate_native_files(layout)?;
    let mut declared = vec![None; inventory.domains.len()];
    for domain in &manifest.domains {
        let index = inventory
            .domains
            .iter()
            .position(|native| {
                native.kind == domain.kind && native.native_root == domain.native_root
            })
            .ok_or_else(|| {
                eyre::eyre!(
                    "manifest domain has no matching native population: {:?}",
                    domain.kind
                )
            })?;
        eyre::ensure!(
            declared[index].is_none(),
            "duplicate native population in manifest: {:?}",
            domain.kind
        );
        declared[index] = Some(domain);
    }
    let mut opened = Vec::new();
    let mut files = 0_u64;
    let mut bytes = 0_u64;
    for (native, declared) in inventory.domains.iter().zip(declared) {
        let Some(domain) = declared else {
            eyre::ensure!(
                native.members.is_empty(),
                "manifest omits nonempty native population: {:?}",
                native.kind
            );
            continue;
        };
        let wanted: BTreeMap<_, _> = domain
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect();
        let names = native
            .members
            .iter()
            .map(|path| {
                path.to_str().ok_or_else(|| {
                    eyre::eyre!("native member path is not UTF-8: {}", path.display())
                })
            })
            .collect::<eyre::Result<BTreeSet<_>>>()?;
        eyre::ensure!(
            names == wanted.keys().copied().collect(),
            "native member inventory differs for {:?}",
            native.kind
        );
        // The writer does not observe roots or modes of empty optional populations.
        if native.members.is_empty() {
            continue;
        }
        let root = SourceRoot::open(&native.root)?;
        eyre::ensure!(
            root.open_entry(Path::new(""))?.identity.mode & 0o7777 == domain.mode,
            "native root mode differs for {:?}",
            native.kind
        );
        let mut observed = Vec::new();
        for member in &native.members {
            let name = member
                .to_str()
                .ok_or_else(|| eyre::eyre!("native member path is not UTF-8"))?;
            let expected = wanted
                .get(name)
                .ok_or_else(|| eyre::eyre!("native member is not declared"))?;
            let mut entry = root.open_entry(member)?;
            eyre::ensure!(
                entry.identity.mode & 0o7777 == expected.mode,
                "native member mode differs: {name}"
            );
            eyre::ensure!(
                entry.identity.is_directory == (expected.kind == EntryKind::Directory),
                "native member kind differs: {name}"
            );
            if !entry.identity.is_directory {
                eyre::ensure!(
                    entry.identity.size == expected.size,
                    "native member size differs: {name}"
                );
                let limit = expected
                    .size
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("native file size overflow"))?;
                let mut reader = (&mut entry.file).take(limit);
                let mut hash = Sha256::new();
                let mut length = 0_u64;
                let mut buffer = [0; 64 * 1024];
                loop {
                    let count = reader.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    length = length
                        .checked_add(count as u64)
                        .ok_or_else(|| eyre::eyre!("native file length overflow"))?;
                    hash.update(&buffer[..count]);
                }
                eyre::ensure!(
                    length == expected.size,
                    "native member length changed: {name}"
                );
                eyre::ensure!(
                    expected.sha256.as_deref() == Some(hex::encode(hash.finalize()).as_str()),
                    "native member checksum differs: {name}"
                );
                files = files
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("native file count overflow"))?;
                bytes = bytes
                    .checked_add(length)
                    .ok_or_else(|| eyre::eyre!("native byte count overflow"))?;
            }
            entry.verify_unchanged()?;
            observed.push((member, entry.identity));
        }
        opened.push((root, observed));
    }
    eyre::ensure!(
        files == manifest.file_count && bytes == manifest.total_bytes,
        "native file inventory totals differ"
    );
    let after = super::super::inventory::enumerate_native_files(layout)?;
    eyre::ensure!(
        after.domains.len() == inventory.domains.len(),
        "native domains changed during verification"
    );
    for before in &inventory.domains {
        let current = after
            .domains
            .iter()
            .find(|domain| domain.kind == before.kind && domain.native_root == before.native_root)
            .ok_or_else(|| eyre::eyre!("native domain disappeared during verification"))?;
        eyre::ensure!(
            current.root == before.root
                && current.members.iter().collect::<BTreeSet<_>>()
                    == before.members.iter().collect::<BTreeSet<_>>(),
            "native member inventory changed during verification"
        );
    }
    for (root, observed) in opened {
        root.verify_unchanged()?;
        for (member, identity) in observed {
            root.reopen(member, &identity)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_snapshot(
    inputs: &ValidationInputs,
    node_args: Vec<std::ffi::OsString>,
    scratch_parent: &std::path::Path,
) -> eyre::Result<super::report::ValidationReport> {
    use super::report::{CheckName::*, ValidationReport};
    use crate::snapshot::config::{
        parse_node_inputs, resolve_requested_layout, NativeReadSelection,
    };

    let selection = CheckSelection::resolve(
        &inputs.checks,
        inputs.manifest.is_some() || inputs.signature.is_some() || inputs.archive.is_some(),
        inputs.expected_signer.is_some(),
    )?;
    let mut report = ValidationReport::new(selection.checks.iter().copied());
    report.protected_paths.0.extend(
        inputs
            .manifest
            .iter()
            .chain(&inputs.signature)
            .chain(&inputs.archive)
            .cloned(),
    );
    let native_requested = [Headers, Evm, Ce, Bodies, Ocomp]
        .iter()
        .any(|name| selection.checks.contains(name));
    let mut metadata = None;
    let mut files_ready = false;
    if selection.checks.contains(&Provenance) || selection.checks.contains(&Files) {
        let audit = audit_artifact(inputs, selection.checks.contains(&Files));
        report.provenance = audit.provenance;
        record_outcome(&mut report, Provenance, &audit.provenance_result);
        if selection.checks.contains(&Files) {
            if audit.provenance_result.is_err() {
                block_on(&mut report, Files, Provenance);
            } else if let Err(error) = &audit.metadata {
                record_failure(&mut report, Files, error);
            } else if let Some(Err(error)) = &audit.archive_result {
                record_failure(&mut report, Files, error);
            } else {
                files_ready = true;
            }
        }
        metadata = Some(audit.metadata);
    }
    // Provenance needs only exact artifact bytes, never node configuration/stores.
    if !native_requested && !selection.checks.contains(&Files) {
        return Ok(report);
    }
    let node = match parse_node_inputs(node_args) {
        Ok(node) => node,
        Err(error) => {
            fail_selected(
                &mut report,
                &[Files, Headers, Evm, Ce, Bodies, Ocomp],
                &error,
            );
            return Ok(report);
        }
    };
    // A bad/missing unneeded projection config cannot poison execution-only work.
    let mut layout =
        match resolve_requested_layout(&node, NativeReadSelection { projection: false }) {
            Ok(layout) => layout,
            Err(error) => {
                fail_selected(
                    &mut report,
                    &[Files, Headers, Evm, Ce, Bodies, Ocomp],
                    &error,
                );
                return Ok(report);
            }
        };
    report
        .protected_paths
        .0
        .extend(validation_protected(&layout).0);
    let projection_error = if selection.needs_projection() {
        match resolve_requested_layout(&node, NativeReadSelection { projection: true }) {
            Ok(full) => {
                layout = full;
                report
                    .protected_paths
                    .0
                    .extend(validation_protected(&layout).0);
                None
            }
            Err(error) => Some(error),
        }
    } else {
        None
    };
    if let Some(error) = &projection_error {
        fail_selected(&mut report, &[Files, Bodies, Ocomp], error);
    }
    // CRITICAL: exact file hashes include native MDBX lock bookkeeping.
    // Do this before any native DB open or progress inspection, including CE.
    if files_ready && projection_error.is_none() {
        let result = (|| -> eyre::Result<()> {
            let manifest = metadata
                .as_ref()
                .ok_or_else(|| missing_artifact("manifest"))?
                .as_ref()
                .map_err(artifact_error)?;
            let native = files_layout(&layout)?;
            verify_native_files(&native, manifest)
        })();
        record_outcome(&mut report, Files, &result);
    }
    if !native_requested {
        return Ok(report);
    }

    let needs_work = [Evm, Ce, Bodies, Ocomp]
        .iter()
        .any(|name| selection.checks.contains(name));
    let scratch = if needs_work {
        let result = (|| -> eyre::Result<tempfile::TempDir> {
            let mut protected = validation_protected(&layout);
            protected.0.extend(
                inputs
                    .manifest
                    .iter()
                    .chain(&inputs.signature)
                    .chain(&inputs.archive)
                    .cloned(),
            );
            outbe_snapshot::layout::validate_layout(
                &[],
                &protected,
                &[scratch_parent.to_path_buf()],
            )?;
            Ok(tempfile::Builder::new()
                .prefix("snapshot-validation-")
                .tempdir_in(scratch_parent)?)
        })();
        match result {
            Ok(scratch) => Some(scratch),
            Err(error) => {
                fail_selected(&mut report, &[Evm, Ce, Bodies, Ocomp], &error);
                None
            }
        }
    } else {
        None
    };
    // This call owns every native view and drops them before outer scratch.close().
    run_native_checks(
        &layout,
        &selection,
        projection_error.as_ref(),
        scratch.as_ref().map(|s| s.path()),
        &mut report,
    );
    if let Some(scratch) = scratch {
        if let Err(error) = scratch.close() {
            let error = eyre::eyre!("remove validation scratch: {error}");
            fail_selected(&mut report, &[Evm, Ce, Bodies, Ocomp], &error);
        }
    }
    // No unused selected check is silently promoted. Pending means Incomplete.
    Ok(report)
}

/// Resolve report destinations without opening native stores or requiring any
/// native semantic check. Unknown configured roots prevent file publication.
pub(crate) fn report_protected_paths(
    node_args: Vec<std::ffi::OsString>,
) -> eyre::Result<outbe_snapshot::layout::ProtectedPaths> {
    if node_args.is_empty() {
        // Artifact-only inspection may supply no native dataset at all.
        return Ok(outbe_snapshot::layout::ProtectedPaths::default());
    }
    let inputs = crate::snapshot::config::parse_node_inputs(node_args)?;
    let layout = crate::snapshot::config::resolve_report_layout(&inputs)?;
    Ok(validation_protected(&layout))
}

fn validation_protected(
    layout: &crate::snapshot::config::RequestedLayout,
) -> outbe_snapshot::layout::ProtectedPaths {
    let mut protected = layout.protected.clone();
    protected.0.extend([
        layout.chain_root.clone(),
        layout.consensus_root.clone(),
        layout.ocomp_root.clone(),
        layout.static_files_root.clone(),
        layout.execution_rocksdb_root.clone(),
    ]);
    protected
        .0
        .extend(layout.projection.as_ref().map(|p| p.root.clone()));
    protected
}

fn files_layout(
    layout: &crate::snapshot::config::RequestedLayout,
) -> eyre::Result<crate::snapshot::config::NativeLayout> {
    let projection = layout
        .projection
        .as_ref()
        .ok_or_else(|| super::Incomplete("missing selected projection configuration".into()))?;
    Ok(crate::snapshot::config::NativeLayout {
        chain: layout.chain.clone(),
        chain_root: layout.chain_root.clone(),
        consensus_root: layout.consensus_root.clone(),
        ocomp_root: layout.ocomp_root.clone(),
        offchain_root: projection.root.clone(),
        static_files_root: layout.static_files_root.clone(),
        execution_rocksdb_root: layout.execution_rocksdb_root.clone(),
        projection_start_block: projection.start_block,
        protected: layout.protected.clone(),
    })
}

fn failure_status(error: &eyre::Report) -> super::report::CheckStatus {
    use super::report::CheckStatus;
    if error.downcast_ref::<super::Incomplete>().is_some()
        || error.chain().any(|source| {
            source
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        })
    {
        CheckStatus::Incomplete
    } else {
        CheckStatus::Failed
    }
}

fn record_failure(
    report: &mut super::report::ValidationReport,
    check: CheckName,
    error: &eyre::Report,
) {
    report.record(check, failure_status(error), Some(&format!("{error:#}")));
}

fn record_outcome<T>(
    report: &mut super::report::ValidationReport,
    check: CheckName,
    result: &eyre::Result<T>,
) {
    match result {
        Ok(_) => report.record(check, super::report::CheckStatus::Passed, None),
        Err(error) => record_failure(report, check, error),
    }
}

fn fail_selected(
    report: &mut super::report::ValidationReport,
    checks: &[CheckName],
    error: &eyre::Report,
) {
    for check in checks {
        // Preserve an already reported primary contradiction over secondary errors.
        if report.check(*check).status != super::report::CheckStatus::Failed {
            record_failure(report, *check, error);
        }
    }
}

fn block_on(
    report: &mut super::report::ValidationReport,
    check: CheckName,
    prerequisite: CheckName,
) {
    use super::report::CheckStatus;
    if report.check(check).status == CheckStatus::Failed {
        return;
    }
    let status = report.check(prerequisite).status;
    if status != CheckStatus::Passed {
        report.record(
            check,
            if status == CheckStatus::Failed {
                CheckStatus::Failed
            } else {
                CheckStatus::Incomplete
            },
            Some(&format!("required {prerequisite:?} check is {status:?}")),
        );
    }
}

fn require_source(path: &std::path::Path, directory: bool) -> eyre::Result<()> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        let context = format!("selected native source unavailable: {}", path.display());
        if error.kind() == std::io::ErrorKind::NotFound {
            eyre::Report::new(error).wrap_err(super::Incomplete(context))
        } else {
            eyre::Report::new(error).wrap_err(context)
        }
    })?;
    eyre::ensure!(
        if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        },
        "selected native source has wrong file kind: {}",
        path.display()
    );
    Ok(())
}

fn canonical_ce_identity(
    layout: &crate::snapshot::config::RequestedLayout,
) -> outbe_compressed_entities::EnvironmentIdentity {
    use outbe_compressed_entities::{
        CeTopologyV1, EnvironmentIdentity, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
    };
    // Same canonical identity as existing native::ce_identity, whose signature
    // requires a full projection layout. CE-only must not resolve projection.
    EnvironmentIdentity {
        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: layout.chain.chain().id(),
        genesis_hash: layout.chain.genesis_hash(),
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        topology: CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".into(),
        vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
    }
}

fn record_count(report: &mut super::report::ValidationReport, name: &str, count: u64) {
    report
        .inventory_bounds
        .push(super::report::InventoryBounds {
            name: name.into(),
            start: 0,
            end_exclusive: count,
            visited: count,
        });
}

fn run_native_checks(
    layout: &crate::snapshot::config::RequestedLayout,
    selection: &CheckSelection,
    projection_error: Option<&eyre::Report>,
    scratch: Option<&std::path::Path>,
    report: &mut super::report::ValidationReport,
) {
    use super::report::{CheckName::*, CheckStatus, RequiredHeight, RetainedRange};
    let opened = (|| -> eyre::Result<crate::snapshot::native::RethReadOnlyView> {
        require_source(&layout.chain_root.join("db/mdbx.dat"), false)?;
        require_source(&layout.static_files_root, true)?;
        crate::snapshot::native::RethReadOnlyView::open_requested(layout)
    })();
    let view = match opened {
        Ok(view) => view,
        Err(error) => {
            fail_selected(report, &[Headers, Evm, Ce, Bodies, Ocomp], &error);
            return;
        }
    };
    report.observed.h = Some(view.progress.finalized.clone());
    report.observed.e = Some(view.progress.execution.clone());
    let mut required = vec![
        view.progress.finalized.number,
        view.progress.execution.number,
    ];
    let mut ce_input = None;
    if selection.checks.contains(&Ce) && scratch.is_some() {
        let prepared = (|| -> eyre::Result<_> {
            require_source(
                &layout.chain_root.join("compressed_entities/smt/mdbx.dat"),
                false,
            )?;
            let reader = outbe_compressed_entities::CeMdbxReadOnly::open(
                &layout.chain_root,
                canonical_ce_identity(layout),
            )?;
            let marker = reader.marker()?;
            Ok((reader, marker))
        })();
        match prepared {
            Ok((reader, marker)) => {
                report.observed.q = Some(outbe_snapshot::manifest::BlockIdentity {
                    number: marker.height,
                    hash: hex::encode(marker.block_hash),
                });
                required.push(marker.height);
                ce_input = Some((reader, marker));
            }
            Err(error) => record_failure(report, Ce, &error),
        }
    }
    required.sort_unstable();
    required.dedup();
    let headers = match super::headers::verify_retained_headers(&view, &required) {
        Ok(headers) => {
            for range in &headers.intervals {
                report.retained_ranges.push(RetainedRange {
                    domain: "headers".into(),
                    start: *range.start(),
                    end_inclusive: *range.end(),
                });
            }
            record_count(report, "retained_headers", headers.verified_headers);
            for height in &headers.required_missing {
                report.required_missing.push(RequiredHeight {
                    domain: "headers".into(),
                    height: *height,
                });
            }
            if headers.required_missing.is_empty() {
                report.record(Headers, CheckStatus::Passed, None);
            } else {
                report.record(
                    Headers,
                    CheckStatus::Incomplete,
                    Some("required native header anchors are not retained"),
                );
            }
            Some(headers)
        }
        Err(error) => {
            record_failure(report, Headers, &error);
            None
        }
    };
    let Some(headers) = headers else {
        for check in [Evm, Ce, Bodies, Ocomp] {
            block_on(report, check, Headers);
        }
        return;
    };
    // Retained structure was verified. Missing anchors remain explicit in the
    // header report; each independent check requires its own exact header.
    let mut verified = None;
    if selection.checks.contains(&Evm) {
        if let Some(scratch) = scratch {
            let result = super::evm::verify_current_evm(&view, scratch);
            record_outcome(report, Evm, &result);
            if let Ok(state) = result {
                report.retained_ranges.push(RetainedRange {
                    domain: "current_evm".into(),
                    start: state.header.inner.number,
                    end_inclusive: state.header.inner.number,
                });
                verified = Some(state);
            }
        }
    }
    if let (Some((reader, marker)), Some(scratch)) = (ce_input, scratch) {
        audit_ce_and_bodies(
            layout,
            &view,
            (reader, marker),
            scratch,
            selection.checks.contains(&Bodies),
            projection_error.is_none(),
            report,
        );
    } else if selection.checks.contains(&Bodies) {
        block_on(report, Bodies, Ce);
    }
    if selection.checks.contains(&Ocomp) {
        if report.check(Evm).status != CheckStatus::Passed {
            block_on(report, Ocomp, Evm);
        } else if projection_error.is_none() {
            if let (Some(state), Some(scratch)) = (verified.as_ref(), scratch) {
                let canonical = super::canonical_state::CanonicalState::new(state, &view, &headers);
                let result = super::ocomp::verify_ocomp_relations(
                    &canonical, &view, layout, scratch, report,
                );
                record_outcome(report, Ocomp, &result);
            }
        }
    }
}

struct IgnoreCeLeaves;
impl outbe_compressed_entities::CeAuditVisitor for IgnoreCeLeaves {
    fn visit_leaf(
        &mut self,
        _namespace: outbe_compressed_entities::TreeNamespace,
        _key: outbe_compressed_entities::TreeKey,
        _value: outbe_compressed_entities::LeafValue,
    ) -> Result<(), outbe_compressed_entities::CeAuditError> {
        Ok(())
    }
}

struct CountRetainedBodies(u64);
impl outbe_tribute::RetainedTributeAuditVisitor for CountRetainedBodies {
    fn visit_retained(
        &mut self,
        _entry: outbe_tribute::RetainedTributeAuditEntry,
    ) -> Result<(), outbe_compressed_entities::CeAuditError> {
        self.0 = self.0.checked_add(1).ok_or_else(|| {
            outbe_compressed_entities::CeAuditError::Invalid("retained body count overflow".into())
        })?;
        Ok(())
    }
}

fn audit_ce_and_bodies(
    layout: &crate::snapshot::config::RequestedLayout,
    view: &crate::snapshot::native::RethReadOnlyView,
    ce_input: (
        outbe_compressed_entities::CeMdbxReadOnly,
        outbe_compressed_entities::FinalizedMarker,
    ),
    scratch: &std::path::Path,
    bodies_requested: bool,
    projection_available: bool,
    report: &mut super::report::ValidationReport,
) {
    use super::report::{CheckName::*, CheckStatus, RequiredHeight, RetainedRange};
    use outbe_compressed_entities::{CeAuditLimits, CeAuditWork, CeBodyAudit};
    let (reader, marker) = ce_input;
    let ce_result = (|| -> eyre::Result<_> {
        let header = view.header(marker.height)?.ok_or_else(|| {
            report.required_missing.push(RequiredHeight {
                domain: "ce_header".into(),
                height: marker.height,
            });
            super::Incomplete(format!("missing CE marker header Q={}", marker.height))
        })?;
        let work = CeAuditWork::create(scratch.join("ce-audit"), CeAuditLimits::default())?;
        // Keep work alive through the body comparator; a single CE traversal
        // populates its expected leaf stream. No duplicate CE root scan.
        if bodies_requested && projection_available {
            let mut expected = CeBodyAudit::create(&work)?;
            let ce = super::ce::verify_ce(&reader, &header, &work, &mut expected)?;
            record_ce_success(report, marker.height, &ce);
            let bodies = (|| -> eyre::Result<()> {
                let projection = super::bodies::ProjectionBodyView::open(layout, scratch)?;
                let body = projection.verify_bodies(&marker, expected, &work)?;
                report.observed.p = Some(outbe_snapshot::manifest::BlockIdentity {
                    number: body.checkpoint.block_number,
                    hash: hex::encode(body.checkpoint.block_hash),
                });
                // Check retained structure even when live Q/P equality is unavailable.
                let mut retained = CountRetainedBodies(0);
                projection.audit_retained(&work, &mut retained)?;
                record_count(report, "retained_tribute_bodies", retained.0);
                report.body_structure = Some(super::report::BodyStructureObservation {
                    checkpoint: outbe_snapshot::manifest::BlockIdentity {
                        number: body.checkpoint.block_number,
                        hash: hex::encode(body.checkpoint.block_hash),
                    },
                    status: CheckStatus::Passed,
                });
                let equality = body.equality?;
                record_count(report, "live_projection_bodies", equality.bodies);
                report.retained_ranges.push(RetainedRange {
                    domain: "live_body_equality".into(),
                    start: marker.height,
                    end_inclusive: marker.height,
                });
                Ok(())
            })();
            record_outcome(report, Bodies, &bodies);
        } else {
            let ce = super::ce::verify_ce(&reader, &header, &work, &mut IgnoreCeLeaves)?;
            record_ce_success(report, marker.height, &ce);
        }
        Ok(())
    })();
    if let Err(error) = ce_result {
        record_failure(report, Ce, &error);
        if bodies_requested {
            block_on(report, Bodies, Ce);
        }
    } else if bodies_requested && !projection_available {
        // Projection resolution already supplied a concrete error, not an invented P.
        if report.check(Bodies).status == CheckStatus::Incomplete
            && report.check(Bodies).diagnostic.is_none()
        {
            report.record(
                Bodies,
                CheckStatus::Incomplete,
                Some("missing selected projection configuration"),
            );
        }
    }
}

fn record_ce_success(
    report: &mut super::report::ValidationReport,
    height: u64,
    ce: &outbe_compressed_entities::CeAuditReport,
) {
    report.record(CheckName::Ce, super::report::CheckStatus::Passed, None);
    report.retained_ranges.push(super::report::RetainedRange {
        domain: "ce".into(),
        start: height,
        end_inclusive: height,
    });
    record_count(report, "ce_trees", ce.trees);
    record_count(report, "ce_leaves", ce.leaves);
}
