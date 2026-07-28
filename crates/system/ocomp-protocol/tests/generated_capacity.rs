// OCOMP-TEST-ID: OCM-CAP-001

use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    capacity::{
        result_vote_internal_work, worker_shard_count, CapacityBudgetV1, CapacityColdRunV1,
        CapacityEvidenceError, CapacityEvidenceV1, CapacityHistoricalReplayBindingV1,
        CapacityRecoveredGenerationBindingV1, CapacityRunBindingV1,
        CapacityValidatorBlockProcessingV1, CapacityWorkBillV1, ObservedMachineFactsV1,
    },
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    profile::poc_schema_limits,
};

fn conforming_machine() -> ObservedMachineFactsV1 {
    ObservedMachineFactsV1 {
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
        production_enclave_gramine_direct: true,
    }
}

fn work(value: u64) -> CapacityWorkBillV1 {
    CapacityWorkBillV1 {
        transaction_bytes: value,
        block_bytes: value,
        gas: value,
        internal_work: value,
        cpu_micros: value,
        network_bytes: value,
        assigned_memory_bytes: value,
        disk_write_bytes: value,
        cas_bytes: value,
        block_processing_micros: value,
        finality_latency_micros: value,
    }
}

fn budget(value: u64) -> CapacityBudgetV1 {
    CapacityBudgetV1 {
        transaction_bytes: value,
        block_bytes: value,
        gas: value,
        internal_work: value,
        cpu_micros: value,
        network_bytes: value,
        assigned_memory_bytes: value,
        disk_write_bytes: value,
        cas_bytes: value,
        block_processing_micros: value,
        finality_latency_micros: value,
    }
}

fn evidence(work_value: u64) -> CapacityEvidenceV1 {
    CapacityEvidenceV1 {
        machine: conforming_machine(),
        budget: budget(1_000),
        runs: (1..=5)
            .map(|ordinal| CapacityColdRunV1 {
                ordinal,
                source_revision: B256::repeat_byte(1),
                artifact_set_hash: B256::repeat_byte(2),
                cold_namespace_hash: B256::repeat_byte(ordinal.saturating_add(10)),
                binding: CapacityRunBindingV1 {
                    scenario_evidence_sha256: B256::repeat_byte(ordinal.saturating_add(20)),
                    job_id: B256::repeat_byte(ordinal.saturating_add(30)),
                    result_digest: B256::repeat_byte(ordinal.saturating_add(40)),
                    q_forming_transaction_hash: B256::repeat_byte(ordinal.saturating_add(50)),
                    q_forming_block_number: 40,
                    q_forming_block_hash: B256::repeat_byte(ordinal.saturating_add(60)),
                    finalized_block_number: 42,
                    finalized_block_hash: B256::repeat_byte(ordinal.saturating_add(70)),
                    tribute_count: 257,
                    nod_count: 257,
                    worker_shard_count: 2,
                    validator_block_processing: std::array::from_fn(|validator_index| {
                        CapacityValidatorBlockProcessingV1 {
                            validator_index: u8::try_from(validator_index).unwrap(),
                            block_number: 40,
                            block_hash: B256::repeat_byte(ordinal.saturating_add(60)),
                            elapsed_micros: if validator_index == 3 {
                                work_value
                            } else {
                                100
                            },
                        }
                    }),
                    historical_replay: CapacityHistoricalReplayBindingV1 {
                        validator_index: 0,
                        first_missing_block_number: 1,
                        target_block_number: 44,
                        target_block_hash: B256::repeat_byte(ordinal.saturating_add(80)),
                        replayed_block_count: 44,
                        elapsed_micros: 100,
                        recovered_result_digest: B256::repeat_byte(ordinal.saturating_add(40)),
                        recovered_generation: CapacityRecoveredGenerationBindingV1 {
                            worldwide_day: 20260728,
                            generation: 1,
                            job_id: B256::repeat_byte(ordinal.saturating_add(30)),
                            program_semantics_hash: B256::repeat_byte(90),
                            nod_root: B256::repeat_byte(91),
                            bucket_root: B256::repeat_byte(92),
                            output_manifest_root: B256::repeat_byte(93),
                            tribute_count: 257,
                            nod_count: 257,
                            bucket_count: 1,
                            nod_amount_total: U256::from(100),
                            nod_gratis_consumed: U256::ZERO,
                            issued_at: 1,
                            result_evidence_hash: B256::repeat_byte(94),
                            block_number: 40,
                            block_hash: B256::repeat_byte(ordinal.saturating_add(60)),
                        },
                    },
                },
                succeeded: true,
                retried: false,
                work: work(work_value),
            })
            .collect(),
    }
}

#[test]
fn ocm_cap_001_population_crosses_shard_boundaries_without_a_job_ceiling() {
    let shard_capacity =
        u32::try_from(OCOMP_POC_CANDIDATE_LIMITS_V1.max_tributes_per_work_shard).unwrap();
    assert_eq!(worker_shard_count(255, shard_capacity).unwrap(), 1);
    assert_eq!(worker_shard_count(256, shard_capacity).unwrap(), 1);
    assert_eq!(worker_shard_count(257, shard_capacity).unwrap(), 2);
    assert_eq!(worker_shard_count(10_000, shard_capacity).unwrap(), 40);
    assert_eq!(
        worker_shard_count(1_000_000_000, shard_capacity).unwrap(),
        3_906_250
    );
}

#[test]
fn ocm_cap_001_five_exact_cold_runs_generate_one_canonical_profile() {
    let verified = evidence(800).verify().unwrap();
    let profile = verified
        .capacity_profile(B256::repeat_byte(3), B256::repeat_byte(4))
        .unwrap();
    assert_eq!(profile.max_tributes_per_work_shard, 256);
    assert_eq!(profile.max_workers_per_domain, 4);
    assert!(
        profile.max_pending_jobs >= 2,
        "the PoC profile must admit more than one independently progressing Job"
    );
    assert_eq!(
        profile.max_intents_per_block, 1,
        "multi-job state must not increase per-block request work"
    );
    assert_eq!(
        profile.max_activations_per_block, 1,
        "multi-job state must not increase q-forming apply work"
    );
    assert_eq!(profile.max_reference_currencies, 8);

    let limits = poc_schema_limits();
    let canonical = profile.encode_canonical(&limits).unwrap();
    assert_eq!(
        outbe_ocomp_protocol::profile::CapacityProfileV1::decode_canonical(&canonical, &limits)
            .unwrap(),
        profile
    );
}

#[test]
fn ocm_cap_001_rejects_nonconforming_or_retried_measurements() {
    let mut four_runs = evidence(800);
    four_runs.runs.pop();
    assert_eq!(
        four_runs.verify().unwrap_err(),
        CapacityEvidenceError::InvalidRunCount
    );

    let mut retried = evidence(800);
    retried.runs[2].retried = true;
    assert_eq!(
        retried.verify().unwrap_err(),
        CapacityEvidenceError::RetriedRun { ordinal: 3 }
    );

    let mut insufficient_disk = evidence(800);
    insufficient_disk.machine.free_workspace_bytes -= 1;
    assert_eq!(
        insufficient_disk.verify().unwrap_err(),
        CapacityEvidenceError::MachineProfile {
            fact: "free workspace"
        }
    );
}

#[test]
fn ocm_cap_001_applies_the_twenty_percent_boundary_exactly() {
    evidence(800).verify().unwrap();
    let error = evidence(801).verify().unwrap_err();
    assert!(matches!(
        error,
        CapacityEvidenceError::InsufficientHeadroom {
            ordinal: 1,
            dimension: "transaction bytes",
            used: 801,
            available: 1_000,
        }
    ));
}

#[test]
fn ocm_cap_001_rejects_a_run_missing_required_observed_dimensions() {
    for clear in [
        |bill: &mut CapacityWorkBillV1| bill.cpu_micros = 0,
        |bill: &mut CapacityWorkBillV1| bill.disk_write_bytes = 0,
        |bill: &mut CapacityWorkBillV1| bill.finality_latency_micros = 0,
    ] {
        let mut incomplete = evidence(800);
        clear(&mut incomplete.runs[0].work);
        assert_eq!(
            incomplete.verify().unwrap_err(),
            CapacityEvidenceError::ZeroWorkBill { ordinal: 1 }
        );
    }
}

#[test]
fn ocm_cap_001_capacity_evidence_has_a_strict_typed_json_boundary() {
    let expected = evidence(800);
    let encoded = serde_json::to_vec(&expected).unwrap();
    let decoded: CapacityEvidenceV1 = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, expected);
    decoded.verify().unwrap();

    let mut unknown: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("unmeasured_guess".to_owned(), serde_json::json!(1));
    assert!(serde_json::from_value::<CapacityEvidenceV1>(unknown).is_err());
}

#[test]
fn ocm_cap_001_rejects_a_cold_run_not_bound_to_the_public_s_plus_one_path() {
    let mut wrong_population = evidence(800);
    wrong_population.runs[3].binding.tribute_count = 256;
    assert_eq!(
        wrong_population.verify().unwrap_err(),
        CapacityEvidenceError::InvalidPublicCapacityBinding { ordinal: 4 }
    );

    let mut reused_scenario = evidence(800);
    reused_scenario.runs[4].binding.scenario_evidence_sha256 =
        reused_scenario.runs[0].binding.scenario_evidence_sha256;
    assert_eq!(
        reused_scenario.verify().unwrap_err(),
        CapacityEvidenceError::ReusedScenarioEvidence
    );

    let mut replay_misses_quorum = evidence(800);
    replay_misses_quorum.runs[1]
        .binding
        .historical_replay
        .first_missing_block_number = 41;
    assert_eq!(
        replay_misses_quorum.verify().unwrap_err(),
        CapacityEvidenceError::InvalidHistoricalReplayBinding { ordinal: 2 }
    );

    let mut invented_validator_timing = evidence(800);
    invented_validator_timing.runs[2]
        .binding
        .validator_block_processing[2]
        .block_hash = B256::repeat_byte(1);
    assert_eq!(
        invented_validator_timing.verify().unwrap_err(),
        CapacityEvidenceError::InvalidPublicCapacityBinding { ordinal: 3 }
    );

    let mut recovered_other_generation = evidence(800);
    recovered_other_generation.runs[3]
        .binding
        .historical_replay
        .recovered_result_digest = B256::repeat_byte(1);
    assert_eq!(
        recovered_other_generation.verify().unwrap_err(),
        CapacityEvidenceError::InvalidHistoricalReplayBinding { ordinal: 4 }
    );
}

#[test]
fn ocm_cap_001_internal_work_uses_the_frozen_checked_q_forming_formula() {
    let limits = OCOMP_POC_CANDIDATE_LIMITS_V1;
    let vote_cap = usize::try_from(limits.max_result_vote_bytes).unwrap();
    let expected = limits.activation_base_gas
        + limits.activation_per_input_byte_gas * limits.max_result_vote_bytes
        + limits.activation_per_root_transition_gas * 4
        + limits.activation_per_signature_gas;
    assert_eq!(result_vote_internal_work(vote_cap).unwrap(), expected);
    assert!(expected <= limits.max_activation_internal_work);
    assert_eq!(
        result_vote_internal_work(vote_cap + 1).unwrap_err(),
        CapacityEvidenceError::ResultVoteBytesExceeded
    );
}
