//! Node-state probes: datadir moves and node-log inspection used by the
//! recovery/promotion scenarios.

use std::fs;
#[cfg(any(test, feature = "ocomp-integration"))]
use std::fs::File;
#[cfg(feature = "ocomp-integration")]
use std::io::{BufRead as _, BufReader};
#[cfg(any(test, feature = "ocomp-integration"))]
use std::io::{Read as _, Seek as _, SeekFrom};
#[cfg(any(test, feature = "ocomp-integration"))]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
#[cfg(feature = "ocomp-integration")]
use std::thread::sleep;
#[cfg(feature = "ocomp-integration")]
use std::time::{Duration, Instant};

use alloy_primitives::B256;
use eyre::{bail, ensure, Result, WrapErr};
use serde::Serialize;
use serde_json::{json, Value};
#[cfg(any(test, feature = "ocomp-integration"))]
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::internal::proc::{first_hex, run_capture};
use crate::internal::shell::Sh;

use super::Localnet;

/// One successful production startup-recovery span observed from a validator.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CeStartupReplayObservationV1 {
    pub validator_index: u8,
    pub first_missing_block_number: u64,
    pub target_block_number: u64,
    pub target_block_hash: B256,
    pub replayed_block_count: u64,
    pub elapsed_micros: u64,
}

/// One structured OCOMP execution-boundary marker parsed from a node's real
/// runtime log. The producer emits only consensus-observational fields; this
/// probe never infers behavior from source code or mutates node state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OcompRuntimeTraceMarkerV1 {
    pub node: String,
    pub kind: String,
    pub origin: Option<String>,
    pub block_number: u64,
}

#[cfg(any(test, feature = "ocomp-integration"))]
struct TrackedRethLog {
    file: File,
    offset: u64,
    anchor: Vec<u8>,
    pending_line: Vec<u8>,
}

#[cfg(any(test, feature = "ocomp-integration"))]
const RETH_LOG_ANCHOR_BYTES: u64 = 64;
#[cfg(any(test, feature = "ocomp-integration"))]
const MAX_RETH_LOG_LINE_BYTES: usize = 256 * 1024;
#[cfg(any(test, feature = "ocomp-integration"))]
const MAX_RETH_LOG_POLL_BYTES: usize = 4 * 1024 * 1024;
#[cfg(any(test, feature = "ocomp-integration"))]
const MAX_TRACKED_RETH_LOG_FILES: usize = 16;

#[cfg(any(test, feature = "ocomp-integration"))]
struct RethLogTail {
    root: PathBuf,
    files: std::collections::BTreeMap<(u64, u64), TrackedRethLog>,
}

#[cfg(any(test, feature = "ocomp-integration"))]
impl RethLogTail {
    fn at_end(root: &Path) -> Result<Self> {
        let mut tail = Self {
            root: root.to_path_buf(),
            files: std::collections::BTreeMap::new(),
        };
        tail.refresh(true)?;
        Ok(tail)
    }

    fn read_new_complete_lines(&mut self) -> Result<String> {
        self.refresh(false)?;
        let mut output = String::new();
        for tracked in self.files.values_mut() {
            let length = tracked
                .file
                .metadata()
                .wrap_err("inspect tracked Reth runtime log")?
                .len();
            let reset = if length < tracked.offset {
                true
            } else {
                read_reth_log_anchor(&mut tracked.file, tracked.offset)? != tracked.anchor
            };
            if reset {
                tracked.offset = 0;
                tracked.anchor.clear();
                tracked.pending_line.clear();
            }
            tracked
                .file
                .seek(SeekFrom::Start(tracked.offset))
                .wrap_err("seek tracked Reth runtime log")?;
            let mut bytes = Vec::new();
            (&mut tracked.file)
                .take(
                    u64::try_from(MAX_RETH_LOG_POLL_BYTES + 1)
                        .expect("fixed Reth log poll cap fits u64"),
                )
                .read_to_end(&mut bytes)
                .wrap_err("read tracked Reth runtime log")?;
            ensure!(
                bytes.len() <= MAX_RETH_LOG_POLL_BYTES,
                "Reth runtime log grew by more than {MAX_RETH_LOG_POLL_BYTES} bytes in one poll"
            );
            tracked.offset = tracked
                .offset
                .checked_add(
                    u64::try_from(bytes.len()).wrap_err("Reth log suffix length exceeds u64")?,
                )
                .ok_or_else(|| eyre::eyre!("Reth log cursor overflow"))?;
            tracked.anchor = read_reth_log_anchor(&mut tracked.file, tracked.offset)?;
            tracked.pending_line.extend_from_slice(&bytes);
            let Some(complete_len) = tracked
                .pending_line
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map(|index| index + 1)
            else {
                ensure!(
                    tracked.pending_line.len() <= MAX_RETH_LOG_LINE_BYTES,
                    "Reth runtime log contains an over-cap incomplete line"
                );
                continue;
            };
            ensure!(
                complete_len <= MAX_RETH_LOG_POLL_BYTES
                    && tracked.pending_line.len() - complete_len <= MAX_RETH_LOG_LINE_BYTES,
                "Reth runtime log poll or incomplete line exceeds its bounded evidence cap"
            );
            ensure!(
                output
                    .len()
                    .checked_add(complete_len)
                    .is_some_and(|length| length <= MAX_RETH_LOG_POLL_BYTES),
                "combined Reth runtime log poll exceeds its bounded evidence cap"
            );
            output.push_str(
                std::str::from_utf8(&tracked.pending_line[..complete_len])
                    .wrap_err("Reth runtime log suffix is not UTF-8")?,
            );
            tracked.pending_line.drain(..complete_len);
        }
        Ok(output)
    }

    fn refresh(&mut self, seed_at_end: bool) -> Result<()> {
        let paths = reth_log_paths(&self.root)?;
        ensure!(
            paths.len() <= MAX_TRACKED_RETH_LOG_FILES,
            "Reth runtime log file count exceeds {MAX_TRACKED_RETH_LOG_FILES}"
        );
        for path in paths {
            let file = File::open(&path)
                .wrap_err_with(|| format!("open validator runtime log {}", path.display()))?;
            let metadata = file
                .metadata()
                .wrap_err_with(|| format!("inspect validator runtime log {}", path.display()))?;
            let identity = (metadata.dev(), metadata.ino());
            if let std::collections::btree_map::Entry::Vacant(entry) = self.files.entry(identity) {
                let offset = if seed_at_end { metadata.len() } else { 0 };
                entry.insert(TrackedRethLog {
                    anchor: read_reth_log_anchor_from_file(&file, offset)?,
                    file,
                    offset,
                    pending_line: Vec::new(),
                });
            }
        }
        ensure!(
            self.files.len() <= MAX_TRACKED_RETH_LOG_FILES,
            "tracked Reth runtime log file count exceeds {MAX_TRACKED_RETH_LOG_FILES}"
        );
        Ok(())
    }
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn read_reth_log_anchor(file: &mut File, offset: u64) -> Result<Vec<u8>> {
    let start = offset.saturating_sub(RETH_LOG_ANCHOR_BYTES);
    file.seek(SeekFrom::Start(start))
        .wrap_err("seek Reth runtime log anchor")?;
    let length = usize::try_from(offset - start).expect("fixed Reth log anchor fits usize");
    let mut anchor = vec![0_u8; length];
    file.read_exact(&mut anchor)
        .wrap_err("read Reth runtime log anchor")?;
    Ok(anchor)
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn read_reth_log_anchor_from_file(file: &File, offset: u64) -> Result<Vec<u8>> {
    let mut file = file.try_clone().wrap_err("clone Reth runtime log handle")?;
    read_reth_log_anchor(&mut file, offset)
}

impl Localnet {
    /// Audit every scenario log after its assertions and before teardown. The
    /// unsupported-update flow may emit only its exact compatibility fatal;
    /// unrelated fatals and all panic/DKG-share/VRF/SGX/projection alarms fail
    /// the scenario even when its functional steps passed.
    pub fn audit_unexpected_logs(
        &self,
        unsupported_version: Option<u64>,
        expected_dkg_reveal: Option<&str>,
    ) -> Result<LogAudit> {
        let mut logs = Vec::new();
        collect_logs(&self.cfg.dir, &mut logs)?;
        let mut findings = Vec::new();
        let expected_fragment = unsupported_version.map(|version| {
            format!(
                "cannot activate protocol version v{}.{} ({version}): binary supports at most v",
                version >> 24,
                version & 0x00ff_ffff
            )
        });
        let mut expected_by_validator = vec![0_usize; self.cfg.validators];
        let mut expected_reveal_by_validator = vec![0_usize; self.cfg.validators];
        let mut counts = LogCounts {
            runtime_log_files: logs.len(),
            ..LogCounts::default()
        };
        for path in logs {
            let content = fs::read_to_string(&path)
                .wrap_err_with(|| format!("read E2E log {}", path.display()))?;
            for (index, line) in content.lines().enumerate() {
                if expected_fragment.as_deref().is_some_and(|fragment| {
                    accept_expected_update_fatal(&path, line, fragment, &mut expected_by_validator)
                }) {
                    counts.expected_update_fatal += 1;
                    continue;
                }
                if expected_dkg_reveal.is_some_and(|public_key| {
                    accept_expected_dkg_reveal(
                        &path,
                        line,
                        public_key,
                        &mut expected_reveal_by_validator,
                    )
                }) {
                    counts.expected_dkg_reveal += 1;
                    continue;
                }
                counts.observe(line);
                if unexpected_log_line(line) {
                    findings.push(format!("{}:{}: {}", path.display(), index + 1, line));
                    if findings.len() == 20 {
                        break;
                    }
                }
            }
        }
        if expected_fragment.is_some() {
            for (validator, count) in expected_by_validator.into_iter().enumerate() {
                if count == 0 {
                    findings.push(format!(
                        "validator-{validator}/node.log: expected unsupported-version fatal is absent"
                    ));
                }
            }
        }
        if expected_dkg_reveal.is_some() {
            for (validator, count) in expected_reveal_by_validator.into_iter().enumerate() {
                if count == 0 {
                    findings.push(format!(
                        "validator-{validator}/node.log: expected exact DKG share reveal is absent"
                    ));
                }
            }
        }
        Ok(LogAudit { counts, findings })
    }

    pub(crate) fn scenario_id(&self) -> usize {
        self.cfg.scenario
    }

    pub(crate) fn scenario_dir(&self) -> &Path {
        &self.cfg.dir
    }

    /// Returns the real Reth processing duration for one exact canonical block
    /// on one validator. This observes the runtime record emitted by the block
    /// import path; it does not re-execute the block through debug RPC.
    #[cfg(feature = "ocomp-integration")]
    pub fn validator_block_processing_micros(
        &self,
        validator_index: usize,
        block_number: u64,
        block_hash: B256,
    ) -> Result<u64> {
        ensure!(
            validator_index < self.committee_size(),
            "validator index {validator_index} is outside the committee"
        );
        let mut observed = None;
        let root = self.cfg.validator_dir(validator_index).join("logs");
        for path in reth_log_paths(&root)? {
            let file = File::open(&path)
                .wrap_err_with(|| format!("open validator runtime log {}", path.display()))?;
            for line in BufReader::new(file).lines() {
                let line = line
                    .wrap_err_with(|| format!("read validator runtime log {}", path.display()))?;
                let Some(micros) =
                    parse_canonical_block_processing_micros(&line, block_number, block_hash)
                        .wrap_err_with(|| {
                            format!("parse canonical block record in {}", path.display())
                        })?
                else {
                    continue;
                };
                ensure!(
                    observed.replace(micros).is_none(),
                    "validator {validator_index} has multiple canonical block records for \
                     {block_number}/{block_hash:#x}"
                );
            }
        }
        observed.ok_or_else(|| {
            eyre::eyre!(
                "validator {validator_index} has no canonical block record for \
                 {block_number}/{block_hash:#x}"
            )
        })
    }

    /// Returns the observed wall-clock latency between canonical application
    /// and finalization acknowledgement for one exact block on one validator.
    #[cfg(feature = "ocomp-integration")]
    pub fn validator_finality_latency_micros(
        &self,
        validator_index: usize,
        block_number: u64,
        block_hash: B256,
    ) -> Result<u64> {
        ensure!(
            validator_index < self.committee_size(),
            "validator index {validator_index} is outside the committee"
        );
        let mut canonical_at = None;
        let mut finalized_at = None;
        let root = self.cfg.validator_dir(validator_index).join("logs");
        for path in reth_log_paths(&root)? {
            let file = File::open(&path)
                .wrap_err_with(|| format!("open validator runtime log {}", path.display()))?;
            for line in BufReader::new(file).lines() {
                let line = line
                    .wrap_err_with(|| format!("read validator runtime log {}", path.display()))?;
                if let Some(observed_at) =
                    parse_canonical_block_observed_at_micros(&line, block_number, block_hash)
                        .wrap_err_with(|| {
                            format!("parse canonical block timestamp in {}", path.display())
                        })?
                {
                    ensure!(
                        canonical_at.replace(observed_at).is_none(),
                        "validator {validator_index} has multiple canonical timestamps for \
                         {block_number}/{block_hash:#x}"
                    );
                }
                if let Some(observed_at) =
                    parse_finalized_block_observed_at_micros(&line, block_number, block_hash)
                        .wrap_err_with(|| {
                            format!("parse finalized block timestamp in {}", path.display())
                        })?
                {
                    ensure!(
                        finalized_at.replace(observed_at).is_none(),
                        "validator {validator_index} has multiple finalization timestamps for \
                         {block_number}/{block_hash:#x}"
                    );
                }
            }
        }
        let canonical_at = canonical_at.ok_or_else(|| {
            eyre::eyre!(
                "validator {validator_index} has no canonical timestamp for \
                 {block_number}/{block_hash:#x}"
            )
        })?;
        let finalized_at = finalized_at.ok_or_else(|| {
            eyre::eyre!(
                "validator {validator_index} has no finalization timestamp for \
                 {block_number}/{block_hash:#x}"
            )
        })?;
        let latency = finalized_at
            .checked_sub(canonical_at)
            .ok_or_else(|| eyre::eyre!("finalization timestamp precedes canonical application"))?;
        ensure!(latency > 0, "observed finality latency must be positive");
        Ok(latency)
    }

    /// Forces one validator to reconstruct CE from preserved canonical Reth
    /// history and returns the exact successful replay span emitted by the
    /// production startup gate.
    #[cfg(feature = "ocomp-integration")]
    pub fn reconstruct_validator_ce_from_canonical_history(
        &mut self,
        validator_index: usize,
    ) -> Result<CeStartupReplayObservationV1> {
        ensure!(
            validator_index < self.committee_size(),
            "validator index {validator_index} is outside the committee"
        );
        let root = self.cfg.validator_dir(validator_index).join("logs");
        let mut tail = RethLogTail::at_end(&root)?;
        self.restart_validator_after_ce_reset(validator_index)?;

        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let content = tail.read_new_complete_lines()?;
            let validator_index_u8 =
                u8::try_from(validator_index).wrap_err("validator index exceeds u8")?;
            let mut observed = None;
            for line in content.lines() {
                let Some(observation) = parse_ce_startup_replay(line, validator_index_u8)? else {
                    continue;
                };
                ensure!(
                    observed.replace(observation).is_none(),
                    "validator {validator_index} emitted multiple successful CE startup replay spans"
                );
            }
            if let Some(observation) = observed {
                return Ok(observation);
            }
            ensure!(
                Instant::now() < deadline,
                "validator {validator_index} emitted no successful CE startup replay span after \
                 reconstructing its test-owned CE database"
            );
            sleep(Duration::from_millis(100));
        }
    }

    /// Relocate a datadir under the data dir (warm promotion reuses synced state).
    pub fn move_datadir(&self, from_rel: &str, to_rel: &str) -> Result<()> {
        let from = self.cfg.dir.join(from_rel);
        let to = self.cfg.dir.join(to_rel);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::rename(&from, &to) {
            Ok(()) => Ok(()),
            Err(_) if self.cfg.sudo => {
                Sh::new(&self.cfg).sudo_best_effort(
                    "mv",
                    &[&from.display().to_string(), &to.display().to_string()],
                );
                Ok(())
            }
            Err(e) => Err(e).wrap_err("move datadir"),
        }
    }

    fn node_log(&self, node: &str) -> String {
        let path = self.cfg.dir.join(node).join("node.log");
        fs::read_to_string(path).unwrap_or_default()
    }

    /// Parse bounded OCOMP runtime markers for one exact owned node.
    pub fn ocomp_runtime_trace_markers(
        &self,
        node: &str,
    ) -> Result<Vec<OcompRuntimeTraceMarkerV1>> {
        ensure!(
            node == "follower"
                || node
                    .strip_prefix("validator-")
                    .and_then(|index| index.parse::<usize>().ok())
                    .is_some_and(|index| index < self.committee_size()),
            "OCOMP trace probe refuses unknown node {node}"
        );
        let log = self.node_log(node);
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

    /// Decode the exact validator-owned OCOMP pin journal using the production
    /// bounded codec. This is read-only evidence, not a state-injection seam.
    #[cfg(feature = "ocomp-integration")]
    pub fn ocomp_retention_journal(
        &self,
        validator_index: usize,
    ) -> Result<outbe_node::ocomp::retention::RetentionJournalSnapshotV1> {
        ensure!(
            validator_index < self.committee_size(),
            "retention journal validator is outside the committee"
        );
        let root = self
            .cfg
            .validator_dir(validator_index)
            .join("data/consensus/ocomp_retention");
        outbe_node::ocomp::retention::inspect_retention_journal(&root)
            .wrap_err_with(|| format!("inspect production retention journal {}", root.display()))
    }

    /// Whether validator `index`'s owned node process has already exited.
    pub fn validator_exited(&mut self, index: usize) -> bool {
        match self.validators.get_mut(&index) {
            Some(guard) => guard.exited(),
            None => true,
        }
    }

    /// Whether validator `index`'s log contains `needle` (`e2e_joiner_log_has`).
    pub fn log_has(&self, index: usize, needle: &str) -> bool {
        self.node_log(&format!("validator-{index}"))
            .contains(needle)
    }

    /// Count of log LINES containing `needle` (matches shell `grep -c`).
    pub fn log_count(&self, index: usize, needle: &str) -> usize {
        self.node_log(&format!("validator-{index}"))
            .lines()
            .filter(|l| l.contains(needle))
            .count()
    }

    /// Whether validator `index`'s enclave log contains `needle`.
    pub fn enclave_log_has(&self, index: usize, needle: &str) -> bool {
        let path = self.cfg.validator_dir(index).join("enclave.log");
        fs::read_to_string(path)
            .unwrap_or_default()
            .contains(needle)
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

    /// Whether a durable DKG share file exists in validator `index`'s keys dir
    /// (`e2e_assert "DKG share persisted to keys-dir"`, s4:28-29).
    pub fn has_share_file(&self, index: usize) -> bool {
        let dir = self.cfg.validator_dir(index).join("keys");
        fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(Result::ok).any(|e| {
                    let n = e.file_name().to_string_lossy().to_lowercase();
                    (n.contains("dkg") && n.contains("share")) || n == "dkg_share.hex"
                })
            })
            .unwrap_or(false)
    }
}

#[derive(Debug, Default)]
struct LogCounts {
    runtime_log_files: usize,
    fatal: usize,
    panic: usize,
    dkg_share_reveal: usize,
    vrf_alarm: usize,
    sgx_resource_exhaustion: usize,
    projection_fatal: usize,
    expected_update_fatal: usize,
    expected_dkg_reveal: usize,
}

impl LogCounts {
    fn observe(&mut self, line: &str) {
        let line = line.to_ascii_lowercase();
        self.fatal += usize::from(line.contains("fatal"));
        self.panic += usize::from(line.contains("panic"));
        self.dkg_share_reveal += usize::from(
            line.contains("dkg")
                && line.contains("share")
                && (line.contains("reveal") || line.contains("disclos")),
        );
        self.vrf_alarm += usize::from(line.contains("vrf") && line.contains("alarm"));
        self.sgx_resource_exhaustion += usize::from(
            line.contains("eagain") || line.contains("resource temporarily unavailable"),
        );
        self.projection_fatal += usize::from(line.contains("projection") && line.contains("fatal"));
    }

    fn json(&self) -> Value {
        json!({
            "runtime_log_files": self.runtime_log_files,
            "fatal": self.fatal,
            "panic": self.panic,
            "dkg_share_reveal": self.dkg_share_reveal,
            "vrf_alarm": self.vrf_alarm,
            "sgx_resource_exhaustion": self.sgx_resource_exhaustion,
            "projection_fatal": self.projection_fatal,
            "expected_update_fatal": self.expected_update_fatal,
            "expected_dkg_reveal": self.expected_dkg_reveal,
        })
    }
}

/// Structured result retained as scenario evidence before logs are removed.
#[derive(Debug)]
pub struct LogAudit {
    counts: LogCounts,
    findings: Vec<String>,
}

impl LogAudit {
    pub(crate) fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    pub(crate) fn json(&self) -> Value {
        json!({
            "clean": self.is_clean(),
            "counts": self.counts.json(),
            "findings": self.findings,
        })
    }

    pub(crate) fn ensure_clean(&self) -> Result<()> {
        if !self.findings.is_empty() {
            bail!(
                "unexpected fatal/alarm log records:\n{}",
                self.findings.join("\n")
            );
        }
        Ok(())
    }
}

fn collect_logs(dir: &Path, logs: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).wrap_err_with(|| format!("scan {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_logs(&path, logs)?;
        } else if is_runtime_log(&path) {
            logs.push(path);
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn reth_log_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut logs = Vec::new();
    collect_reth_logs(root, &mut logs)?;
    logs.sort();
    logs.dedup();
    ensure!(
        !logs.is_empty(),
        "no Reth runtime log found under {}",
        root.display()
    );
    Ok(logs)
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn collect_reth_logs(dir: &Path, logs: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).wrap_err_with(|| format!("scan {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_reth_logs(&path, logs)?;
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "reth.log" || name.starts_with("reth.log."))
        {
            logs.push(path);
        }
    }
    Ok(())
}

fn is_runtime_log(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("node.log" | "enclave.log" | "reth.log")
    )
}

fn validator_node_log_index(path: &Path, validators: usize) -> Option<usize> {
    if path.file_name()?.to_str()? != "node.log" {
        return None;
    }
    let validator = path.parent()?.file_name()?.to_str()?;
    let index = validator.strip_prefix("validator-")?.parse().ok()?;
    (index < validators).then_some(index)
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
    line.matches("dkg: a validator's individual share was revealed")
        .count()
        == 1
        // `node.log` keeps tracing's ANSI formatting around the `=` separator,
        // while `reth.log` is plain text. Match the field name and its exact
        // value independently so both representations prove the same reveal.
        && line.contains("revealed_validator")
        && line.contains(&expected_public_key.to_ascii_lowercase())
        && !line.contains("fatal")
        && !line.contains("panic")
        && !(line.contains("vrf") && line.contains("alarm"))
        && !line.contains("eagain")
        && !line.contains("resource temporarily unavailable")
        && !(line.contains("projection") && line.contains("fatal"))
}

fn exact_expected_update_fatal(line: &str, expected_fragment: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.matches("fatal").count() == 1
        && line.matches("cannot activate protocol version").count() == 1
        && line.contains(expected_fragment)
        && !contains_nonfatal_alarm(&line)
}

fn contains_nonfatal_alarm(line: &str) -> bool {
    line.contains("panic")
        || (line.contains("dkg")
            && line.contains("share")
            && (line.contains("reveal") || line.contains("disclos")))
        || (line.contains("vrf") && line.contains("alarm"))
        || line.contains("eagain")
        || line.contains("resource temporarily unavailable")
        || (line.contains("projection") && line.contains("fatal"))
}

fn unexpected_log_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("fatal") || contains_nonfatal_alarm(&line)
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn parse_canonical_block_processing_micros(
    line: &str,
    expected_number: u64,
    expected_hash: B256,
) -> Result<Option<u64>> {
    if !line.contains("Block added to canonical chain") {
        return Ok(None);
    }
    let Some(number) = structured_field(line, "number") else {
        bail!("canonical block record has no number");
    };
    let number = number
        .parse::<u64>()
        .wrap_err("canonical block number is not u64")?;
    let Some(hash) = structured_field(line, "hash") else {
        bail!("canonical block record has no hash");
    };
    let hash = hash
        .parse::<B256>()
        .wrap_err("canonical block hash is not B256")?;
    if number != expected_number || hash != expected_hash {
        return Ok(None);
    }
    let Some(elapsed) = structured_field(line, "elapsed") else {
        bail!("canonical block record has no elapsed duration");
    };
    Ok(Some(parse_duration_micros_ceil(elapsed)?))
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn parse_canonical_block_observed_at_micros(
    line: &str,
    expected_number: u64,
    expected_hash: B256,
) -> Result<Option<u64>> {
    if !line.contains("Block added to canonical chain") {
        return Ok(None);
    }
    let number = required_log_u64_field(line, "number", "canonical block")?;
    let hash = required_log_hash_field(line, "hash", "canonical block")?;
    if number != expected_number || hash != expected_hash {
        return Ok(None);
    }
    Ok(Some(parse_log_timestamp_micros(line)?))
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn parse_finalized_block_observed_at_micros(
    line: &str,
    expected_number: u64,
    expected_hash: B256,
) -> Result<Option<u64>> {
    if !line.contains("marshal-delivered block finalized and acked") {
        return Ok(None);
    }
    let number = required_log_u64_field(line, "height", "finalized block")?;
    let hash = required_log_hash_field(line, "digest", "finalized block")?;
    if number != expected_number || hash != expected_hash {
        return Ok(None);
    }
    Ok(Some(parse_log_timestamp_micros(line)?))
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn required_log_u64_field(line: &str, name: &str, record: &str) -> Result<u64> {
    structured_field(line, name)
        .ok_or_else(|| eyre::eyre!("{record} record has no {name}"))?
        .parse::<u64>()
        .wrap_err_with(|| format!("{record} {name} is not u64"))
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn required_log_hash_field(line: &str, name: &str, record: &str) -> Result<B256> {
    structured_field(line, name)
        .ok_or_else(|| eyre::eyre!("{record} record has no {name}"))?
        .parse::<B256>()
        .wrap_err_with(|| format!("{record} {name} is not B256"))
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn parse_log_timestamp_micros(line: &str) -> Result<u64> {
    let timestamp = line
        .split_ascii_whitespace()
        .next()
        .ok_or_else(|| eyre::eyre!("runtime log record has no timestamp"))?;
    let timestamp = OffsetDateTime::parse(timestamp, &Rfc3339)
        .wrap_err("runtime log timestamp is not RFC3339")?;
    let micros = timestamp.unix_timestamp_nanos() / 1_000;
    u64::try_from(micros).wrap_err("runtime log timestamp is before Unix epoch or exceeds u64")
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn parse_ce_startup_replay(
    line: &str,
    validator_index: u8,
) -> Result<Option<CeStartupReplayObservationV1>> {
    if !line.contains("compressed-entity startup replay completed") {
        return Ok(None);
    }
    let first_missing_block_number = required_u64_field(line, "first_missing")?;
    let target_block_number = required_u64_field(line, "target_height")?;
    let replayed_block_count = required_u64_field(line, "replayed_blocks")?;
    let elapsed_micros = required_u64_field(line, "elapsed_micros")?;
    let target_block_hash = structured_field(line, "target_hash")
        .ok_or_else(|| eyre::eyre!("CE startup replay record has no target_hash"))?
        .parse::<B256>()
        .wrap_err("CE startup replay target_hash is not B256")?;
    let expected_count = target_block_number
        .checked_sub(first_missing_block_number)
        .and_then(|span| span.checked_add(1))
        .ok_or_else(|| eyre::eyre!("CE startup replay span is inverted or overflows"))?;
    ensure!(
        first_missing_block_number > 0
            && target_block_hash != B256::ZERO
            && replayed_block_count == expected_count
            && elapsed_micros > 0,
        "CE startup replay record is not a complete positive canonical span"
    );
    Ok(Some(CeStartupReplayObservationV1 {
        validator_index,
        first_missing_block_number,
        target_block_number,
        target_block_hash,
        replayed_block_count,
        elapsed_micros,
    }))
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn required_u64_field(line: &str, name: &str) -> Result<u64> {
    structured_field(line, name)
        .ok_or_else(|| eyre::eyre!("CE startup replay record has no {name}"))?
        .parse::<u64>()
        .wrap_err_with(|| format!("CE startup replay {name} is not u64"))
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn structured_field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    line.split_ascii_whitespace()
        .filter_map(|token| token.split_once('='))
        .find_map(|(field, value)| (field == name).then_some(value))
}

#[cfg(any(test, feature = "ocomp-integration"))]
fn parse_duration_micros_ceil(encoded: &str) -> Result<u64> {
    let (number, nanos_per_unit) = [
        ("ms", 1_000_000_u128),
        ("µs", 1_000_u128),
        ("us", 1_000_u128),
        ("ns", 1_u128),
        ("s", 1_000_000_000_u128),
    ]
    .into_iter()
    .find_map(|(suffix, scale)| encoded.strip_suffix(suffix).map(|value| (value, scale)))
    .ok_or_else(|| eyre::eyre!("unsupported duration unit in {encoded}"))?;
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    ensure!(
        !whole.is_empty()
            && whole.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
            && fraction.len() <= 18,
        "malformed duration {encoded}"
    );
    let whole = whole.parse::<u128>().wrap_err("duration whole overflow")?;
    let whole_nanos = whole
        .checked_mul(nanos_per_unit)
        .ok_or_else(|| eyre::eyre!("duration nanoseconds overflow"))?;
    let fraction_nanos = if fraction.is_empty() {
        0
    } else {
        let numerator = fraction
            .parse::<u128>()
            .wrap_err("duration fraction overflow")?
            .checked_mul(nanos_per_unit)
            .ok_or_else(|| eyre::eyre!("duration fraction nanoseconds overflow"))?;
        let denominator = 10_u128
            .checked_pow(u32::try_from(fraction.len()).wrap_err("duration precision overflow")?)
            .ok_or_else(|| eyre::eyre!("duration denominator overflow"))?;
        numerator
            .checked_add(denominator - 1)
            .ok_or_else(|| eyre::eyre!("duration fraction rounding overflow"))?
            / denominator
    };
    let nanos = whole_nanos
        .checked_add(fraction_nanos)
        .ok_or_else(|| eyre::eyre!("duration nanoseconds overflow"))?;
    let micros = nanos
        .checked_add(999)
        .ok_or_else(|| eyre::eyre!("duration microsecond rounding overflow"))?
        / 1_000;
    let micros = u64::try_from(micros).wrap_err("duration exceeds u64 microseconds")?;
    ensure!(micros > 0, "duration must be positive");
    Ok(micros)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        accept_expected_dkg_reveal, accept_expected_update_fatal, exact_expected_dkg_reveal,
        exact_expected_update_fatal, is_runtime_log, parse_canonical_block_observed_at_micros,
        parse_canonical_block_processing_micros, parse_ce_startup_replay,
        parse_finalized_block_observed_at_micros, unexpected_log_line,
        CeStartupReplayObservationV1, LogCounts, RethLogTail,
    };

    #[test]
    fn audits_runtime_logs_without_reading_binary_database_logs() {
        for path in [
            "validator-0/node.log",
            "validator-0/enclave.log",
            "validator-0/logs/54322345/reth.log",
        ] {
            assert!(is_runtime_log(Path::new(path)), "missed {path}");
        }
        assert!(!is_runtime_log(Path::new(
            "validator-0/data/rocksdb/000011.log"
        )));
    }

    #[test]
    fn classifies_forbidden_operational_records() {
        for line in [
            "thread panicked at actor.rs",
            "fatal consensus supervisor exit",
            "DKG share reveal detected",
            "VRF alarm: proof mismatch",
            "EAGAIN while entering enclave",
            "Resource temporarily unavailable",
            "projection-fatal checkpoint error",
        ] {
            assert!(unexpected_log_line(line), "missed {line}");
        }
        assert!(!unexpected_log_line("committee finalized height=10"));
    }

    #[test]
    fn evidence_counts_are_explicit_and_category_specific() {
        let mut counts = LogCounts::default();
        counts.observe("fatal projection supervisor exit");
        counts.observe("VRF alarm: proof mismatch");
        let json = counts.json();
        assert_eq!(json["runtime_log_files"], 0);
        assert_eq!(json["fatal"], 1);
        assert_eq!(json["projection_fatal"], 1);
        assert_eq!(json["vrf_alarm"], 1);
        assert_eq!(json["panic"], 0);
        assert_eq!(json["dkg_share_reveal"], 0);
        assert_eq!(json["sgx_resource_exhaustion"], 0);
        assert_eq!(json["expected_update_fatal"], 0);
        assert_eq!(json["expected_dkg_reveal"], 0);
    }

    #[test]
    fn allows_only_the_expected_binary_compatibility_fatal() {
        let fragment =
            "cannot activate protocol version v3.0 (50331648): binary supports at most v";
        let expected = "fatal: cannot activate protocol version v3.0 (50331648): binary supports at most v2.3 (33554435)";
        assert!(exact_expected_update_fatal(expected, fragment));
        assert!(!exact_expected_update_fatal(
            &format!("{expected}; fatal: projection exited"),
            fragment
        ));
        assert!(!exact_expected_update_fatal(
            "fatal: cannot activate protocol version v4.0 (67108864): binary supports at most v2.3 (33554435)",
            fragment
        ));
        assert!(unexpected_log_line("fatal: projection exited"));
    }

    #[test]
    fn allows_only_the_exact_expected_dkg_reveal() {
        let key = "91782b96da4ceae23d5adfa62ec55ef41827d43c8b624035972bc0a086f743266168a73e42e5b6c14c119dcf94d39588";
        let expected = format!(
            "WARN outbe::dkg: DKG: a validator's individual share was REVEALED (offline during the ceremony); rotate revealed_validator={key}"
        );
        assert!(exact_expected_dkg_reveal(&expected, key));
        assert!(exact_expected_dkg_reveal(
            &expected.replace(
                "revealed_validator=",
                "revealed_validator\u{1b}[0m\u{1b}[2m=\u{1b}[0m"
            ),
            key
        ));
        assert!(!exact_expected_dkg_reveal(
            &expected.replace(key, &"0".repeat(96)),
            key
        ));
        assert!(!exact_expected_dkg_reveal(
            &format!("fatal {expected}"),
            key
        ));

        let mut seen = [0_usize; 4];
        assert!(accept_expected_dkg_reveal(
            Path::new("scenario-1/validator-2/node.log"),
            &expected,
            key,
            &mut seen,
        ));
        assert_eq!(seen, [0, 0, 1, 0]);
    }

    #[test]
    fn allows_expected_update_fatal_in_reth_log_but_counts_node_log_evidence() {
        let fragment =
            "cannot activate protocol version v3.0 (50331648): binary supports at most v";
        let fatal = "payload builder error=fatal: cannot activate protocol version v3.0 (50331648): binary supports at most v0.1 (1)";
        let mut counts = [0, 0];

        assert!(accept_expected_update_fatal(
            Path::new("validator-1/logs/54322345/reth.log"),
            fatal,
            fragment,
            &mut counts,
        ));
        assert_eq!(counts, [0, 0]);
        assert!(accept_expected_update_fatal(
            Path::new("validator-1/node.log"),
            fatal,
            fragment,
            &mut counts,
        ));
        assert_eq!(counts, [0, 1]);
    }

    #[test]
    fn exact_canonical_block_record_reports_conservative_processing_micros() {
        let block_hash = "0x2a6edf48ac3c8fb19ff4e9daeca617c06c61ced8ec8889a59fafbc00093d4bb7"
            .parse()
            .unwrap();
        let record = "2026-07-27T10:42:12.172399Z INFO reth_node_events::node: \
            Block added to canonical chain number=162 \
            hash=0x2a6edf48ac3c8fb19ff4e9daeca617c06c61ced8ec8889a59fafbc00093d4bb7 \
            peers=3 txs=11 elapsed=590.757245ms";

        assert_eq!(
            parse_canonical_block_processing_micros(record, 162, block_hash).unwrap(),
            Some(590_758)
        );
        assert_eq!(
            parse_canonical_block_processing_micros(record, 163, block_hash).unwrap(),
            None
        );
    }

    #[test]
    fn exact_runtime_records_report_positive_q_block_finality_latency() {
        let block_hash = "0x2a6edf48ac3c8fb19ff4e9daeca617c06c61ced8ec8889a59fafbc00093d4bb7"
            .parse()
            .unwrap();
        let canonical = "2026-07-27T10:42:12.172399Z INFO reth_node_events::node: \
            Block added to canonical chain number=162 \
            hash=0x2a6edf48ac3c8fb19ff4e9daeca617c06c61ced8ec8889a59fafbc00093d4bb7 \
            peers=3 txs=11 elapsed=590.757245ms";
        let finalized = "2026-07-27T10:42:12.707842Z INFO \
            outbe_consensus::executor::actor: marshal-delivered block finalized and acked \
            height=162 \
            digest=0x2a6edf48ac3c8fb19ff4e9daeca617c06c61ced8ec8889a59fafbc00093d4bb7";

        let canonical_at = parse_canonical_block_observed_at_micros(canonical, 162, block_hash)
            .unwrap()
            .unwrap();
        let finalized_at = parse_finalized_block_observed_at_micros(finalized, 162, block_hash)
            .unwrap()
            .unwrap();
        assert_eq!(finalized_at - canonical_at, 535_443);
        assert_eq!(
            parse_finalized_block_observed_at_micros(finalized, 163, block_hash).unwrap(),
            None
        );
    }

    #[test]
    fn completed_ce_startup_replay_record_decodes_the_exact_span() {
        let target_hash = "0x2a6edf48ac3c8fb19ff4e9daeca617c06c61ced8ec8889a59fafbc00093d4bb7"
            .parse()
            .unwrap();
        let record = "2026-07-27T10:44:12.172399Z INFO outbe_engine::ce_recovery: \
            compressed-entity startup replay completed first_missing=1 target_height=166 \
            target_hash=0x2a6edf48ac3c8fb19ff4e9daeca617c06c61ced8ec8889a59fafbc00093d4bb7 \
            replayed_blocks=166 elapsed_micros=912345";

        assert_eq!(
            parse_ce_startup_replay(record, 0).unwrap(),
            Some(CeStartupReplayObservationV1 {
                validator_index: 0,
                first_missing_block_number: 1,
                target_block_number: 166,
                target_block_hash: target_hash,
                replayed_block_count: 166,
                elapsed_micros: 912_345,
            })
        );
    }

    #[test]
    fn reth_log_tail_is_incremental_and_keeps_rotated_files_open() {
        use std::io::Write as _;

        let root = tempfile::tempdir().unwrap();
        let log_dir = root.path().join("chain");
        std::fs::create_dir_all(&log_dir).unwrap();
        let current = log_dir.join("reth.log");
        std::fs::write(&current, "old record\n").unwrap();
        let mut tail = RethLogTail::at_end(root.path()).unwrap();

        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&current)
            .unwrap();
        writeln!(writer, "replay record one").unwrap();
        writer.flush().unwrap();
        assert_eq!(
            tail.read_new_complete_lines().unwrap(),
            "replay record one\n"
        );
        assert!(tail.read_new_complete_lines().unwrap().is_empty());

        std::fs::rename(&current, log_dir.join("reth.log.1")).unwrap();
        std::fs::write(&current, "replay record two\n").unwrap();
        assert_eq!(
            tail.read_new_complete_lines().unwrap(),
            "replay record two\n"
        );
        assert!(tail.read_new_complete_lines().unwrap().is_empty());

        std::fs::write(
            &current,
            "replay record after copy-truncate and rapid refill\n",
        )
        .unwrap();
        assert_eq!(
            tail.read_new_complete_lines().unwrap(),
            "replay record after copy-truncate and rapid refill\n"
        );

        std::fs::write(&current, "partial").unwrap();
        assert!(tail.read_new_complete_lines().unwrap().is_empty());
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&current)
            .unwrap();
        writeln!(writer, " record").unwrap();
        writer.flush().unwrap();
        assert_eq!(tail.read_new_complete_lines().unwrap(), "partial record\n");

        let discovered = super::reth_log_paths(root.path()).unwrap();
        assert!(
            discovered
                .iter()
                .any(|path| path.file_name().unwrap() == "reth.log.1"),
            "already-rotated Reth logs must remain discoverable"
        );
    }
}
