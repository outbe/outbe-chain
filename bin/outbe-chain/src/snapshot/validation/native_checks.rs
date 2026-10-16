//! Native semantic check execution (Headers and EVM).

use super::{
    report::{
        CheckName, CheckStatus, InventoryBounds, RequiredHeight, RetainedRange, ValidationReport,
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
    let expected_kind = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !expected_kind {
        return Err(crate::errors::SnapshotError::SourceWrongFileKind {
            path: path.display().to_string(),
        }
        .into());
    }
    Ok(())
}

fn record_count(report: &mut ValidationReport, name: &str, count: u64) {
    report.inventory_bounds.push(InventoryBounds {
        name: name.into(),
        start: 0,
        end_exclusive: count,
        visited: count,
    });
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

pub(super) fn run_native_checks(
    layout: &crate::snapshot::config::RequestedLayout,
    selection: &CheckSelection,
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
            fail_selected(report, &[Headers, Evm], &error);
            return;
        }
    };
    report.observed.h = Some(view.progress.finalized.clone());
    report.observed.e = Some(view.progress.execution.clone());
    let mut required = vec![
        view.progress.finalized.number,
        view.progress.execution.number,
    ];
    required.sort_unstable();
    required.dedup();
    let Some(_) = verify_and_record_headers(&view, &required, report) else {
        block_on(report, Evm, Headers);
        return;
    };
    // Retained structure was verified. Missing anchors remain explicit in the
    // header report. Each independent check requires its own exact header.
    verify_and_record_evm(&view, selection, scratch, report);
}
