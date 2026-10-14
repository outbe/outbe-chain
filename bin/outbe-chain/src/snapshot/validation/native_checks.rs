//! Native semantic check execution (Headers, EVM, CE, Bodies, and OCOMP).

use super::{
    report::{
        BodyStructureObservation, CheckName, CheckStatus, InventoryBounds, RequiredHeight,
        RetainedRange, ValidationReport,
    },
    run::{block_on, fail_selected, record_failure, record_outcome, CheckSelection},
};

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
        tree_format: "ckb-smt-v0.6.1-poseidon2-catalog-v3".into(),
        vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
    }
}

fn record_count(report: &mut ValidationReport, name: &str, count: u64) {
    report.inventory_bounds.push(InventoryBounds {
        name: name.into(),
        start: 0,
        end_exclusive: count,
        visited: count,
    });
}

fn prepare_ce_input(
    layout: &crate::snapshot::config::RequestedLayout,
    selection: &CheckSelection,
    has_scratch: bool,
    required: &mut Vec<u64>,
    report: &mut ValidationReport,
) -> Option<(
    outbe_compressed_entities::CeMdbxReadOnly,
    outbe_compressed_entities::FinalizedMarker,
)> {
    if !selection.checks.contains(&CheckName::Ce) || !has_scratch {
        return None;
    }
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
            Some((reader, marker))
        }
        Err(error) => {
            record_failure(report, CheckName::Ce, &error);
            None
        }
    }
}

fn verify_and_record_headers(
    view: &crate::snapshot::native::RethReadOnlyView,
    required: &[u64],
    report: &mut ValidationReport,
) -> Option<super::headers::HeaderAudit> {
    use CheckName::*;
    match super::headers::verify_retained_headers(view, required) {
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
    }
}

fn verify_and_record_evm(
    view: &crate::snapshot::native::RethReadOnlyView,
    selection: &CheckSelection,
    scratch: Option<&std::path::Path>,
    report: &mut ValidationReport,
) -> Option<super::evm::VerifiedState> {
    use CheckName::*;
    if !selection.checks.contains(&Evm) {
        return None;
    }
    let scratch = scratch?;
    let result = super::evm::verify_current_evm(view, scratch);
    record_outcome(report, Evm, &result);
    let state = result.ok()?;
    report.retained_ranges.push(RetainedRange {
        domain: "current_evm".into(),
        start: state.header.inner.number,
        end_inclusive: state.header.inner.number,
    });
    Some(state)
}

fn run_ocomp_check(
    layout: &crate::snapshot::config::RequestedLayout,
    view: &crate::snapshot::native::RethReadOnlyView,
    (verified, headers, scratch): (
        Option<&super::evm::VerifiedState>,
        &super::headers::HeaderAudit,
        Option<&std::path::Path>,
    ),
    projection_available: bool,
    report: &mut ValidationReport,
) {
    use CheckName::*;
    if report.check(Evm).status != CheckStatus::Passed {
        block_on(report, Ocomp, Evm);
        return;
    }
    if !projection_available {
        return;
    }
    let (Some(state), Some(scratch)) = (verified, scratch) else {
        return;
    };
    let canonical = super::canonical_state::CanonicalState::new(state, view, headers);
    let result = super::ocomp::verify_ocomp_relations(&canonical, view, layout, scratch, report);
    match result {
        Ok(()) => report.record(
            Ocomp,
            CheckStatus::Passed,
            Some("OCOMP relations passed. Private Tribute amounts were not read."),
        ),
        Err(error) => record_failure(report, Ocomp, &error),
    }
}

pub(super) fn run_native_checks(
    layout: &crate::snapshot::config::RequestedLayout,
    selection: &CheckSelection,
    projection_error: Option<&eyre::Report>,
    scratch: Option<&std::path::Path>,
    report: &mut ValidationReport,
) {
    use CheckName::*;
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
    let ce_input = prepare_ce_input(layout, selection, scratch.is_some(), &mut required, report);
    required.sort_unstable();
    required.dedup();
    let Some(headers) = verify_and_record_headers(&view, &required, report) else {
        for check in [Evm, Ce, Bodies, Ocomp] {
            block_on(report, check, Headers);
        }
        return;
    };
    // Retained structure was verified. Missing anchors remain explicit in the
    // header report. Each independent check requires its own exact header.
    let verified = verify_and_record_evm(&view, selection, scratch, report);
    if let (Some((reader, marker)), Some(scratch)) = (ce_input, scratch) {
        audit_ce_and_bodies(
            (layout, &view, scratch),
            (reader, marker),
            (selection.checks.contains(&Bodies), projection_error.is_none()),
            report,
        );
    } else if selection.checks.contains(&Bodies) {
        block_on(report, Bodies, Ce);
    }
    if selection.checks.contains(&Ocomp) {
        run_ocomp_check(
            layout,
            &view,
            (verified.as_ref(), &headers, scratch),
            projection_error.is_none(),
            report,
        );
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

fn audit_projection_bodies(
    layout: &crate::snapshot::config::RequestedLayout,
    scratch: &std::path::Path,
    marker: &outbe_compressed_entities::FinalizedMarker,
    (expected, work): (
        outbe_compressed_entities::CeBodyAudit,
        &outbe_compressed_entities::CeAuditWork,
    ),
    report: &mut ValidationReport,
) -> eyre::Result<()> {
    let projection = super::bodies::ProjectionBodyView::open(layout, scratch)?;
    let body = projection.verify_bodies(marker, expected, work)?;
    report.observed.p = Some(outbe_snapshot::manifest::BlockIdentity {
        number: body.checkpoint.block_number,
        hash: hex::encode(body.checkpoint.block_hash),
    });
    // Check retained structure even when live Q/P equality is unavailable.
    let mut retained = CountRetainedBodies(0);
    projection.audit_retained(work, &mut retained)?;
    record_count(report, "retained_tribute_bodies", retained.0);
    report.body_structure = Some(BodyStructureObservation {
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
}

fn audit_ce_and_bodies(
    (layout, view, scratch): (
        &crate::snapshot::config::RequestedLayout,
        &crate::snapshot::native::RethReadOnlyView,
        &std::path::Path,
    ),
    (reader, marker): (
        outbe_compressed_entities::CeMdbxReadOnly,
        outbe_compressed_entities::FinalizedMarker,
    ),
    (bodies_requested, projection_available): (bool, bool),
    report: &mut ValidationReport,
) {
    use outbe_compressed_entities::{CeAuditLimits, CeAuditWork, CeBodyAudit};
    use CheckName::*;
    let ce_result = (|| -> eyre::Result<_> {
        let header = view.header(marker.height)?.ok_or_else(|| {
            report.required_missing.push(RequiredHeight {
                domain: "ce_header".into(),
                height: marker.height,
            });
            super::Incomplete(format!("missing CE marker header Q={}", marker.height))
        })?;
        let work = CeAuditWork::create(scratch.join("ce-audit"), CeAuditLimits::default())?;
        // Keep work alive through the body comparator. A single CE traversal
        // populates its expected leaf stream. No duplicate CE root scan.
        if bodies_requested && projection_available {
            let mut expected = CeBodyAudit::create(&work)?;
            let ce = super::ce::verify_ce(&reader, &header, &work, &mut expected)?;
            record_ce_success(report, marker.height, &ce);
            let bodies =
                audit_projection_bodies(layout, scratch, &marker, (expected, &work), report);
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
    } else if bodies_requested
        && !projection_available
        && report.check(Bodies).status == CheckStatus::Incomplete
        && report.check(Bodies).diagnostic.is_none()
    {
        // Projection resolution already supplied a concrete error, not an invented P.
        report.record(
            Bodies,
            CheckStatus::Incomplete,
            Some("missing selected projection configuration"),
        );
    }
}

fn record_ce_success(
    report: &mut ValidationReport,
    height: u64,
    ce: &outbe_compressed_entities::CeAuditReport,
) {
    report.record(CheckName::Ce, CheckStatus::Passed, None);
    report.retained_ranges.push(RetainedRange {
        domain: "ce".into(),
        start: height,
        end_inclusive: height,
    });
    record_count(report, "ce_trees", ce.trees);
    record_count(report, "ce_leaves", ce.leaves);
}
