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
                    _ => {
                        return Err(crate::errors::SnapshotError::UnknownSnapshotCheck {
                            name: name.to_owned(),
                        }
                        .into())
                    }
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

use super::artifact::{artifact_error, missing_artifact};
pub(crate) use super::artifact::{audit_artifact, verify_native_files};

fn run_artifact_phase(
    inputs: &ValidationInputs,
    selection: &CheckSelection,
    report: &mut super::report::ValidationReport,
) -> (
    Option<eyre::Result<outbe_snapshot::manifest::SnapshotManifestV1>>,
    bool,
) {
    use super::report::CheckName::*;
    if !selection.checks.contains(&Provenance) && !selection.checks.contains(&Files) {
        return (None, false);
    }
    let audit = audit_artifact(inputs, selection.checks.contains(&Files));
    report.provenance = audit.provenance;
    record_outcome(report, Provenance, &audit.provenance_result);
    let mut files_ready = false;
    if selection.checks.contains(&Files) {
        if audit.provenance_result.is_err() {
            block_on(report, Files, Provenance);
        } else if let Err(error) = &audit.metadata {
            record_failure(report, Files, error);
        } else if let Some(Err(error)) = &audit.archive_result {
            record_failure(report, Files, error);
        } else {
            files_ready = true;
        }
    }
    (Some(audit.metadata), files_ready)
}

fn resolve_validation_layout(
    node: &crate::snapshot::config::NodeInputs,
    selection: &CheckSelection,
    report: &mut super::report::ValidationReport,
) -> Option<(
    crate::snapshot::config::RequestedLayout,
    Option<eyre::Report>,
)> {
    use super::report::CheckName::*;
    use crate::snapshot::config::{resolve_requested_layout, NativeReadSelection};
    // A bad/missing unneeded projection config cannot poison execution-only work.
    let mut layout = match resolve_requested_layout(node, NativeReadSelection { projection: false })
    {
        Ok(layout) => layout,
        Err(error) => {
            fail_selected(report, &[Files, Headers, Evm, Ce, Bodies, Ocomp], &error);
            return None;
        }
    };
    report
        .protected_paths
        .0
        .extend(validation_protected(&layout).0);
    let projection_error = if selection.needs_projection() {
        match resolve_requested_layout(node, NativeReadSelection { projection: true }) {
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
        fail_selected(report, &[Files, Bodies, Ocomp], error);
    }
    Some((layout, projection_error))
}

fn create_validation_scratch(
    inputs: &ValidationInputs,
    layout: &crate::snapshot::config::RequestedLayout,
    scratch_parent: &std::path::Path,
    report: &mut super::report::ValidationReport,
) -> Option<tempfile::TempDir> {
    use super::report::CheckName::*;
    let result = (|| -> eyre::Result<tempfile::TempDir> {
        let mut protected = validation_protected(layout);
        protected.0.extend(
            inputs
                .manifest
                .iter()
                .chain(&inputs.signature)
                .chain(&inputs.archive)
                .cloned(),
        );
        outbe_snapshot::layout::validate_layout(&[], &protected, &[scratch_parent.to_path_buf()])?;
        Ok(tempfile::Builder::new()
            .prefix("snapshot-validation-")
            .tempdir_in(scratch_parent)?)
    })();
    match result {
        Ok(scratch) => Some(scratch),
        Err(error) => {
            fail_selected(report, &[Evm, Ce, Bodies, Ocomp], &error);
            None
        }
    }
}

pub(crate) fn validate_snapshot_with_node(
    inputs: &ValidationInputs,
    parse_node: impl FnOnce() -> eyre::Result<crate::snapshot::config::NodeInputs>,
    scratch_parent: &std::path::Path,
) -> eyre::Result<super::report::ValidationReport> {
    use super::report::{CheckName::*, ValidationReport};

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
    let (metadata, files_ready) = run_artifact_phase(inputs, &selection, &mut report);
    // Provenance needs only exact artifact bytes, never node configuration/stores.
    if !native_requested && !selection.checks.contains(&Files) {
        return Ok(report);
    }
    let node = match parse_node() {
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
    let Some((layout, projection_error)) =
        resolve_validation_layout(&node, &selection, &mut report)
    else {
        return Ok(report);
    };
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
    let scratch = needs_work
        .then(|| create_validation_scratch(inputs, &layout, scratch_parent, &mut report))
        .flatten();
    // This call owns every native view and drops them before outer scratch.close().
    run_native_checks(
        &layout,
        &selection,
        projection_error.as_ref(),
        scratch.as_ref().map(|s| s.path()),
        &mut report,
    );
    if let Some(scratch) = scratch {
        if let Err(source) = scratch.close() {
            let error = crate::errors::SnapshotError::RemoveValidationScratch { source }.into();
            fail_selected(&mut report, &[Evm, Ce, Bodies, Ocomp], &error);
        }
    }
    // No unused selected check is silently promoted. Pending means Incomplete.
    Ok(report)
}

#[cfg(all(test, feature = "snapshot-integration"))]
pub(crate) fn validate_snapshot(
    inputs: &ValidationInputs,
    node_args: Vec<std::ffi::OsString>,
    scratch_parent: &std::path::Path,
) -> eyre::Result<super::report::ValidationReport> {
    validate_snapshot_with_node(
        inputs,
        || crate::snapshot::config::parse_node_inputs(node_args),
        scratch_parent,
    )
}

/// Resolve report destinations without opening native stores or requiring any
/// native semantic check. Unknown configured roots prevent file publication.
pub(crate) fn report_protected_paths(
    inputs: &crate::snapshot::config::NodeInputs,
) -> eyre::Result<outbe_snapshot::layout::ProtectedPaths> {
    let layout = crate::snapshot::config::resolve_report_layout(inputs)?;
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

pub(super) use super::report::{block_on, fail_selected, record_failure, record_outcome};

fn run_native_checks(
    layout: &crate::snapshot::config::RequestedLayout,
    selection: &CheckSelection,
    projection_error: Option<&eyre::Report>,
    scratch: Option<&std::path::Path>,
    report: &mut super::report::ValidationReport,
) {
    super::native_checks::run_native_checks(layout, selection, projection_error, scratch, report);
}
