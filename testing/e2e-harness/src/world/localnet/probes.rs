//! Node-state probes: datadir moves and node-log inspection used by the
//! recovery/promotion scenarios.

use std::fs;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
#[cfg(any(test, feature = "ocomp-integration"))]
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::B256;
use eyre::{ensure, Result, WrapErr};
use serde::Serialize;

use crate::internal::proc::{first_hex, run_capture, ChildGuard};

use super::Localnet;

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

#[derive(Debug)]
struct HeaderCaptureTimeout {
    status: ExitStatus,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl std::fmt::Display for HeaderCaptureTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "canonical header read exceeded its deadline; exit={}; stdout={}; stderr={}",
            self.status,
            self.stdout.display(),
            self.stderr.display()
        )
    }
}

impl std::error::Error for HeaderCaptureTimeout {}

fn header_capture_timeout(
    status: ExitStatus,
    stdout: tempfile::NamedTempFile,
    stderr: tempfile::NamedTempFile,
) -> Result<Output> {
    let (_, stdout) = stdout.keep().wrap_err("preserve timed-out header stdout")?;
    let (_, stderr) = stderr.keep().wrap_err("preserve timed-out header stderr")?;
    Err(HeaderCaptureTimeout {
        status,
        stdout,
        stderr,
    }
    .into())
}

fn header_stream_before(file: &mut File, deadline: Instant) -> Result<Option<Vec<u8>>> {
    file.seek(SeekFrom::Start(0))?;
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        if Instant::now() >= deadline {
            return Ok(None);
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(Some(output));
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

/// Tempfile-backed capture avoids pipe backpressure. Timeout preserves streams
/// on disk without reading beyond the acceptance budget. Owned reap may need
/// its existing cleanup allowance, but can never extend a successful proof.
fn capture_header_command_until(mut command: Command, deadline: Instant) -> Result<Output> {
    ensure!(
        Instant::now() < deadline,
        "canonical header read deadline already expired"
    );
    let mut stdout = tempfile::NamedTempFile::new()?;
    let mut stderr = tempfile::NamedTempFile::new()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.as_file().try_clone()?))
        .stderr(Stdio::from(stderr.as_file().try_clone()?));
    let mut child = ChildGuard::spawn("readonly-canonical-header", command)?;
    let status = loop {
        let observed = child.exit_status()?;
        if Instant::now() >= deadline {
            let status = match observed {
                Some(status) => status,
                None => child
                    .fault_and_reap()
                    .wrap_err("reap timed-out owned canonical-header reader")?,
            };
            return header_capture_timeout(status, stdout, stderr);
        }
        if let Some(status) = observed {
            break status;
        }
        sleep(Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())));
    };
    let Some(captured_stdout) = header_stream_before(stdout.as_file_mut(), deadline)? else {
        return header_capture_timeout(status, stdout, stderr);
    };
    let Some(captured_stderr) = header_stream_before(stderr.as_file_mut(), deadline)? else {
        return header_capture_timeout(status, stdout, stderr);
    };
    if Instant::now() >= deadline {
        return header_capture_timeout(status, stdout, stderr);
    }
    Ok(Output {
        status,
        stdout: captured_stdout,
        stderr: captured_stderr,
    })
}

fn stopped_header_command(
    binary: &Path,
    data: &Path,
    genesis: &Path,
    height: u64,
) -> Result<Command> {
    for relative in ["db", "static_files", "rocksdb"] {
        ensure!(
            fs::symlink_metadata(data.join(relative))?.is_dir(),
            "canonical header storage is not a directory: {}",
            data.join(relative).display()
        );
    }
    // Reth's readonly CLI creates RocksDB when CURRENT is missing. Refuse to
    // invoke it in that state: collecting evidence must never repair storage.
    ensure!(
        fs::symlink_metadata(data.join("rocksdb/CURRENT"))?.is_file(),
        "canonical header storage lacks regular rocksdb/CURRENT"
    );
    ensure!(
        fs::symlink_metadata(genesis)?.is_file(),
        "canonical header genesis is not a regular file"
    );
    let mut command = Command::new(binary);
    command
        .arg("db")
        .arg("--datadir")
        .arg(data)
        .arg("--chain")
        .arg(genesis)
        .args([
            "--color",
            "never",
            "--log.file.max-files",
            "0",
            "get",
            "static-file",
            "headers",
        ])
        .arg(height.to_string());
    Ok(command)
}

/// One successful testnet startup-recovery span observed from a validator.
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
    pub(crate) fn scenario_id(&self) -> usize {
        self.cfg.scenario
    }

    pub(crate) fn scenario_dir(&self) -> &Path {
        &self.cfg.dir
    }

    /// Forces one validator to reconstruct CE from preserved canonical Reth
    /// history and returns the exact successful replay span emitted by the
    /// testnet startup gate.
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
    /// `launch_dcap_full_node`; the display name only owns the child process.
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use crate::env::Environment;
    use crate::internal::{config::Config, proc::ChildGuard};

    use super::{
        has_required_signing_share, parse_ce_startup_replay, read_required_node_log,
        validator_slot_node_log_path, CeStartupReplayObservationV1, RethLogTail,
    };

    #[test]
    fn canonical_header_capture_preserves_both_streams_and_nonzero_exit() {
        for code in [0, 23] {
            let mut command = Command::new("sh");
            command
                .args([
                    "-c",
                    "printf header; printf diagnostic >&2; exit \"$1\"",
                    "reader",
                ])
                .arg(code.to_string());
            let output = super::capture_header_command_until(
                command,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap();
            assert_eq!(output.status.code(), Some(code));
            assert_eq!(output.stdout, b"header");
            assert_eq!(output.stderr, b"diagnostic");
        }
    }

    #[test]
    fn canonical_header_capture_deadline_never_becomes_success() {
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("invoked");
        let mut command = Command::new("touch");
        command.arg(&marker);
        assert!(super::capture_header_command_until(command, Instant::now()).is_err());
        assert!(!marker.exists());

        let mut survivor = Command::new("sleep");
        survivor.arg("60");
        let mut survivor = ChildGuard::spawn("header-timeout-unrelated", survivor).unwrap();
        let mut reader = Command::new("sleep");
        reader.arg("60");
        let error = super::capture_header_command_until(
            reader,
            Instant::now() + Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeded its deadline"));
        let timeout = error.downcast_ref::<super::HeaderCaptureTimeout>().unwrap();
        assert!(!timeout.status.success());
        assert!(timeout.stdout.is_file() && timeout.stderr.is_file());
        std::fs::remove_file(&timeout.stdout).unwrap();
        std::fs::remove_file(&timeout.stderr).unwrap();
        assert!(survivor.exit_status().unwrap().is_none());
        survivor.fault_and_reap().unwrap();
    }

    #[test]
    fn expired_capture_collection_preserves_complete_diagnostic_files() {
        let mut stdout = tempfile::NamedTempFile::new().unwrap();
        let stderr = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(stdout.path(), b"header diagnostic").unwrap();
        std::fs::write(stderr.path(), b"storage diagnostic").unwrap();
        assert!(
            super::header_stream_before(stdout.as_file_mut(), Instant::now())
                .unwrap()
                .is_none()
        );
        let status = Command::new("true").status().unwrap();
        let error = super::header_capture_timeout(status, stdout, stderr).unwrap_err();
        let timeout = error.downcast_ref::<super::HeaderCaptureTimeout>().unwrap();
        assert_eq!(timeout.status, status);
        assert_eq!(
            std::fs::read(&timeout.stdout).unwrap(),
            b"header diagnostic"
        );
        assert_eq!(
            std::fs::read(&timeout.stderr).unwrap(),
            b"storage diagnostic"
        );
        std::fs::remove_file(&timeout.stdout).unwrap();
        std::fs::remove_file(&timeout.stderr).unwrap();
    }

    #[test]
    fn stopped_header_command_requires_existing_storage_and_exact_readonly_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let genesis = directory.path().join("genesis.json");
        let binary = Path::new("/configured/release/outbe-chain");
        for relative in ["db", "static_files", "rocksdb"] {
            std::fs::create_dir_all(data.join(relative)).unwrap();
        }
        std::fs::write(&genesis, "{}").unwrap();
        assert!(super::stopped_header_command(binary, &data, &genesis, 42).is_err());
        assert!(!data.join("rocksdb/CURRENT").exists());
        std::fs::write(data.join("rocksdb/CURRENT"), "MANIFEST-000001\n").unwrap();
        let command = super::stopped_header_command(binary, &data, &genesis, 42).unwrap();
        assert_eq!(command.get_program(), binary);
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "db".to_owned(),
                "--datadir".into(),
                data.display().to_string(),
                "--chain".into(),
                genesis.display().to_string(),
                "--color".into(),
                "never".into(),
                "--log.file.max-files".into(),
                "0".into(),
                "get".into(),
                "static-file".into(),
                "headers".into(),
                "42".into()
            ]
        );
        std::fs::remove_file(data.join("rocksdb/CURRENT")).unwrap();
        std::os::unix::fs::symlink(&genesis, data.join("rocksdb/CURRENT")).unwrap();
        assert!(super::stopped_header_command(binary, &data, &genesis, 42).is_err());
    }

    #[test]
    fn offline_header_read_requires_exact_owned_exited_node_before_invocation() {
        let directory = tempfile::tempdir().unwrap();
        let env = Environment {
            validators: 1,
            data_dir: directory.path().to_path_buf(),
            ..Environment::default()
        };
        env.ports.start_scenario(1).unwrap();
        let mut net = super::Localnet::new(Config::resolve(&env));
        let data = net.cfg.validator_dir(0).join("data");
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(net.owned_validator_process(0).is_err());
        assert!(net
            .stopped_validator_header_output(0, 0, 42, deadline)
            .is_err());
        let mut command = Command::new("sh");
        command
            .args(["-c", "exec sleep 60", "node", "--datadir"])
            .arg(&data);
        let child = ChildGuard::spawn("header-owned-node", command).unwrap();
        let pid = child.pid();
        net.validators.insert(0, child);
        assert_eq!(net.owned_validator_process(0).unwrap(), (pid, None));
        assert!(net
            .stopped_validator_header_output(0, pid + 1, 42, deadline)
            .unwrap_err()
            .to_string()
            .contains("PID/datadir"));
        assert!(net
            .stopped_validator_header_output(0, pid, 42, deadline)
            .unwrap_err()
            .to_string()
            .contains("still running"));
        assert_eq!(net.owned_validator_process(0).unwrap(), (pid, None));
        net.validators
            .get_mut(&0)
            .unwrap()
            .fault_and_reap()
            .unwrap();
        let (observed_pid, status) = net.owned_validator_process(0).unwrap();
        assert_eq!(observed_pid, pid);
        assert!(status.is_some());
        assert!(net
            .stopped_validator_header_output(0, pid, 42, deadline)
            .is_err());
        assert!(
            !data.exists(),
            "evidence collection must not initialize missing storage"
        );
    }

    #[test]
    fn absent_signing_share_is_not_a_malformed_share() {
        let root = tempfile::tempdir().unwrap();
        assert!(!has_required_signing_share(root.path()).unwrap());
        for relative in ["keys/dkg_share.hex", "data/keys/dkg_share.hex"] {
            let path = root.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "not a share").unwrap();
            assert!(has_required_signing_share(root.path()).is_err());
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn dangling_or_non_file_share_is_an_observation_error() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("keys/dkg_share.hex");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(root.path().join("absent"), &path).unwrap();
        assert!(has_required_signing_share(root.path()).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(has_required_signing_share(root.path()).is_err());
        std::fs::remove_dir(&path).unwrap();
        assert!(std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success());
        assert!(has_required_signing_share(root.path())
            .unwrap_err()
            .to_string()
            .contains("not a regular file"));
    }

    #[test]
    fn follower_trace_path_uses_its_owned_validator_slot() {
        let root = Path::new("/run/scenario-1");
        assert_eq!(
            validator_slot_node_log_path(root, 14),
            root.join("validator-14/node.log")
        );
        assert_ne!(
            validator_slot_node_log_path(root, 14),
            root.join("follower/node.log")
        );
    }

    #[test]
    fn missing_owned_follower_trace_log_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let path = validator_slot_node_log_path(root.path(), 14);
        let error = read_required_node_log(&path, "follower").unwrap_err();
        assert!(error
            .to_string()
            .contains("read OCOMP runtime trace for follower from owned log"));
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
