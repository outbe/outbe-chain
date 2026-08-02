//! Typed assembly of OCM-26 cold-run evidence from public Cucumber records.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use alloy_primitives::{B256, U256};
use eyre::{ensure, Context, Result};
use outbe_ocomp_protocol::capacity::{
    CapacityBudgetV1, CapacityColdRunV1, CapacityEvidenceV1, CapacityHistoricalReplayBindingV1,
    CapacityRecoveredGenerationBindingV1, CapacityRunBindingV1, CapacityValidatorBlockProcessingV1,
    CapacityWorkBillV1, ObservedMachineFactsV1, VerifiedCapacityEvidenceV1,
};
use outbe_ocomp_protocol::committee::POC_COMMITTEE_SIZE;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::ocomp_capacity::OcompCapacityHostObservationV1;

use super::hash_file;

const SOURCE_REVISION_DOMAIN: &[u8] = b"OUTBE_OCOMP_SOURCE_REVISION_V1";
const ARTIFACT_SET_DOMAIN: &[u8] = b"OUTBE_OCOMP_CAPACITY_ARTIFACT_SET_V2";
const COLD_NAMESPACE_DOMAIN: &[u8] = b"OUTBE_OCOMP_COLD_NAMESPACE_V1";
const REQUIRED_BINARIES: [&str; 6] = [
    "outbe_chain",
    "outbe_cli",
    "outbe_e2e",
    "outbe_keygen",
    "outbe_ocomp",
    "outbe_tee_enclave",
];

#[derive(Debug, Deserialize)]
struct CapacityScenarioV1 {
    source: ScenarioSourceV1,
    result: String,
    environment: ScenarioEnvironmentV1,
    scenario_data_dir: String,
    ocomp: ScenarioOcompV1,
    #[serde(flatten)]
    _other: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioSourceV1 {
    sha: String,
    dirty: bool,
    tracked_dirty: bool,
    untracked_dirty: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioEnvironmentV1 {
    validators: u64,
    tee: String,
    all: bool,
    gramine_image_id: String,
}

#[derive(Debug, Deserialize)]
struct ScenarioOcompV1 {
    exact_binaries: BTreeMap<String, BinaryIdentityV1>,
    public_path: CapacityPublicPathV1,
    #[serde(flatten)]
    _other: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BinaryIdentityV1 {
    path: String,
    length: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct CapacityPublicPathV1 {
    capacity_resources: CapacityResourcesV1,
    capacity_public_path: CapacityPublicObservationV1,
    capacity_historical_replay: CapacityHistoricalReplayObservationV1,
    #[serde(flatten)]
    _other: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityResourcesV1 {
    cgroup_path: String,
    resource_cgroup_writable: bool,
    memory_limit_bytes: u64,
    cpu_quota_micros: u64,
    cpu_period_micros: u64,
    cpu_micros: u64,
    assigned_memory_bytes: u64,
    disk_write_bytes: u64,
    network_bytes: u64,
    cas_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityPublicObservationV1 {
    job_id: B256,
    result_digest: B256,
    q_forming_transaction_hash: B256,
    q_forming_block_number: u64,
    q_forming_block_hash: B256,
    #[serde(default)]
    _q_forming_receipt_success: Option<bool>,
    #[serde(default)]
    _q_forming_receipt_sha256: Option<String>,
    #[serde(default)]
    _q_forming_validator_receipt_sha256: Option<Vec<String>>,
    #[serde(default)]
    _q_forming_state_root: Option<B256>,
    #[serde(default)]
    _q_forming_ce_root: Option<B256>,
    #[serde(default)]
    _q_forming_validator_commitments: Option<Vec<crate::world::rpc::BlockCommitmentV1>>,
    #[serde(default)]
    _canonical_import_validator_count: Option<u8>,
    #[serde(default)]
    _canonical_import_verified: Option<bool>,
    finalized_block_number: u64,
    finalized_block_hash: B256,
    tribute_count: u64,
    nod_count: u64,
    worker_shard_count: u64,
    transaction_bytes: u64,
    block_bytes: u64,
    gas: u64,
    internal_work: u64,
    block_processing_micros_by_validator: Vec<u64>,
    block_processing_micros: u64,
    finality_latency_micros: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityHistoricalReplayObservationV1 {
    recovery: CapacityHistoricalReplaySpanV1,
    recovered_result_digest: B256,
    recovered_generation: CapacityRecoveredGenerationObservationV1,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityRecoveredGenerationObservationV1 {
    worldwide_day: u32,
    generation: u64,
    job_id: B256,
    program_semantics_hash: B256,
    nod_root: B256,
    bucket_root: B256,
    output_manifest_root: B256,
    tribute_count: u32,
    nod_count: u32,
    bucket_count: u32,
    nod_amount_total: U256,
    nod_gratis_consumed: U256,
    issued_at: u64,
    result_evidence_hash: B256,
    block_number: u64,
    block_hash: B256,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityHistoricalReplaySpanV1 {
    validator_index: u8,
    first_missing_block_number: u64,
    target_block_number: u64,
    target_block_hash: B256,
    replayed_block_count: u64,
    elapsed_micros: u64,
}

/// Converts one immutable public capacity scenario into a protocol work bill.
pub fn assemble_capacity_run(ordinal: u8, scenario_path: &Path) -> Result<CapacityColdRunV1> {
    ensure!((1..=5).contains(&ordinal), "capacity ordinal is not 1..=5");
    let bytes = std::fs::read(scenario_path)
        .wrap_err_with(|| format!("read capacity scenario {}", scenario_path.display()))?;
    let scenario: CapacityScenarioV1 = serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("decode capacity scenario {}", scenario_path.display()))?;
    ensure!(
        scenario.result == "passed",
        "capacity scenario did not pass"
    );
    ensure!(
        !scenario.source.dirty
            && !scenario.source.tracked_dirty
            && !scenario.source.untracked_dirty,
        "capacity scenario was not produced from a clean exact revision"
    );
    ensure!(
        scenario.environment.validators == 4
            && scenario.environment.tee == "gramine-direct"
            && scenario.environment.all,
        "capacity scenario did not use four validators, the production enclave under gramine-direct and --all"
    );
    let revision = hex::decode(&scenario.source.sha).wrap_err("decode source revision")?;
    ensure!(
        revision.len() == 20 || revision.len() == 32,
        "source revision must be a 20-byte Git SHA-1 or 32-byte Git SHA-256"
    );
    let namespace = Path::new(&scenario.scenario_data_dir);
    ensure!(
        namespace.is_absolute()
            && namespace != Path::new("/")
            && namespace
                .components()
                .all(|component| !matches!(component, Component::ParentDir)),
        "capacity scenario namespace is not an absolute non-root path"
    );
    let artifact_set_hash = artifact_set_hash(
        &scenario.ocomp.exact_binaries,
        &scenario.environment.gramine_image_id,
    )?;
    let public = scenario.ocomp.public_path.capacity_public_path;
    let historical_replay = scenario.ocomp.public_path.capacity_historical_replay;
    let resources = scenario.ocomp.public_path.capacity_resources;
    ensure!(
        resources.cgroup_path.contains("outbe-ocomp-capacity-")
            && resources.resource_cgroup_writable
            && resources.memory_limit_bytes >= resources.assigned_memory_bytes
            && resources.cpu_quota_micros > 0
            && resources.cpu_period_micros > 0,
        "capacity resource record is not bound to a finite dedicated cgroup"
    );
    ensure!(
        public.finalized_block_number >= public.q_forming_block_number,
        "capacity finalized capture precedes the q-forming block"
    );
    ensure!(
        public.block_processing_micros_by_validator.len() == 4
            && public
                .block_processing_micros_by_validator
                .iter()
                .all(|value| *value > 0)
            && public
                .block_processing_micros_by_validator
                .iter()
                .copied()
                .max()
                == Some(public.block_processing_micros),
        "capacity block-processing maximum is not bound to four live validator observations"
    );
    ensure!(
        historical_replay.recovered_generation.job_id == public.job_id
            && historical_replay.recovered_result_digest == public.result_digest
            && u64::from(historical_replay.recovered_generation.tribute_count)
                == public.tribute_count
            && u64::from(historical_replay.recovered_generation.nod_count) == public.nod_count
            && historical_replay.recovered_generation.block_number == public.q_forming_block_number
            && historical_replay.recovered_generation.block_hash == public.q_forming_block_hash,
        "historical CE replay did not recover the exact certified public generation"
    );
    let validator_timings: [u64; POC_COMMITTEE_SIZE] = public
        .block_processing_micros_by_validator
        .try_into()
        .map_err(|values: Vec<u64>| {
            eyre::eyre!(
                "capacity block-processing observation count is {}, expected {}",
                values.len(),
                POC_COMMITTEE_SIZE
            )
        })?;

    Ok(CapacityColdRunV1 {
        ordinal,
        source_revision: domain_hash(SOURCE_REVISION_DOMAIN, &revision),
        artifact_set_hash,
        cold_namespace_hash: domain_hash(
            COLD_NAMESPACE_DOMAIN,
            scenario.scenario_data_dir.as_bytes(),
        ),
        binding: CapacityRunBindingV1 {
            scenario_evidence_sha256: sha256(&bytes),
            job_id: public.job_id,
            result_digest: public.result_digest,
            q_forming_transaction_hash: public.q_forming_transaction_hash,
            q_forming_block_number: public.q_forming_block_number,
            q_forming_block_hash: public.q_forming_block_hash,
            finalized_block_number: public.finalized_block_number,
            finalized_block_hash: public.finalized_block_hash,
            tribute_count: public.tribute_count,
            nod_count: public.nod_count,
            worker_shard_count: public.worker_shard_count,
            validator_block_processing: std::array::from_fn(|validator_index| {
                CapacityValidatorBlockProcessingV1 {
                    validator_index: u8::try_from(validator_index)
                        .expect("fixed PoC committee index fits u8"),
                    block_number: public.q_forming_block_number,
                    block_hash: public.q_forming_block_hash,
                    elapsed_micros: validator_timings[validator_index],
                }
            }),
            historical_replay: CapacityHistoricalReplayBindingV1 {
                validator_index: historical_replay.recovery.validator_index,
                first_missing_block_number: historical_replay.recovery.first_missing_block_number,
                target_block_number: historical_replay.recovery.target_block_number,
                target_block_hash: historical_replay.recovery.target_block_hash,
                replayed_block_count: historical_replay.recovery.replayed_block_count,
                elapsed_micros: historical_replay.recovery.elapsed_micros,
                recovered_result_digest: historical_replay.recovered_result_digest,
                recovered_generation: CapacityRecoveredGenerationBindingV1 {
                    worldwide_day: historical_replay.recovered_generation.worldwide_day,
                    generation: historical_replay.recovered_generation.generation,
                    job_id: historical_replay.recovered_generation.job_id,
                    program_semantics_hash: historical_replay
                        .recovered_generation
                        .program_semantics_hash,
                    nod_root: historical_replay.recovered_generation.nod_root,
                    bucket_root: historical_replay.recovered_generation.bucket_root,
                    output_manifest_root: historical_replay
                        .recovered_generation
                        .output_manifest_root,
                    tribute_count: historical_replay.recovered_generation.tribute_count,
                    nod_count: historical_replay.recovered_generation.nod_count,
                    bucket_count: historical_replay.recovered_generation.bucket_count,
                    nod_amount_total: historical_replay.recovered_generation.nod_amount_total,
                    nod_gratis_consumed: historical_replay.recovered_generation.nod_gratis_consumed,
                    issued_at: historical_replay.recovered_generation.issued_at,
                    result_evidence_hash: historical_replay
                        .recovered_generation
                        .result_evidence_hash,
                    block_number: historical_replay.recovered_generation.block_number,
                    block_hash: historical_replay.recovered_generation.block_hash,
                },
            },
        },
        succeeded: true,
        retried: false,
        work: CapacityWorkBillV1 {
            transaction_bytes: public.transaction_bytes,
            block_bytes: public.block_bytes,
            gas: public.gas,
            internal_work: public.internal_work,
            cpu_micros: resources.cpu_micros,
            network_bytes: resources.network_bytes,
            assigned_memory_bytes: resources.assigned_memory_bytes,
            disk_write_bytes: resources.disk_write_bytes,
            cas_bytes: resources.cas_bytes,
            block_processing_micros: public.block_processing_micros,
            finality_latency_micros: public.finality_latency_micros,
        },
    })
}

/// Re-derives one cold run from its immutable public scenario preimage and
/// requires exact equality with the typed capacity evidence.
pub fn verify_capacity_run_preimage(
    expected: &CapacityColdRunV1,
    scenario_path: &Path,
) -> Result<()> {
    let actual = assemble_capacity_run(expected.ordinal, scenario_path)?;
    ensure!(
        actual == *expected,
        "capacity run {} differs from its public scenario preimage {}",
        expected.ordinal,
        scenario_path.display()
    );
    Ok(())
}

/// Assembles and verifies exactly five distinct cold scenario records.
pub fn assemble_capacity_evidence(
    host: OcompCapacityHostObservationV1,
    budget: CapacityBudgetV1,
    scenario_paths: &[impl AsRef<Path>],
) -> Result<(CapacityEvidenceV1, VerifiedCapacityEvidenceV1)> {
    ensure!(
        scenario_paths.len() == 5,
        "capacity assembly requires exactly five scenario records"
    );
    let mut process_memory_limit_bytes = None;
    let mut production_enclave_gramine_direct = true;
    let mut writable_resource_cgroup = true;
    for path in scenario_paths {
        let bytes = std::fs::read(path.as_ref()).wrap_err_with(|| {
            format!(
                "read capacity scenario environment {}",
                path.as_ref().display()
            )
        })?;
        let scenario: CapacityScenarioV1 =
            serde_json::from_slice(&bytes).wrap_err("decode capacity scenario environment")?;
        process_memory_limit_bytes = match process_memory_limit_bytes {
            None => Some(
                scenario
                    .ocomp
                    .public_path
                    .capacity_resources
                    .memory_limit_bytes,
            ),
            Some(expected) => {
                ensure!(
                    scenario
                        .ocomp
                        .public_path
                        .capacity_resources
                        .memory_limit_bytes
                        == expected,
                    "capacity runs use different process memory limits"
                );
                Some(expected)
            }
        };
        production_enclave_gramine_direct &= scenario.environment.tee == "gramine-direct";
        writable_resource_cgroup &= scenario
            .ocomp
            .public_path
            .capacity_resources
            .resource_cgroup_writable;
    }
    let machine = ObservedMachineFactsV1 {
        architecture: host.architecture,
        operating_system: host.operating_system,
        logical_cpu_count: host.logical_cpu_count,
        physical_memory_bytes: host.physical_memory_bytes,
        process_memory_limit_bytes: process_memory_limit_bytes
            .ok_or_else(|| eyre::eyre!("capacity runs have no process memory limit"))?,
        root_disk_bytes: host.root_disk_bytes,
        free_workspace_bytes: host.free_workspace_bytes,
        block_iops: host.block_iops,
        block_throughput_bytes_per_second: host.block_throughput_bytes_per_second,
        pid1_is_systemd: host.pid1_is_systemd,
        unified_cgroup_v2: host.unified_cgroup_v2,
        writable_resource_cgroup,
        production_enclave_gramine_direct,
    };
    let runs = scenario_paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let ordinal = u8::try_from(index + 1).wrap_err("capacity ordinal exceeds u8")?;
            assemble_capacity_run(ordinal, path.as_ref())
        })
        .collect::<Result<Vec<_>>>()?;
    let evidence = CapacityEvidenceV1 {
        machine,
        budget,
        runs,
    };
    let verified = evidence
        .verify()
        .wrap_err("verify assembled OCOMP capacity evidence")?;
    Ok((evidence, verified))
}

fn artifact_set_hash(
    binaries: &BTreeMap<String, BinaryIdentityV1>,
    gramine_image_id: &str,
) -> Result<B256> {
    ensure!(
        binaries.len() == REQUIRED_BINARIES.len()
            && REQUIRED_BINARIES
                .iter()
                .all(|name| binaries.contains_key(*name)),
        "capacity evidence does not contain the exact OCOMP workload binaries"
    );
    let mut preimage = Vec::new();
    preimage.extend_from_slice(ARTIFACT_SET_DOMAIN);
    crate::internal::proc::DockerImageId::from_inspect_output(gramine_image_id)
        .wrap_err("capacity evidence has invalid Gramine Docker image ID")?;
    preimage.extend_from_slice(
        &u16::try_from(gramine_image_id.len())
            .wrap_err("Gramine Docker image ID exceeds u16")?
            .to_be_bytes(),
    );
    preimage.extend_from_slice(gramine_image_id.as_bytes());
    for name in REQUIRED_BINARIES {
        let identity = binaries
            .get(name)
            .ok_or_else(|| eyre::eyre!("missing binary {name}"))?;
        let actual = hash_file(Path::new(&identity.path))
            .wrap_err_with(|| format!("rehash exact capacity binary {name}"))?;
        ensure!(
            actual.length == identity.length && actual.sha256 == identity.sha256,
            "capacity binary {name} differs from its scenario identity"
        );
        let digest = hex::decode(&identity.sha256)
            .wrap_err_with(|| format!("decode capacity binary {name} SHA-256"))?;
        ensure!(digest.len() == 32, "capacity binary digest is not SHA-256");
        preimage.extend_from_slice(
            &u16::try_from(name.len())
                .wrap_err("capacity binary name exceeds u16")?
                .to_be_bytes(),
        );
        preimage.extend_from_slice(name.as_bytes());
        preimage.extend_from_slice(&identity.length.to_be_bytes());
        preimage.extend_from_slice(&digest);
    }
    Ok(sha256(&preimage))
}

fn domain_hash(domain: &[u8], value: &[u8]) -> B256 {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(value);
    B256::from_slice(&hasher.finalize())
}

fn sha256(value: &[u8]) -> B256 {
    B256::from_slice(&Sha256::digest(value))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn ocm_cap_001_assembles_only_the_exact_clean_public_s_plus_one_record() {
        let root = tempdir().unwrap();
        let mut binaries = serde_json::Map::new();
        for name in REQUIRED_BINARIES {
            let path = root.path().join(name);
            fs::write(&path, name.as_bytes()).unwrap();
            let identity = hash_file(&path).unwrap();
            binaries.insert(
                name.to_owned(),
                json!({
                    "path": path,
                    "length": identity.length,
                    "sha256": identity.sha256,
                }),
            );
        }
        let scenario = json!({
            "schema_version": 1,
            "recorded_at_unix_ms": 1,
            "source": {
                "sha": "11".repeat(20),
                "dirty": false,
                "tracked_dirty": false,
                "untracked_dirty": false,
            },
            "result": "passed",
            "environment": {
                "validators": 4,
                "tee": "gramine-direct",
                "all": true,
                "gramine_image_id": format!("sha256:{}", "ab".repeat(32)),
            },
            "scenario_data_dir": root.path().join("run-1"),
            "log_audit": {"clean": true},
            "ocomp": {
                "exact_binaries": binaries,
                "topology": {"domain_roots": ["/tmp/domain"]},
                "public_path": {
                    "job_request": {
                        "job_id": B256::repeat_byte(1),
                        "open_height": 10,
                    },
                    "activation": {
                        "result_digest": B256::repeat_byte(2),
                    },
                    "capacity_resources": {
                        "cgroup_path": "/sys/fs/cgroup/system.slice/outbe-ocomp-capacity-01.service",
                        "resource_cgroup_writable": true,
                        "memory_limit_bytes": 12000,
                        "cpu_quota_micros": 400000,
                        "cpu_period_micros": 100000,
                        "cpu_micros": 1,
                        "assigned_memory_bytes": 2,
                        "disk_write_bytes": 3,
                        "network_bytes": 4,
                        "cas_bytes": 5,
                    },
                    "capacity_public_path": {
                        "job_id": B256::repeat_byte(1),
                        "result_digest": B256::repeat_byte(2),
                        "q_forming_transaction_hash": B256::repeat_byte(3),
                        "q_forming_block_number": 40,
                        "q_forming_block_hash": B256::repeat_byte(4),
                        "finalized_block_number": 42,
                        "finalized_block_hash": B256::repeat_byte(5),
                        "tribute_count": 257,
                        "nod_count": 257,
                        "worker_shard_count": 2,
                        "transaction_bytes": 6,
                        "block_bytes": 7,
                        "gas": 8,
                        "internal_work": 9,
                        "block_processing_micros_by_validator": [8, 9, 10, 7],
                        "block_processing_micros": 10,
                        "finality_latency_micros": 11,
                    },
                    "capacity_historical_replay": {
                        "recovery": {
                            "validator_index": 0,
                            "first_missing_block_number": 1,
                            "target_block_number": 44,
                            "target_block_hash": B256::repeat_byte(6),
                            "replayed_block_count": 44,
                            "elapsed_micros": 12,
                        },
                        "recovered_result_digest": B256::repeat_byte(2),
                        "recovered_generation": {
                            "worldwide_day": 20260728,
                            "generation": 1,
                            "job_id": B256::repeat_byte(1),
                            "program_semantics_hash": B256::repeat_byte(7),
                            "nod_root": B256::repeat_byte(8),
                            "bucket_root": B256::repeat_byte(9),
                            "output_manifest_root": B256::repeat_byte(10),
                            "tribute_count": 257,
                            "nod_count": 257,
                            "bucket_count": 1,
                            "nod_amount_total": U256::from(100),
                            "nod_gratis_consumed": U256::ZERO,
                            "issued_at": 1,
                            "result_evidence_hash": B256::repeat_byte(11),
                            "block_number": 40,
                            "block_hash": B256::repeat_byte(4),
                        },
                    }
                }
            }
        });
        let path = root.path().join("scenario.json");
        fs::write(&path, serde_json::to_vec_pretty(&scenario).unwrap()).unwrap();

        let run = assemble_capacity_run(1, &path).unwrap();
        verify_capacity_run_preimage(&run, &path).unwrap();
        assert_eq!(run.binding.tribute_count, 257);
        assert_eq!(run.binding.worker_shard_count, 2);
        assert_eq!(run.binding.historical_replay.replayed_block_count, 44);
        assert_eq!(run.work.cpu_micros, 1);
        assert_eq!(run.work.finality_latency_micros, 11);

        let mut invented = run.clone();
        invented.binding.validator_block_processing[0].elapsed_micros += 1;
        assert!(verify_capacity_run_preimage(&invented, &path).is_err());

        let mut another_image = scenario.clone();
        another_image["environment"]["gramine_image_id"] =
            json!(format!("sha256:{}", "cd".repeat(32)));
        fs::write(&path, serde_json::to_vec_pretty(&another_image).unwrap()).unwrap();
        let another_image_run = assemble_capacity_run(1, &path).unwrap();
        assert_ne!(run.artifact_set_hash, another_image_run.artifact_set_hash);

        let mut dirty = scenario;
        dirty["source"]["dirty"] = json!(true);
        fs::write(&path, serde_json::to_vec_pretty(&dirty).unwrap()).unwrap();
        assert!(assemble_capacity_run(1, &path).is_err());
    }
}
