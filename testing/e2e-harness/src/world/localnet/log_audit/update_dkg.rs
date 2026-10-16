//! Expected unsupported-update fatals and DKG share reveals, per validator.

use std::path::Path;

use super::discovery::{contains_nonfatal_alarm, validator_node_log_index};
use super::AuditExpectations;

/// Per-validator tallies of the declared update-fatal and DKG-reveal records.
pub(super) struct ExpectationTally<'a> {
    update_fragment: Option<String>,
    dkg_reveal: Option<&'a str>,
    updates_by_validator: Vec<usize>,
    reveals_by_validator: Vec<usize>,
}

impl<'a> ExpectationTally<'a> {
    pub(super) fn new(expectations: &AuditExpectations<'a>) -> Self {
        let slots = expectations.validators.min(1_024);
        Self {
            update_fragment: expectations
                .unsupported_version
                .map(unsupported_version_fragment),
            dkg_reveal: expectations.expected_dkg_reveal,
            updates_by_validator: vec![0; slots],
            reveals_by_validator: vec![0; slots],
        }
    }

    pub(super) fn accept_update(&mut self, path: &Path, line: &str) -> bool {
        self.update_fragment.as_deref().is_some_and(|fragment| {
            accept_expected_update_fatal(path, line, fragment, &mut self.updates_by_validator)
        })
    }

    pub(super) fn accept_reveal(&mut self, path: &Path, line: &str) -> bool {
        self.dkg_reveal.is_some_and(|public_key| {
            accept_expected_dkg_reveal(path, line, public_key, &mut self.reveals_by_validator)
        })
    }

    pub(super) fn report_missing(self, findings: &mut Vec<String>) {
        // A rejected proposal never becomes a block every replica must execute.
        // Require an exact rejection in the committee, not a local payload-build
        // attempt on every node. The scenario separately checks current-launch
        // rejection and the unchanged version/schedule/height on every validator.
        if self.update_fragment.is_some()
            && self.updates_by_validator.iter().all(|count| *count == 0)
        {
            findings.push(
                "committee node logs: expected unsupported-version fatal is absent".to_owned(),
            );
        }
        if self.dkg_reveal.is_none() {
            return;
        }
        for (validator, count) in self.reveals_by_validator.into_iter().enumerate() {
            if count == 0 {
                findings.push(format!(
                    "validator-{validator}/node.log: expected exact DKG share reveal is absent"
                ));
            }
        }
    }
}

fn accept_expected_update_fatal(
    path: &Path,
    line: &str,
    expected_fragment: &str,
    expected_by_validator: &mut [usize],
) -> bool {
    if !exact_expected_update_fatal(line, expected_fragment) {
        return false;
    }
    if let Some(validator) = validator_node_log_index(path, expected_by_validator.len()) {
        expected_by_validator[validator] += 1;
    }
    true
}

fn accept_expected_dkg_reveal(
    path: &Path,
    line: &str,
    expected_public_key: &str,
    expected_by_validator: &mut [usize],
) -> bool {
    if !exact_expected_dkg_reveal(line, expected_public_key) {
        return false;
    }
    if let Some(validator) = validator_node_log_index(path, expected_by_validator.len()) {
        expected_by_validator[validator] += 1;
    }
    true
}

fn exact_expected_dkg_reveal(line: &str, expected_public_key: &str) -> bool {
    let line = line.to_ascii_lowercase();
    let exact_reveal = line
        .matches("dkg: a validator's individual share was revealed")
        .count()
        == 1
        && line.contains("revealed_validator")
        && line.contains(&expected_public_key.to_ascii_lowercase());
    let alarm = [
        "fatal",
        "panic",
        "eagain",
        "resource temporarily unavailable",
    ]
    .iter()
    .any(|marker| line.contains(marker))
        || (line.contains("vrf") && line.contains("alarm"))
        || (line.contains("projection") && line.contains("fatal"));
    exact_reveal && !alarm
}

pub(super) fn unsupported_version_fragment(version: u64) -> String {
    format!(
        "cannot activate protocol version v{}.{} ({version}): binary supports at most v",
        version >> 24,
        version & 0x00ff_ffff
    )
}

pub(super) fn exact_expected_update_fatal(line: &str, expected_fragment: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.matches("fatal").count() == 1
        && line.matches("cannot activate protocol version").count() == 1
        && line.contains(expected_fragment)
        && !contains_nonfatal_alarm(&line)
}
