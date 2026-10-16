//! Node-state probes: datadir moves and node-log inspection used by the
//! recovery/promotion scenarios.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output};
use std::time::{Duration, Instant};

use eyre::{ensure, Result, WrapErr};
use serde::Serialize;

use crate::internal::proc::{first_hex, run_capture};

use super::Localnet;

mod header_capture;

use header_capture::{capture_header_command_until, stopped_header_command};
#[cfg(test)]
use header_capture::{header_capture_timeout, header_stream_before, HeaderCaptureTimeout};

fn validator_slot_node_log_path(root: &Path, slot: usize) -> PathBuf {
    root.join(format!("validator-{slot}")).join("node.log")
}

fn read_required_node_log(path: &Path, node: &str) -> Result<String> {
    fs::read_to_string(path).wrap_err_with(|| {
        format!(
            "read OCOMP runtime trace for {node} from owned log {}",
            path.display()
        )
    })
}

fn has_required_signing_share(node_dir: &Path) -> Result<bool> {
    let mut found = false;
    for path in [
        node_dir.join("keys/dkg_share.hex"),
        node_dir.join("data/keys/dkg_share.hex"),
    ] {
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).wrap_err_with(|| format!("inspect share {}", path.display()));
            }
            Ok(metadata) => ensure!(
                metadata.is_file(),
                "share is not a regular file: {}",
                path.display()
            ),
        }
        outbe_consensus::bls::load_signing_share(
            &path,
            &outbe_consensus::bls::KeyBackend::Plaintext,
        )
        .wrap_err_with(|| format!("decode existing share {}", path.display()))?;
        found = true;
    }
    Ok(found)
}

/// One structured OCOMP execution-boundary marker parsed from a node's real
/// runtime log. The producer emits only consensus-observational fields. This
/// probe never infers behavior from source code or mutates node state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompRuntimeTraceMarkerV1 {
    pub node: String,
    pub kind: String,
    pub origin: Option<String>,
    pub block_number: u64,
}

impl Localnet {
    pub(crate) fn scenario_id(&self) -> usize {
        self.cfg.scenario
    }

    pub(crate) fn scenario_dir(&self) -> &Path {
        &self.cfg.dir
    }

    fn node_log(&self, node: &str) -> Result<String> {
        let path = self.cfg.dir.join(node).join("node.log");
        read_required_node_log(&path, node)
    }

    /// Parse bounded OCOMP runtime markers for one exact owned node.
    pub fn ocomp_runtime_trace_markers(
        &self,
        node: &str,
    ) -> Result<Vec<OcompRuntimeTraceMarkerV1>> {
        ensure!(
            node.strip_prefix("validator-")
                .and_then(|index| index.parse::<usize>().ok())
                .is_some_and(|index| index < self.committee_size()),
            "OCOMP trace probe refuses unknown node {node}"
        );
        let path = self.cfg.dir.join(node).join("node.log");
        self.ocomp_runtime_trace_markers_from_path(node, &path)
    }

    /// Parse OCOMP runtime markers for a named FullNode whose owned data lives
    /// in one allocated validator slot rather than a directory named after the
    /// process. The slot is the storage identity used by
    /// `launch_dcap_full_node`. The display name only owns the child process.
    pub fn ocomp_runtime_trace_markers_at_validator_slot(
        &self,
        node: &str,
        slot: usize,
    ) -> Result<Vec<OcompRuntimeTraceMarkerV1>> {
        ensure!(
            self.followers.contains_key(node),
            "OCOMP trace probe refuses unowned follower {node}"
        );
        ensure!(
            slot >= self.committee_size(),
            "OCOMP follower trace slot {slot} overlaps the active committee"
        );
        let path = validator_slot_node_log_path(&self.cfg.dir, slot);
        self.ocomp_runtime_trace_markers_from_path(node, &path)
    }

    fn ocomp_runtime_trace_markers_from_path(
        &self,
        node: &str,
        path: &Path,
    ) -> Result<Vec<OcompRuntimeTraceMarkerV1>> {
        let log = read_required_node_log(path, node)?;
        let mut markers = Vec::new();
        for line in log.lines() {
            let Some(payload) = line.split_once("OCOMP_TRACE_V1 ").map(|(_, value)| value) else {
                continue;
            };
            let fields = payload
                .split_whitespace()
                .filter_map(|field| field.split_once('='))
                .collect::<std::collections::BTreeMap<_, _>>();
            let kind = fields
                .get("kind")
                .ok_or_else(|| eyre::eyre!("OCOMP trace marker has no kind in {node}"))?
                .trim_matches(|character: char| {
                    !character.is_ascii_alphanumeric() && character != '_'
                })
                .to_owned();
            let block_number = fields
                .get("block")
                .ok_or_else(|| eyre::eyre!("OCOMP trace marker has no block in {node}"))?
                .trim_matches(|character: char| !character.is_ascii_digit())
                .parse::<u64>()
                .wrap_err_with(|| format!("decode OCOMP trace block in {node}"))?;
            let origin = fields.get("origin").map(|value| {
                value
                    .trim_matches(|character: char| {
                        !character.is_ascii_alphanumeric() && character != '_'
                    })
                    .to_owned()
            });
            markers.push(OcompRuntimeTraceMarkerV1 {
                node: node.to_owned(),
                kind,
                origin,
                block_number,
            });
        }
        ensure!(
            markers.len() <= 4_096,
            "OCOMP trace marker count exceeds the bounded per-node evidence cap"
        );
        Ok(markers)
    }

    /// Whether validator `index`'s owned node process has already exited.
    pub fn validator_exited(&mut self, index: usize) -> bool {
        match self.validators.get_mut(&index) {
            Some(guard) => guard.exited(),
            None => true,
        }
    }

    /// Observe/reap the exact retained child, without signalling it or treating
    /// a missing owner as an expected process exit.
    pub(crate) fn owned_validator_process(
        &mut self,
        index: usize,
    ) -> Result<(u32, Option<ExitStatus>)> {
        let guard = self
            .validators
            .get_mut(&index)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no owned process"))?;
        Ok((guard.pid(), guard.exit_status()?))
    }

    /// Read evidence only after this exact validator has exited. No node
    /// restart, database initialization or latest/head fallback is permitted.
    pub(crate) fn stopped_validator_header_output(
        &mut self,
        index: usize,
        expected_pid: u32,
        height: u64,
        deadline: Instant,
    ) -> Result<Output> {
        let deadline = self.scenario_deadline.map_or(deadline, |outer| {
            deadline.min(outer.checked_sub(Duration::from_secs(20)).unwrap_or(outer))
        });
        let data = self.cfg.validator_dir(index).join("data");
        let guard = self
            .validators
            .get_mut(&index)
            .ok_or_else(|| eyre::eyre!("validator-{index} has no retained process"))?;
        ensure!(
            guard.pid() == expected_pid && guard.owns_node_data_dir(&data),
            "canonical header read does not match the owned validator-{index} PID/datadir"
        );
        ensure!(
            guard.exit_status()?.is_some(),
            "validator-{index} is still running during offline header read"
        );
        let command = stopped_header_command(
            &self.cfg.bin_chain,
            &data,
            &self.cfg.dir.join("genesis.json"),
            height,
        )?;
        let output = capture_header_command_until(command, deadline).wrap_err_with(|| {
            format!(
                "read stopped validator-{index} canonical header h{height} from {}",
                data.display()
            )
        })?;
        let (pid, status) = self.owned_validator_process(index)?;
        ensure!(
            pid == expected_pid && status.is_some(),
            "canonical header reader lost its stopped validator owner"
        );
        Ok(output)
    }

    /// Whether validator `index`'s log contains `needle` (`e2e_joiner_log_has`).
    pub fn log_has(&self, index: usize, needle: &str) -> Result<bool> {
        Ok(self
            .node_log(&format!("validator-{index}"))?
            .contains(needle))
    }

    /// Count of log LINES containing `needle` (matches shell `grep -c`).
    pub fn log_count(&self, index: usize, needle: &str) -> Result<usize> {
        Ok(self
            .node_log(&format!("validator-{index}"))?
            .lines()
            .filter(|l| l.contains(needle))
            .count())
    }

    /// First runtime-log line containing `needle`, including its path and line.
    ///
    /// Recovery waits use this to fail immediately on a deterministic boundary
    /// rejection instead of polling an on-chain state that can no longer change.
    pub fn first_runtime_log_line_containing(&self, needle: &str) -> Result<Option<String>> {
        super::log_audit::first_runtime_log_line_containing(&self.cfg.dir, needle)
    }

    /// Whether validator `index`'s enclave log contains `needle`.
    pub fn enclave_log_has(&self, index: usize, needle: &str) -> Result<bool> {
        let path = self.cfg.validator_dir(index).join("enclave.log");
        let log = fs::read_to_string(&path)
            .wrap_err_with(|| format!("read validator-{index} enclave log {}", path.display()))?;
        Ok(log.contains(needle))
    }

    /// The `--consensus.keys-dir` for validator `index` (persisted-share restart).
    pub fn keys_dir(&self, index: usize) -> String {
        self.cfg
            .validator_dir(index)
            .join("keys")
            .display()
            .to_string()
    }

    /// Consensus BLS public key derived from this validator's provisioned
    /// signing key, in the exact lowercase hex form used by DKG reveal alarms.
    pub fn consensus_public_key(&self, index: usize) -> Result<String> {
        let signing_key = self.cfg.validator_dir(index).join("signing-key.hex");
        let output = run_capture(
            &self.cfg.bin_keygen,
            &["show-pubkey", "--key", &signing_key.display().to_string()],
        )?;
        first_hex(&output, 96).ok_or_else(|| eyre::eyre!("no BLS public key from keygen"))
    }

    /// Whether a valid durable DKG signing share exists in validator `index`'s
    /// keys dir.
    ///
    /// Joiner restart scenarios pass an explicit `<node>/keys` override, while
    /// the ordinary FullNode-to-validator flow uses the product default under
    /// `<datadir>/data/keys`. Both are canonical `--consensus.keys-dir` layouts.
    pub fn has_share_file(&self, index: usize) -> bool {
        let node_dir = self.cfg.validator_dir(index);
        [
            node_dir.join("keys/dkg_share.hex"),
            node_dir.join("data/keys/dkg_share.hex"),
        ]
        .into_iter()
        .any(|path| {
            outbe_consensus::bls::load_signing_share(
                &path,
                &outbe_consensus::bls::KeyBackend::Plaintext,
            )
            .is_ok()
        })
    }

    /// Absence is distinct from an unreadable or malformed existing share.
    pub(crate) fn has_share_file_result(&self, index: usize) -> Result<bool> {
        has_required_signing_share(&self.cfg.validator_dir(index))
    }
}

#[cfg(test)]
mod tests;
