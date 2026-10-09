//! Independent storage encoding and real trie proof construction for test fixtures.

mod nod_materialization;
mod storage;

pub use nod_materialization::nod_action_population;
pub use storage::solidity_bytes_storage_slots;

use std::collections::BTreeMap;

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_trie::{
    proof::{ProofNodes, ProofRetainer},
    HashBuilder, Nibbles, TrieAccount,
};

use crate::{
    capacity::ObservedMachineFactsV1,
    codec::CodecLimits,
    common::{BoundedBytes, ProofBytes},
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    intent::{
        ActivationPreconditionsV1, CertifiedParentAccountingMetadataV2,
        ContributorTargetPreconditionV1, DayType, FrozenMetadosisValuesV1, JobIntentV1,
        MetadosisAttemptPreconditionV1, MetadosisExpectedStatus, NodTargetPreconditionV1,
        ParentProofKind, TributeInputBindingV1,
    },
    profile::{CapacityProfileV1, ProtocolBundleV1},
    result::{ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1, ResultRootsV1},
    schema::SchemaLimits,
};

const MAX_TRIBUTES_PER_WORK_SHARD: u32 = {
    let generated = OCOMP_POC_CANDIDATE_LIMITS_V1.max_tributes_per_work_shard;
    assert!(
        generated <= u32::MAX as u64,
        "generated tribute shard cap fits u32"
    );
    generated as u32
};

/// Fix the schema limits used by finality and input tests.
pub const FINALITY_INPUT_TEST_LIMITS: SchemaLimits = SchemaLimits {
    codec: CodecLimits::new(1_048_576, 4_096, 2_097_152),
    max_bounded_bytes: 262_144,
    max_proof_bytes: 262_144,
    max_opening_bytes: 262_144,
    max_collection_items: 4_096,
    max_action_items: 4_096,
    max_chunk_items: 4_096,
    max_unit_inputs: 64,
    max_result_chunk_bytes: 524_288,
    max_control_body_bytes: 262_144,
};

/// Derive a four-byte selector from a test signature.
#[must_use]
pub fn keccak_selector(signature: &str) -> [u8; 4] {
    let digest = keccak256(signature.as_bytes());
    let mut selector = [0_u8; 4];
    selector.copy_from_slice(&digest.0[..4]);
    selector
}

/// Return the minimal capacity profile used by protocol test fixtures.
#[must_use]
pub fn minimal_capacity_profile() -> CapacityProfileV1 {
    let generated = OCOMP_POC_CANDIDATE_LIMITS_V1;
    CapacityProfileV1 {
        profile_id: B256::repeat_byte(13),
        max_tributes_per_work_shard: MAX_TRIBUTES_PER_WORK_SHARD,
        max_workers_per_domain: 4,
        max_intents_per_block: 1,
        max_activations_per_block: 1,
        max_ready_inspections_per_block: 1,
        max_expirations_per_block: 1,
        ready_backoff_blocks: 1,
        max_reference_currencies: 1,
        max_oracle_wwd_pair_entries: 1,
        max_active_scurve_entries: 1,
        result_deadline_blocks: 10,
        source_retention_after_terminal_blocks: generated.source_retention_after_terminal_blocks,
        generated_limits_manifest_hash: B256::repeat_byte(30),
    }
}

/// Return the canonical protocol bundle used by protocol test fixtures.
#[must_use]
pub fn minimal_protocol_bundle() -> ProtocolBundleV1 {
    crate::profile::measurement_protocol_bundle_v1()
}

/// Build parent-accounting metadata for finality test fixtures.
#[must_use]
pub fn certified_parent_accounting(
    finalized_block_number: u64,
) -> CertifiedParentAccountingMetadataV2 {
    let hash = B256::repeat_byte;
    CertifiedParentAccountingMetadataV2 {
        finalized_block_number,
        finalized_block_hash: hash(46),
        finalized_epoch: 2,
        finalized_view: 3,
        parent_view: 2,
        ordered_committee: vec![BoundedBytes(vec![1])],
        signer_bitmap: BoundedBytes(vec![1]),
        canonical_commonware_finalization_proof: ProofBytes(vec![2]),
        committee_set_hash: hash(47),
        vrf_material_version: 1,
        vrf_group_public_key_hash: hash(48),
        proof_kind: ParentProofKind::Finalization,
        missed_proposers: Vec::new(),
    }
}

/// Source facts shared by an intent and its activation preconditions.
#[derive(Clone, Copy)]
pub struct ActivationFixtureSource {
    pub wwd: u32,
    pub pending_nonce: u64,
    pub tribute_source_generation: u64,
    pub collection_key: B256,
    pub sealed_collection_root: B256,
    pub exact_count: u32,
    pub exact_nominal_total: U256,
}

/// Target versions and namespace state for an activation fixture.
#[derive(Clone, Copy)]
pub struct ActivationFixtureTargets {
    pub nod_target_generation: u64,
    pub namespace_root_before: B256,
    pub contributor_series_version: u64,
    pub metadosis_state_version: u64,
}

/// Build preconditions whose day, source bounds, and nonce agree by construction.
#[must_use]
pub fn activation_preconditions_fixture(
    source: ActivationFixtureSource,
    targets: ActivationFixtureTargets,
) -> ActivationPreconditionsV1 {
    ActivationPreconditionsV1 {
        tribute: TributeInputBindingV1 {
            wwd: source.wwd,
            source_generation: source.tribute_source_generation,
            collection_key: source.collection_key,
            sealed_collection_root: source.sealed_collection_root,
            exact_count: source.exact_count,
            exact_nominal_total: source.exact_nominal_total,
        },
        nod: NodTargetPreconditionV1 {
            wwd: source.wwd,
            target_generation: targets.nod_target_generation,
            namespace_root_before: targets.namespace_root_before,
            max_nod_count: source.exact_count,
        },
        contributors: ContributorTargetPreconditionV1 {
            worldwide_day: source.wwd,
            expected_series_version: targets.contributor_series_version,
            max_contributor_count: source.exact_count,
            max_eligible_nominal_total: source.exact_nominal_total,
        },
        metadosis: MetadosisAttemptPreconditionV1 {
            wwd: source.wwd,
            pending_nonce: source.pending_nonce,
            expected_status: MetadosisExpectedStatus::OffchainPending,
            state_version: targets.metadosis_state_version,
        },
    }
}

/// The fixed activation preconditions shared by intent and receipt fixtures.
#[must_use]
pub fn fixed_activation_preconditions() -> ActivationPreconditionsV1 {
    let hash = B256::repeat_byte;
    activation_preconditions_fixture(
        ActivationFixtureSource {
            wwd: 7,
            pending_nonce: 0,
            tribute_source_generation: 3,
            collection_key: hash(30),
            sealed_collection_root: hash(31),
            exact_count: 1,
            exact_nominal_total: U256::ZERO,
        },
        ActivationFixtureTargets {
            nod_target_generation: 5,
            namespace_root_before: hash(32),
            contributor_series_version: 8,
            metadosis_state_version: 12,
        },
    )
}

/// The fields of a fixture job intent that the activation preconditions do
/// not fix.
pub struct JobIntentFixtureValues {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub fork_id: B256,
    pub attempt: u32,
    pub protocol_bundle_hash: B256,
    pub ce_sealed_root: B256,
    pub pre_admission_envelope_hash: B256,
    pub source_availability_policy_id: B256,
    pub frozen_metadosis_values: FrozenMetadosisValuesV1,
    pub logical_evaluation_height: u64,
    pub logical_evaluation_time: u64,
    pub result_validator_set_epoch: u64,
    pub result_committee_set_hash: B256,
    pub result_ocomp_binding_hash: B256,
    pub result_member_count: u16,
    pub result_quorum_threshold: u16,
    pub custody_committee_epoch_hash: Option<B256>,
}

/// Build a fixture job intent. Copy the fields bound to activation
/// preconditions from `activation_preconditions`. Copy the other fields from
/// `values`. Invalid caller-supplied preconditions can still fail validation.
/// This helper does not validate them.
#[must_use]
pub fn job_intent_fixture(
    activation_preconditions: ActivationPreconditionsV1,
    values: JobIntentFixtureValues,
) -> JobIntentV1 {
    JobIntentV1 {
        chain_id: values.chain_id,
        genesis_hash: values.genesis_hash,
        fork_id: values.fork_id,
        wwd: activation_preconditions.tribute.wwd,
        pending_nonce: activation_preconditions.metadosis.pending_nonce,
        attempt: values.attempt,
        protocol_bundle_hash: values.protocol_bundle_hash,
        ce_sealed_root: values.ce_sealed_root,
        sealed_tribute_collection_key: activation_preconditions.tribute.collection_key,
        sealed_tribute_collection_root: activation_preconditions.tribute.sealed_collection_root,
        authenticated_day_count: activation_preconditions.tribute.exact_count,
        authenticated_day_nominal: activation_preconditions.tribute.exact_nominal_total,
        pre_admission_envelope_hash: values.pre_admission_envelope_hash,
        source_availability_policy_id: values.source_availability_policy_id,
        frozen_metadosis_values: values.frozen_metadosis_values,
        logical_evaluation_height: values.logical_evaluation_height,
        logical_evaluation_time: values.logical_evaluation_time,
        activation_preconditions,
        result_validator_set_epoch: values.result_validator_set_epoch,
        result_committee_set_hash: values.result_committee_set_hash,
        result_ocomp_binding_hash: values.result_ocomp_binding_hash,
        result_member_count: values.result_member_count,
        result_quorum_threshold: values.result_quorum_threshold,
        custody_committee_epoch_hash: values.custody_committee_epoch_hash,
    }
}

/// Build the fixed finalized-intent input shared by protocol and RPC tests.
#[must_use]
pub fn fixed_job_intent() -> JobIntentV1 {
    let hash = B256::repeat_byte;
    job_intent_fixture(
        fixed_activation_preconditions(),
        JobIntentFixtureValues {
            chain_id: 42,
            genesis_hash: hash(40),
            fork_id: hash(1),
            attempt: 0,
            protocol_bundle_hash: hash(41),
            ce_sealed_root: hash(42),
            pre_admission_envelope_hash: hash(43),
            source_availability_policy_id: hash(44),
            frozen_metadosis_values: FrozenMetadosisValuesV1 {
                day_type: DayType::Green,
                day_limit: U256::ZERO,
                previous_vwap: U256::ZERO,
                current_vwap: U256::ZERO,
                gratis_demand: U256::ZERO,
                day_gratis_limit_minor: U256::ZERO,
                lysis_limit_minor: U256::ZERO,
                desis_limit_minor: U256::ZERO,
                request_limit_split_receipt_hash: hash(113),
            },
            logical_evaluation_height: 100,
            logical_evaluation_time: 1_000,
            result_validator_set_epoch: 1,
            result_committee_set_hash: hash(45),
            result_ocomp_binding_hash: hash(46),
            result_member_count: 4,
            result_quorum_threshold: 3,
            custody_committee_epoch_hash: None,
        },
    )
}

/// Return the fixed arithmetic summary for result tests.
#[must_use]
pub fn fixed_lysis_arithmetic_summary() -> LysisArithmeticSummaryV1 {
    let hash = B256::repeat_byte;
    LysisArithmeticSummaryV1 {
        input_manifest_hash: hash(54),
        plan_hash: hash(55),
        unit_artifact_root: hash(56),
        fidelity_fraction_root: hash(57),
        gratis_prefix_root: hash(58),
        roots: ResultRootsV1 {
            nod_root: hash(50),
            bucket_root: hash(51),
            contributor_root: hash(52),
            output_manifest_root: hash(53),
        },
        counts: ExactCountsV1 {
            tribute_count: 1,
            nod_count: 1,
            bucket_count: 0,
            contributor_count: 0,
            semantic_event_count: 0,
        },
        conservation: ConservationTotalsV1 {
            tribute_nominal_total: U256::ZERO,
            eligible_nominal_total: U256::ZERO,
            day_limit: U256::ZERO,
            gratis_demand: U256::ZERO,
            day_gratis_limit_minor: U256::ZERO,
            lysis_limit_minor: U256::ZERO,
            desis_limit_minor: U256::ZERO,
            lysis_allocation_minor: U256::ZERO,
            unused_lysis_limit_minor: U256::ZERO,
            carry_over_credit: U256::ZERO,
            nod_cost_total: U256::ZERO,
        },
        first_error_ordinal: None,
    }
}

/// Collect the retained proof nodes for a target in canonical path order.
pub fn proof_nodes_for_target(retained: &ProofNodes, target: &Nibbles) -> Vec<Bytes> {
    retained
        .matching_nodes_sorted(target)
        .into_iter()
        .map(|(_, node)| node)
        .collect()
}

/// Insert ordered leaves and retain proof nodes for the targets.
pub fn trie_root_with_proof_nodes(
    targets: impl IntoIterator<Item = Nibbles>,
    leaves: impl IntoIterator<Item = (Nibbles, Vec<u8>)>,
) -> (B256, ProofNodes) {
    let mut builder = HashBuilder::default().with_proof_retainer(ProofRetainer::from_iter(targets));
    for (path, value) in leaves {
        builder.add_leaf(path, &value);
    }
    let root = builder.root();
    let retained = builder.take_proof_nodes();
    (root, retained)
}

/// Build an account trie and return each proof by account address.
pub fn account_mpt_with_proofs(
    accounts: &[(Address, TrieAccount)],
) -> (B256, BTreeMap<Address, Vec<Bytes>>) {
    let targets = accounts
        .iter()
        .map(|(address, _)| (*address, Nibbles::unpack(keccak256(address))))
        .collect::<BTreeMap<_, _>>();
    let mut leaves = accounts
        .iter()
        .map(|(address, account)| (targets[address], alloy_rlp::encode(*account)))
        .collect::<Vec<_>>();
    leaves.sort_by_key(|(path, _)| *path);
    let (root, retained) = trie_root_with_proof_nodes(targets.values().copied(), leaves);
    let proofs = targets
        .into_iter()
        .map(|(address, target)| {
            let proof = proof_nodes_for_target(&retained, &target);
            (address, proof)
        })
        .collect();
    (root, proofs)
}

/// Build canonical storage-root proofs in the supplied slot order.
pub fn storage_trie(slots: &[(U256, U256)]) -> (B256, Vec<Vec<Bytes>>) {
    let targets = slots
        .iter()
        .map(|(slot, _)| Nibbles::unpack(keccak256(slot.to_be_bytes::<32>())))
        .collect::<Vec<_>>();
    let mut leaves = BTreeMap::new();
    for ((_, word), target) in slots.iter().zip(&targets) {
        if !word.is_zero() {
            leaves.insert(*target, alloy_rlp::encode_fixed_size(word).to_vec());
        }
    }

    let (root, retained) = trie_root_with_proof_nodes(targets.clone(), leaves);
    let proofs = targets
        .iter()
        .map(|target| proof_nodes_for_target(&retained, target))
        .collect();
    (root, proofs)
}

/// Return observed machine facts used by capacity evidence fixtures.
#[must_use]
pub fn conforming_capacity_machine() -> ObservedMachineFactsV1 {
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
        production_enclave_sgx_no_attest: true,
    }
}
