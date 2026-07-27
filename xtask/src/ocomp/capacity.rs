//! Deterministic OCM-26 capacity-profile generation.
//!
//! The E2E harness owns measurement. This command owns the independent,
//! fail-closed transformation from five typed cold observations to the exact
//! canonical `CapacityProfileV1` bytes consumed by final fork generation.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use alloy_primitives::B256;
use eyre::{ensure, Context, Result};
use outbe_consensus::{
    config::MAX_P2P_MESSAGE_SIZE,
    timing::{DEFAULT_CERTIFICATION_TIMEOUT_MS, DEFAULT_LEADER_TIMEOUT_MS},
};
use outbe_ocomp_protocol::{
    capacity::{
        CapacityBudgetV1, CapacityEvidenceV1, VerifiedCapacityEvidenceV1, OCOMP_POC_CAS_QUOTA_BYTES,
    },
    generated_shape::{
        OCOMP_CAPACITY_PROFILE_ID_HEX, OCOMP_POC_CANDIDATE_LIMITS_V1, OCOMP_POC_DEVNET_MACHINE_V1,
    },
    profile::{poc_schema_limits, CapacityProfileV1},
};
use outbe_primitives::consensus::OUTBE_MAX_BLOCK_SIZE;
use serde::Serialize;
use sha2::{Digest, Sha256};

const GENERATED_CAPACITY_SCHEMA_VERSION: u16 = 1;
const GENERATED_CAPACITY_KIND: &str = "outbe-ocomp-generated-capacity-v1";
const CAPACITY_MEMORY_MAX_BYTES: u64 = OCOMP_POC_DEVNET_MACHINE_V1.minimum_process_memory_bytes;
const CAPACITY_CPU_QUOTA_PERCENT: u16 = 400;
const OCOMP_POC_COMMITTEE_MEMBERS: u64 = 4;
const OCOMP_POC_BLOCK_GAS_LIMIT: u64 = 30_000_000;
const OCOMP_POC_RESULT_DEADLINE_BLOCKS: u64 = 64;

#[derive(Debug, Serialize)]
struct GeneratedCapacityManifestV1 {
    schema_version: u16,
    kind: &'static str,
    source_revision: B256,
    artifact_set_hash: B256,
    capacity_evidence_sha256: B256,
    generated_limits_manifest_sha256: B256,
    capacity_profile_id: B256,
    capacity_profile_ocb1_sha256: B256,
    capacity_profile_ocb1_hex: String,
    capacity_profile: CapacityProfileDocumentV1,
    worst_work: outbe_ocomp_protocol::capacity::CapacityWorkBillV1,
}

#[derive(Debug, Serialize)]
struct CapacityProfileDocumentV1 {
    profile_id: B256,
    max_tributes_per_work_shard: u32,
    max_workers_per_domain: u8,
    max_pending_jobs: u8,
    max_intents_per_block: u8,
    max_activations_per_block: u8,
    max_ready_inspections_per_block: u8,
    max_expirations_per_block: u8,
    retry_backoff_blocks: u64,
    max_terminal_job_records: u16,
    max_reference_currencies: u16,
    max_fidelity_cohorts_per_owner: u16,
    max_oracle_wwd_pair_entries: u32,
    max_active_scurve_entries: u32,
    result_deadline_blocks: u64,
    source_retention_after_terminal_blocks: u64,
    generated_limits_manifest_hash: B256,
}

impl From<CapacityProfileV1> for CapacityProfileDocumentV1 {
    fn from(profile: CapacityProfileV1) -> Self {
        Self {
            profile_id: profile.profile_id,
            max_tributes_per_work_shard: profile.max_tributes_per_work_shard,
            max_workers_per_domain: profile.max_workers_per_domain,
            max_pending_jobs: profile.max_pending_jobs,
            max_intents_per_block: profile.max_intents_per_block,
            max_activations_per_block: profile.max_activations_per_block,
            max_ready_inspections_per_block: profile.max_ready_inspections_per_block,
            max_expirations_per_block: profile.max_expirations_per_block,
            retry_backoff_blocks: profile.retry_backoff_blocks,
            max_terminal_job_records: profile.max_terminal_job_records,
            max_reference_currencies: profile.max_reference_currencies,
            max_fidelity_cohorts_per_owner: profile.max_fidelity_cohorts_per_owner,
            max_oracle_wwd_pair_entries: profile.max_oracle_wwd_pair_entries,
            max_active_scurve_entries: profile.max_active_scurve_entries,
            result_deadline_blocks: profile.result_deadline_blocks,
            source_retention_after_terminal_blocks: profile.source_retention_after_terminal_blocks,
            generated_limits_manifest_hash: profile.generated_limits_manifest_hash,
        }
    }
}

/// Verifies exact evidence and atomically publishes the generated manifest.
pub fn run(
    repository_root: &Path,
    evidence_path: &Path,
    limits_manifest_path: &Path,
    output_path: &Path,
) -> Result<()> {
    let evidence_path = resolve(repository_root, evidence_path);
    let limits_manifest_path = resolve(repository_root, limits_manifest_path);
    let output_path = resolve(repository_root, output_path);
    ensure!(
        output_path.parent().is_some() && output_path != Path::new("/"),
        "capacity output must be a non-root file path"
    );

    let evidence_bytes = std::fs::read(&evidence_path)
        .wrap_err_with(|| format!("read capacity evidence {}", evidence_path.display()))?;
    let evidence: CapacityEvidenceV1 = serde_json::from_slice(&evidence_bytes)
        .wrap_err_with(|| format!("decode typed capacity evidence {}", evidence_path.display()))?;
    let verified = evidence
        .verify()
        .wrap_err("verify five cold OCOMP capacity runs")?;

    let limits_bytes = std::fs::read(&limits_manifest_path).wrap_err_with(|| {
        format!(
            "read generated limits manifest {}",
            limits_manifest_path.display()
        )
    })?;
    ensure!(
        !limits_bytes.is_empty(),
        "generated limits manifest is empty"
    );
    let manifest = generate(&evidence_bytes, &limits_bytes, verified)?;
    publish_atomic(&output_path, &manifest)
}

/// Publishes the deterministic resource budget derived from the already
/// frozen PoC machine, consensus and local-runtime limits.
pub fn publish_budget(repository_root: &Path, output_path: &Path) -> Result<()> {
    let output_path = resolve(repository_root, output_path);
    let budget = frozen_capacity_budget()?;
    let value = serde_json::to_value(budget)?;
    publish_new_json(&output_path, &value)
}

/// Executes exactly five cold public S+1 runs. A started ordinal is immutable:
/// failure terminates the command and the output root cannot be reused.
pub fn measure(
    repository_root: &Path,
    output_dir: &Path,
    limits_manifest_path: &Path,
    generated_capacity_path: &Path,
) -> Result<()> {
    require_clean_revision(repository_root)?;
    let output_dir = resolve(repository_root, output_dir);
    ensure!(
        output_dir.is_absolute() && output_dir != Path::new("/") && !output_dir.exists(),
        "capacity output directory must be a new absolute non-root path"
    );
    std::fs::create_dir_all(&output_dir)
        .wrap_err_with(|| format!("create capacity output {}", output_dir.display()))?;
    let budget_path = output_dir.join("capacity-budget-v1.json");
    let budget = serde_json::to_value(frozen_capacity_budget()?)?;
    publish_new_json(&budget_path, &budget)?;

    let binaries = CapacityBinariesV1::from_repository(repository_root)?;
    binaries.validate()?;
    let runner_uid = command_stdout(repository_root, "id", &["-u"])?;
    let runner_gid = command_stdout(repository_root, "id", &["-g"])?;
    validate_runner_identity(&runner_uid, "uid")?;
    validate_runner_identity(&runner_gid, "gid")?;
    require_systemd_host()?;
    let host_path = output_dir.join("host-observation.json");
    run_checked(
        Command::new(&binaries.evidence)
            .arg("--repo")
            .arg(repository_root)
            .arg("capacity-host")
            .arg("--workspace")
            .arg(&output_dir)
            .arg("--output")
            .arg(&host_path),
        "measure OCOMP capacity host",
    )?;

    let source = command_stdout(repository_root, "git", &["rev-parse", "HEAD"])?;
    let source_prefix = source
        .get(..12)
        .ok_or_else(|| eyre::eyre!("Git revision is shorter than 12 bytes"))?;
    let mut scenario_paths = Vec::with_capacity(5);
    for ordinal in 1_u8..=5 {
        let run_root = output_dir.join(format!("run-{ordinal:02}"));
        std::fs::create_dir(&run_root)
            .wrap_err_with(|| format!("create cold run {}", run_root.display()))?;
        let unit = format!("outbe-ocomp-capacity-{source_prefix}-{ordinal:02}");
        publish_attempt_start(&run_root, ordinal, &unit, &source)?;
        let evidence_dir = run_root.join("evidence");
        let data_dir = run_root.join("data");
        let stdout = create_new_file(&run_root.join("systemd-run.stdout"))?;
        let stderr = create_new_file(&run_root.join("systemd-run.stderr"))?;
        let mut command = capacity_systemd_command(
            &unit,
            &runner_uid,
            &runner_gid,
            repository_root,
            &evidence_dir,
            &data_dir,
            &binaries,
        );
        let status = command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .status()
            .wrap_err_with(|| format!("start cold capacity run {ordinal}"))?;
        publish_attempt_outcome(&run_root, ordinal, status.success(), status.code())?;
        ensure!(
            status.success(),
            "cold capacity run {ordinal} failed; retries are forbidden and evidence remains in {}",
            run_root.display()
        );
        scenario_paths.push(single_scenario_record(&evidence_dir)?);
    }

    let capacity_evidence_path = output_dir.join("capacity-evidence-v1.json");
    let mut assemble = Command::new(&binaries.evidence);
    assemble
        .arg("--repo")
        .arg(repository_root)
        .arg("capacity-assemble")
        .arg("--host")
        .arg(&host_path)
        .arg("--budget")
        .arg(&budget_path);
    for scenario in &scenario_paths {
        assemble.arg("--scenario").arg(scenario);
    }
    assemble.arg("--output").arg(&capacity_evidence_path);
    run_checked(&mut assemble, "assemble five OCOMP capacity runs")?;
    run(
        repository_root,
        &capacity_evidence_path,
        limits_manifest_path,
        generated_capacity_path,
    )
}

fn frozen_capacity_budget() -> Result<CapacityBudgetV1> {
    let validation_window_ms = DEFAULT_CERTIFICATION_TIMEOUT_MS
        .checked_sub(DEFAULT_LEADER_TIMEOUT_MS)
        .ok_or_else(|| eyre::eyre!("OCOMP PoC validation window underflows"))?;
    ensure!(
        validation_window_ms > 0,
        "OCOMP PoC validation window must be positive"
    );
    let finality_latency_micros = OCOMP_POC_RESULT_DEADLINE_BLOCKS
        .checked_mul(DEFAULT_CERTIFICATION_TIMEOUT_MS)
        .and_then(|value| value.checked_mul(1_000))
        .ok_or_else(|| eyre::eyre!("OCOMP PoC finality budget overflows"))?;
    let cpu_micros = OCOMP_POC_DEVNET_MACHINE_V1
        .logical_cpu_count
        .checked_mul(finality_latency_micros)
        .ok_or_else(|| eyre::eyre!("OCOMP PoC CPU budget overflows"))?;
    let directed_committee_edges = OCOMP_POC_COMMITTEE_MEMBERS
        .checked_mul(OCOMP_POC_COMMITTEE_MEMBERS.saturating_sub(1))
        .ok_or_else(|| eyre::eyre!("OCOMP PoC committee edge count overflows"))?;
    let network_bytes = u64::from(MAX_P2P_MESSAGE_SIZE)
        .checked_mul(directed_committee_edges)
        .and_then(|value| value.checked_mul(OCOMP_POC_RESULT_DEADLINE_BLOCKS))
        .ok_or_else(|| eyre::eyre!("OCOMP PoC network budget overflows"))?;
    Ok(CapacityBudgetV1 {
        transaction_bytes: u64::try_from(OUTBE_MAX_BLOCK_SIZE)
            .wrap_err("OCOMP block size exceeds u64")?,
        block_bytes: u64::try_from(OUTBE_MAX_BLOCK_SIZE)
            .wrap_err("OCOMP block size exceeds u64")?,
        gas: OCOMP_POC_BLOCK_GAS_LIMIT,
        internal_work: OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_internal_work,
        cpu_micros,
        network_bytes,
        assigned_memory_bytes: OCOMP_POC_DEVNET_MACHINE_V1.minimum_process_memory_bytes,
        disk_write_bytes: OCOMP_POC_DEVNET_MACHINE_V1.minimum_free_workspace_bytes,
        cas_bytes: OCOMP_POC_CAS_QUOTA_BYTES,
        block_processing_micros: validation_window_ms
            .checked_mul(1_000)
            .ok_or_else(|| eyre::eyre!("OCOMP PoC block-processing budget overflows"))?,
        finality_latency_micros,
    })
}

#[derive(Debug)]
struct CapacityBinariesV1 {
    chain: PathBuf,
    ocomp: PathBuf,
    e2e: PathBuf,
    evidence: PathBuf,
    mock_enclave: PathBuf,
}

impl CapacityBinariesV1 {
    fn from_repository(repository_root: &Path) -> Result<Self> {
        Ok(Self {
            chain: repository_root.join("target/debug/outbe-chain"),
            ocomp: repository_root.join("target/debug/outbe-ocomp"),
            e2e: repository_root.join("target/debug/outbe-e2e"),
            evidence: repository_root.join("target/debug/outbe-e2e-evidence"),
            mock_enclave: repository_root.join("target/release/outbe-tee-enclave-mock"),
        })
    }

    fn validate(&self) -> Result<()> {
        for (name, path) in [
            ("outbe-chain", &self.chain),
            ("outbe-ocomp", &self.ocomp),
            ("outbe-e2e", &self.e2e),
            ("outbe-e2e-evidence", &self.evidence),
            ("outbe-tee-enclave-mock", &self.mock_enclave),
        ] {
            let metadata = std::fs::symlink_metadata(path)
                .wrap_err_with(|| format!("inspect exact capacity binary {name}"))?;
            ensure!(
                metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
                "capacity binary {name} is not a real file: {}",
                path.display()
            );
        }
        Ok(())
    }
}

fn capacity_systemd_command(
    unit: &str,
    runner_uid: &str,
    runner_gid: &str,
    repository_root: &Path,
    evidence_dir: &Path,
    data_dir: &Path,
    binaries: &CapacityBinariesV1,
) -> Command {
    let mut command = Command::new("sudo");
    command
        .arg("-n")
        .arg("systemd-run")
        .arg("--quiet")
        .arg("--wait")
        .arg("--collect")
        .arg("--pipe")
        .arg("--same-dir")
        .arg(format!("--unit={unit}"))
        .arg(format!("--uid={runner_uid}"))
        .arg(format!("--gid={runner_gid}"))
        .arg(format!("--setenv=OUTBE_OCOMP_CAPACITY_RUN_ID={unit}"))
        .arg(format!("--property=MemoryMax={CAPACITY_MEMORY_MAX_BYTES}"))
        .arg(format!("--property=CPUQuota={CAPACITY_CPU_QUOTA_PERCENT}%"))
        .arg("--property=MemoryAccounting=yes")
        .arg("--property=CPUAccounting=yes")
        .arg("--property=IOAccounting=yes")
        .arg("--property=Delegate=yes")
        .arg("--property=OOMPolicy=stop")
        .arg("--property=TimeoutStartSec=30min")
        .arg(&binaries.e2e)
        .arg("--tags")
        .arg("@ocomp-capacity")
        .arg("--concurrency")
        .arg("1")
        .arg("--no-resolve-ports")
        .arg("--no-sudo")
        .arg("--tee")
        .arg("mock")
        .arg("--all")
        .arg("--no-cleanup")
        .arg("--repo")
        .arg(repository_root)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--evidence-dir")
        .arg(evidence_dir)
        .arg("--chain-bin")
        .arg(&binaries.chain)
        .arg("--ocomp-bin")
        .arg(&binaries.ocomp)
        .arg("--mock-bin")
        .arg(&binaries.mock_enclave);
    command
}

fn require_systemd_host() -> Result<()> {
    let pid1 = std::fs::read_to_string("/proc/1/comm").wrap_err("read PID 1 identity")?;
    ensure!(
        pid1.trim() == "systemd",
        "OCM-26 cold runs require systemd as PID 1; observed {}",
        pid1.trim()
    );
    ensure!(
        Path::new("/sys/fs/cgroup/cgroup.controllers").is_file(),
        "OCM-26 cold runs require unified cgroup v2"
    );
    let status = Command::new("sudo")
        .args(["-n", "systemd-run", "--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .wrap_err("start privileged systemd-run preflight")?;
    ensure!(
        status.success(),
        "passwordless system-manager systemd-run is unavailable"
    );
    Ok(())
}

fn validate_runner_identity(value: &str, name: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "capacity runner {name} is not a decimal identifier"
    );
    ensure!(
        value != "0",
        "capacity workload must run as an unprivileged user"
    );
    Ok(())
}

fn require_clean_revision(repository_root: &Path) -> Result<()> {
    let tracked = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(repository_root)
        .output()
        .wrap_err("inspect tracked capacity source state")?;
    ensure!(
        tracked.status.success(),
        "git tracked-state inspection failed"
    );
    let untracked = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(repository_root)
        .output()
        .wrap_err("inspect untracked capacity source state")?;
    ensure!(
        untracked.status.success(),
        "git untracked-state inspection failed"
    );
    ensure!(
        tracked.stdout.is_empty() && untracked.stdout.is_empty(),
        "OCM-26 capacity runs require one clean exact revision"
    );
    Ok(())
}

fn publish_attempt_start(root: &Path, ordinal: u8, unit: &str, source: &str) -> Result<()> {
    let value = serde_json::json!({
        "schema_version": 1,
        "ordinal": ordinal,
        "unit": unit,
        "source_revision": source,
        "attempt": 1,
    });
    publish_new_json(&root.join("attempt-start.json"), &value)
}

fn publish_attempt_outcome(
    root: &Path,
    ordinal: u8,
    succeeded: bool,
    exit_code: Option<i32>,
) -> Result<()> {
    let value = serde_json::json!({
        "schema_version": 1,
        "ordinal": ordinal,
        "attempt": 1,
        "succeeded": succeeded,
        "exit_code": exit_code,
    });
    publish_new_json(&root.join("attempt-outcome.json"), &value)
}

fn publish_new_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut file = create_new_file(path)?;
    file.write_all(&bytes)
        .wrap_err_with(|| format!("write immutable capacity record {}", path.display()))?;
    file.sync_all()
        .wrap_err_with(|| format!("sync immutable capacity record {}", path.display()))
}

fn create_new_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .wrap_err_with(|| format!("create immutable capacity file {}", path.display()))
}

fn single_scenario_record(evidence_dir: &Path) -> Result<PathBuf> {
    let mut matches = std::fs::read_dir(evidence_dir)
        .wrap_err_with(|| format!("read capacity evidence {}", evidence_dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("scenario-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    matches.sort();
    ensure!(
        matches.len() == 1,
        "cold run must publish exactly one scenario record, found {}",
        matches.len()
    );
    Ok(matches.remove(0))
}

fn run_checked(command: &mut Command, action: &str) -> Result<()> {
    let status = command
        .status()
        .wrap_err_with(|| format!("start {action}"))?;
    ensure!(status.success(), "{action} failed with {status}");
    Ok(())
}

fn command_stdout(repository_root: &Path, program: &str, arguments: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(repository_root)
        .output()
        .wrap_err_with(|| format!("start {program} {}", arguments.join(" ")))?;
    ensure!(
        output.status.success(),
        "{program} {} failed",
        arguments.join(" ")
    );
    let value = String::from_utf8(output.stdout).wrap_err("command output is not UTF-8")?;
    let value = value.trim();
    ensure!(!value.is_empty(), "{program} produced empty output");
    Ok(value.to_owned())
}

fn generate(
    evidence_bytes: &[u8],
    limits_manifest_bytes: &[u8],
    verified: VerifiedCapacityEvidenceV1,
) -> Result<Vec<u8>> {
    let evidence_sha256 = sha256(evidence_bytes);
    let generated_limits_manifest_sha256 = sha256(limits_manifest_bytes);
    let capacity_profile_id = parse_b256(OCOMP_CAPACITY_PROFILE_ID_HEX)
        .wrap_err("parse generated capacity profile id")?;
    let profile =
        verified.capacity_profile(capacity_profile_id, generated_limits_manifest_sha256)?;
    let canonical = profile
        .encode_canonical(&poc_schema_limits())
        .wrap_err("encode canonical capacity profile")?;
    let document = GeneratedCapacityManifestV1 {
        schema_version: GENERATED_CAPACITY_SCHEMA_VERSION,
        kind: GENERATED_CAPACITY_KIND,
        source_revision: verified.source_revision,
        artifact_set_hash: verified.artifact_set_hash,
        capacity_evidence_sha256: evidence_sha256,
        generated_limits_manifest_sha256,
        capacity_profile_id,
        capacity_profile_ocb1_sha256: sha256(&canonical),
        capacity_profile_ocb1_hex: hex::encode(canonical),
        capacity_profile: profile.into(),
        worst_work: verified.worst_work,
    };
    let mut encoded = serde_json::to_vec_pretty(&document)?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn resolve(repository_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        repository_root.join(path)
    }
}

fn parse_b256(value: &str) -> Result<B256> {
    let bytes = hex::decode(value)?;
    ensure!(bytes.len() == B256::len_bytes(), "expected 32-byte digest");
    Ok(B256::from_slice(&bytes))
}

fn sha256(bytes: &[u8]) -> B256 {
    B256::from_slice(&Sha256::digest(bytes))
}

fn publish_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre::eyre!("capacity output has no parent"))?;
    std::fs::create_dir_all(parent)
        .wrap_err_with(|| format!("create capacity output directory {}", parent.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| eyre::eyre!("capacity output has no file name"))?
        .to_string_lossy();
    let temporary = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    ensure!(
        !temporary.exists(),
        "capacity temporary output already exists: {}",
        temporary.display()
    );
    std::fs::write(&temporary, bytes)
        .wrap_err_with(|| format!("write capacity temporary output {}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .wrap_err_with(|| format!("publish capacity output {}", path.display()))
}

#[cfg(test)]
mod tests {
    use outbe_ocomp_protocol::capacity::{
        CapacityBudgetV1, CapacityColdRunV1, CapacityEvidenceV1, CapacityRunBindingV1,
        CapacityWorkBillV1, ObservedMachineFactsV1,
    };

    use super::*;

    #[test]
    fn ocm_cap_001_generation_is_deterministic_and_bound_to_exact_inputs() {
        let evidence = evidence();
        let evidence_bytes = serde_json::to_vec_pretty(&evidence).unwrap();
        let verified = evidence.verify().unwrap();
        let first = generate(&evidence_bytes, b"{\"limits\":1}\n", verified.clone()).unwrap();
        let second = generate(&evidence_bytes, b"{\"limits\":1}\n", verified).unwrap();
        assert_eq!(first, second);

        let changed = generate(
            &evidence_bytes,
            b"{\"limits\":2}\n",
            evidence.verify().unwrap(),
        )
        .unwrap();
        assert_ne!(first, changed);

        let document: serde_json::Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(
            document["capacity_profile"]["max_tributes_per_work_shard"],
            256
        );
        assert_eq!(
            document["capacity_profile"]["max_pending_jobs"], 2,
            "capacity profile must retain concurrent independent Job progress"
        );
        assert_eq!(
            document["capacity_profile_ocb1_hex"]
                .as_str()
                .unwrap()
                .len()
                % 2,
            0
        );
    }

    #[test]
    fn ocm_cap_001_cold_runner_builds_one_dedicated_non_retrying_public_command() {
        let root = Path::new("/repo");
        let binaries = CapacityBinariesV1 {
            chain: root.join("target/debug/outbe-chain"),
            ocomp: root.join("target/debug/outbe-ocomp"),
            e2e: root.join("target/debug/outbe-e2e"),
            evidence: root.join("target/debug/outbe-e2e-evidence"),
            mock_enclave: root.join("target/release/outbe-tee-enclave-mock"),
        };
        let command = capacity_systemd_command(
            "outbe-ocomp-capacity-deadbeef0000-01",
            "1000",
            "1000",
            root,
            Path::new("/evidence"),
            Path::new("/data"),
            &binaries,
        );
        let arguments = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(command.get_program(), "sudo");
        assert!(arguments.starts_with(&[
            "-n".to_owned(),
            "systemd-run".to_owned(),
            "--quiet".to_owned(),
        ]));
        assert!(arguments.contains(&"--wait".to_owned()));
        assert!(arguments.contains(&"--collect".to_owned()));
        assert!(arguments.contains(&"--uid=1000".to_owned()));
        assert!(arguments.contains(&"--gid=1000".to_owned()));
        assert!(!arguments.contains(&"--user".to_owned()));
        assert!(arguments.contains(&"--property=Delegate=yes".to_owned()));
        assert!(arguments.contains(&format!("--property=MemoryMax={CAPACITY_MEMORY_MAX_BYTES}")));
        assert_eq!(
            arguments
                .iter()
                .filter(|argument| argument.as_str() == "@ocomp-capacity")
                .count(),
            1
        );
        assert_eq!(
            arguments
                .iter()
                .filter(|argument| argument.as_str() == "1")
                .count(),
            1,
            "one runner invocation has exactly one Cucumber attempt"
        );
    }

    #[test]
    fn ocm_cap_001_budget_is_derived_from_frozen_runtime_authorities() {
        let budget = frozen_capacity_budget().unwrap();
        assert_eq!(
            budget.transaction_bytes,
            u64::try_from(OUTBE_MAX_BLOCK_SIZE).unwrap()
        );
        assert_eq!(budget.block_bytes, budget.transaction_bytes);
        assert_eq!(budget.gas, OCOMP_POC_BLOCK_GAS_LIMIT);
        assert_eq!(
            budget.internal_work,
            OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_internal_work
        );
        assert_eq!(budget.cpu_micros, 2_048_000_000);
        assert_eq!(budget.network_bytes, 1_610_612_736);
        assert_eq!(
            budget.assigned_memory_bytes,
            OCOMP_POC_DEVNET_MACHINE_V1.minimum_process_memory_bytes
        );
        assert_eq!(
            budget.disk_write_bytes,
            OCOMP_POC_DEVNET_MACHINE_V1.minimum_free_workspace_bytes
        );
        assert_eq!(budget.cas_bytes, OCOMP_POC_CAS_QUOTA_BYTES);
        assert_eq!(budget.block_processing_micros, 4_000_000);
        assert_eq!(budget.finality_latency_micros, 512_000_000);
        assert_eq!(
            CAPACITY_CPU_QUOTA_PERCENT,
            u16::try_from(OCOMP_POC_DEVNET_MACHINE_V1.logical_cpu_count * 100).unwrap()
        );
    }

    fn evidence() -> CapacityEvidenceV1 {
        let budget = CapacityBudgetV1 {
            transaction_bytes: 1_000,
            block_bytes: 1_000,
            gas: 1_000,
            internal_work: 1_000,
            cpu_micros: 1_000,
            network_bytes: 1_000,
            assigned_memory_bytes: 1_000,
            disk_write_bytes: 1_000,
            cas_bytes: 1_000,
            block_processing_micros: 1_000,
            finality_latency_micros: 1_000,
        };
        let work = CapacityWorkBillV1 {
            transaction_bytes: 800,
            block_bytes: 800,
            gas: 800,
            internal_work: 800,
            cpu_micros: 800,
            network_bytes: 800,
            assigned_memory_bytes: 800,
            disk_write_bytes: 800,
            cas_bytes: 800,
            block_processing_micros: 800,
            finality_latency_micros: 800,
        };
        let runs = (1_u8..=5)
            .map(|ordinal| CapacityColdRunV1 {
                ordinal,
                source_revision: B256::repeat_byte(1),
                artifact_set_hash: B256::repeat_byte(2),
                cold_namespace_hash: B256::repeat_byte(ordinal.saturating_add(10)),
                binding: CapacityRunBindingV1 {
                    scenario_evidence_sha256: B256::repeat_byte(ordinal.saturating_add(20)),
                    job_id: B256::repeat_byte(3),
                    result_digest: B256::repeat_byte(4),
                    q_forming_transaction_hash: B256::repeat_byte(5),
                    q_forming_block_hash: B256::repeat_byte(6),
                    finalized_block_hash: B256::repeat_byte(7),
                    tribute_count: 257,
                    nod_count: 257,
                    worker_shard_count: 2,
                },
                succeeded: true,
                retried: false,
                work,
            })
            .collect();
        CapacityEvidenceV1 {
            machine: ObservedMachineFactsV1 {
                architecture: "x86_64".to_owned(),
                operating_system: "Ubuntu 24.04".to_owned(),
                logical_cpu_count: 4,
                physical_memory_bytes: 17_179_869_184,
                process_memory_limit_bytes: 12_884_901_888,
                root_disk_bytes: 139_586_437_120,
                free_workspace_bytes: 107_374_182_400,
                block_iops: 8_000,
                block_throughput_bytes_per_second: 250_000_000,
                pid1_is_systemd: true,
                unified_cgroup_v2: true,
                writable_resource_cgroup: true,
                mock_gramine: true,
            },
            budget,
            runs,
        }
    }
}
