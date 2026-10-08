mod protocol {
    include!("../../src/test_support/protocol.rs");
}

use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::{FinalizedJobSpecV1, FinalizedJobSummaryV1},
    intent::{
        ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
    },
    profile::poc_schema_limits,
    profile::ProtocolBundleV1,
};

/// Keep system temp-directory symlinks outside storage path validation.
#[allow(dead_code)]
pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)
}

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(if byte == 0 { 0xff } else { byte })
}

pub fn protocol_bundle() -> ProtocolBundleV1 {
    protocol::protocol_bundle_fixture()
}

#[allow(dead_code)]
pub fn finalized_job_spec(
    seed: u8,
    cursor: u64,
    chain_id: u64,
    genesis_hash: B256,
) -> FinalizedJobSpecV1 {
    let limits = poc_schema_limits();
    let day = 20_260_901_u32;
    let bundle = protocol_bundle();
    let protocol_bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let collection_key = hash(seed.wrapping_add(2));
    let collection_root = hash(seed.wrapping_add(3));
    let nominal = U256::from(1);
    let intent = JobIntentV1 {
        chain_id,
        genesis_hash,
        fork_id: bundle.fork_id,
        wwd: day,
        pending_nonce: 0,
        attempt: 0,
        protocol_bundle_hash,
        ce_sealed_root: hash(seed.wrapping_add(5)),
        sealed_tribute_collection_key: collection_key,
        sealed_tribute_collection_root: collection_root,
        authenticated_day_count: 1,
        authenticated_day_nominal: nominal,
        pre_admission_envelope_hash: hash(seed.wrapping_add(6)),
        source_availability_policy_id: hash(seed.wrapping_add(7)),
        frozen_metadosis_values: FrozenMetadosisValuesV1 {
            day_type: DayType::Green,
            day_limit: nominal,
            previous_vwap: nominal,
            current_vwap: nominal,
            gratis_demand: U256::ZERO,
            day_gratis_limit_minor: U256::ZERO,
            lysis_limit_minor: nominal,
            desis_limit_minor: U256::ZERO,
            request_limit_split_receipt_hash: hash(seed.wrapping_add(8)),
        },
        logical_evaluation_height: cursor,
        logical_evaluation_time: cursor,
        activation_preconditions: ActivationPreconditionsV1 {
            tribute: TributeInputBindingV1 {
                wwd: day,
                source_generation: 1,
                collection_key,
                sealed_collection_root: collection_root,
                exact_count: 1,
                exact_nominal_total: nominal,
            },
            nod: NodTargetPreconditionV1 {
                wwd: day,
                target_generation: 1,
                namespace_root_before: hash(seed.wrapping_add(9)),
                max_nod_count: 1,
            },
            contributors: ContributorTargetPreconditionV1 {
                worldwide_day: day,
                expected_series_version: 1,
                max_contributor_count: 1,
                max_eligible_nominal_total: nominal,
            },
            metadosis: MetadosisAttemptPreconditionV1 {
                wwd: day,
                pending_nonce: 0,
                expected_status: MetadosisExpectedStatus::OffchainPending,
                state_version: 1,
            },
        },
        result_validator_set_epoch: 1,
        result_committee_set_hash: hash(seed.wrapping_add(10)),
        result_ocomp_binding_hash: hash(seed.wrapping_add(11)),
        result_member_count: 4,
        result_quorum_threshold: 3,
        custody_committee_epoch_hash: None,
    };
    let finalized_block_hash = hash(seed.wrapping_add(12));
    let finalized_state_root = hash(seed.wrapping_add(13));
    FinalizedJobSpecV1 {
        summary: FinalizedJobSummaryV1 {
            cursor,
            job_id: intent
                .job_id(finalized_block_hash, finalized_state_root, &limits)
                .unwrap(),
            intent_id: intent.intent_id(&limits).unwrap(),
            finalized_block_hash,
            finalized_state_root,
            protocol_bundle_hash,
            open_height: cursor + 1,
            deadline_height: cursor + 1_801,
        },
        canonical_job_intent: BoundedBytes(intent.encode_canonical(&limits).unwrap()),
    }
}
