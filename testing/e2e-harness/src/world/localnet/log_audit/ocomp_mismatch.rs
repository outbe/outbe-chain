//! The exact two-sink shutdown bundle of an expected OCOMP FullNode mismatch.

use std::collections::BTreeSet;
use std::path::PathBuf;

use alloy_primitives::B256;

use super::discovery::{contains_nonfatal_alarm, runtime_sink};
use super::LogRecords;

/// One expected bundle record: the process start it belongs to, its exact
/// message, and whether the result-digest trailer must follow it.
type BundleRecord = (usize, String, bool);

pub(super) fn expected_ocomp_full_node_mismatch_records(
    logs: &[(PathBuf, String)],
    validators: usize,
    job_id: B256,
) -> Result<LogRecords, String> {
    let mut accepted = BTreeSet::new();
    let mut sinks = BTreeSet::new();
    let mut result_digests = None;
    for (path, content) in logs {
        let Some(sink) = runtime_sink(path, validators.saturating_add(1), validators) else {
            continue;
        };
        let malformed = || {
            format!(
                "malformed expected OCOMP FullNode mismatch shutdown bundle in {}",
                path.display()
            )
        };
        let lines = content.lines().collect::<Vec<_>>();
        let digests = mismatch_digests(&lines, job_id).ok_or_else(malformed)?;
        if digests.0 == digests.1 || result_digests.is_some_and(|pair| pair != digests) {
            return Err(malformed());
        }
        result_digests = Some(digests);
        let expected = expected_mismatch_bundle(sink, job_id, digests);
        let records = match_mismatch_bundle(&lines, &expected, digests).ok_or_else(malformed)?;
        if !sinks.insert(sink) {
            return Err(format!(
                "duplicate expected OCOMP FullNode mismatch shutdown sink {sink}"
            ));
        }
        accepted.extend(records.into_iter().map(|index| (path.clone(), index)));
    }

    if sinks != BTreeSet::from(["node.log", "reth.log"]) {
        return Err(format!(
            "expected one node.log and one reth.log OCOMP FullNode mismatch shutdown bundle for job {job_id:#x}, observed {sinks:?}"
        ));
    }
    Ok(accepted)
}

/// The single `(local, canonical)` result pair the sink reports for `job_id`.
fn mismatch_digests(lines: &[&str], job_id: B256) -> Option<(B256, B256)> {
    let prefix = format!(
        "ERROR outbe_chain::launch::node: embedded OCOMP requested node shutdown failure_class=Other failure=embedded OCOMP job {job_id:#x}: local result "
    );
    let pairs = lines
        .iter()
        .filter_map(|line| {
            let (_, detail) = line.split_once(&prefix)?;
            let (local, canonical) = detail
                .trim_end()
                .split_once(" differs from canonical result ")?;
            Some((local.parse::<B256>().ok()?, canonical.parse::<B256>().ok()?))
        })
        .collect::<Vec<_>>();
    let [pair] = pairs.as_slice() else {
        return None;
    };
    Some(*pair)
}

fn expected_mismatch_bundle(
    sink: &str,
    job_id: B256,
    (local, canonical): (B256, B256),
) -> Vec<BundleRecord> {
    let detail = format!("local result {local:#x} differs from canonical result {canonical:#x}");
    let mismatch = format!("embedded OCOMP job {job_id:#x}: {detail}");
    let sticky = format!("embedded OCOMP persisted fatal evidence: job_id={job_id:#x}");
    let recovery = format!("OCOMP recovery readiness failed (Other): {sticky}");

    // Each sink spans the initial mismatch exit and one sticky-fatal restart.
    // Only stderr (node.log) receives the returned error and its cause.
    let mut expected = vec![
        (1, format!("ERROR exex{{id=\"outbe-finalized\"}}: outbe_chain::ocomp_exex::failure: embedded OCOMP requested local node shutdown job_id={job_id:#x} detail={detail}"), false),
        (1, format!("ERROR outbe_chain::launch::node: embedded OCOMP requested node shutdown failure_class=Other failure={mismatch}"), false),
    ];
    if sink == "node.log" {
        expected.push((
            1,
            format!("Error: embedded OCOMP failure (Other): {mismatch}"),
            false,
        ));
    }
    expected.extend([
        (2, format!("ERROR outbe_chain::launch::node: embedded OCOMP requested node shutdown failure_class=Other failure={sticky}"), true),
        (2, format!("ERROR outbe_chain::launch::node::consensus: consensus stack failed during shutdown error={recovery}"), true),
    ]);
    if sink == "node.log" {
        expected.extend([
            (2, format!("Error: additional shutdown failure: consensus task exited with error: {recovery}"), true),
            (2, format!("embedded OCOMP failure (Other): {sticky}"), true),
        ]);
    }
    expected
}

/// Line indices of the complete, in-order bundle, or `None` when it differs.
fn match_mismatch_bundle(
    lines: &[&str],
    expected: &[BundleRecord],
    digests: (B256, B256),
) -> Option<Vec<usize>> {
    let mut starts = 0;
    let mut records = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.contains("reth::cli: Starting Reth version=") {
            starts += 1;
        }
        let Some(position) = expected
            .iter()
            .position(|(_, message, _)| bundle_line_matches(line, message))
        else {
            continue;
        };
        let (expected_start, _, multiline) = &expected[position];
        if starts != *expected_start
            || (*multiline && !digest_trailer_follows(lines, index, digests))
        {
            return None;
        }
        records.push((index, position));
    }
    let in_order = records
        .iter()
        .map(|(_, position)| *position)
        .collect::<Vec<_>>()
        == (0..expected.len()).collect::<Vec<_>>();
    (starts == 2 && in_order).then(|| records.into_iter().map(|(index, _)| index).collect())
}

fn bundle_line_matches(line: &str, message: &str) -> bool {
    if message.starts_with("ERROR ") {
        line.split_once("ERROR ")
            .is_some_and(|(_, body)| body.trim_end() == &message[6..])
            && !contains_nonfatal_alarm(&line.to_ascii_lowercase())
    } else {
        line.trim() == message
    }
}

fn digest_trailer_follows(lines: &[&str], index: usize, (local, canonical): (B256, B256)) -> bool {
    let trimmed = |offset: usize| lines.get(index + offset).map(|line| line.trim());
    trimmed(1) == Some(format!("local_result_digest={local:#x}").as_str())
        && trimmed(2) == Some(format!("canonical_result_digest={canonical:#x}").as_str())
}
