use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::{FinalizedJobSpecV1, FinalizedJobSummaryV1},
    intent::{
        ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        FrozenMetadosisValuesV1, JobIntentV1, MetadosisAttemptPreconditionV1,
        MetadosisExpectedStatus, NodTargetPreconditionV1, TributeInputBindingV1,
    },
    profile::{poc_schema_limits, ProtocolBundleV1},
};

pub struct FixtureJobIdentity {
    pub seed: u8,
    pub cursor: u64,
    pub chain_id: u64,
    pub genesis_hash: B256,
}

pub struct FixtureJobTiming {
    pub open_height: u64,
    pub deadline_height: u64,
}

pub fn finalized_single_tribute_job(
    identity: FixtureJobIdentity,
    bundle: &ProtocolBundleV1,
    timing: impl FnOnce() -> FixtureJobTiming,
) -> FinalizedJobSpecV1 {
    let limits = poc_schema_limits();
    let intent = single_tribute_intent(&identity, bundle);
    let finalized = (
        seeded_hash(identity.seed, 12),
        seeded_hash(identity.seed, 13),
    );
    let job_id = intent.job_id(finalized.0, finalized.1, &limits).unwrap();
    let intent_id = intent.intent_id(&limits).unwrap();
    let timing = timing();
    FinalizedJobSpecV1 {
        summary: FinalizedJobSummaryV1 {
            cursor: identity.cursor,
            job_id,
            intent_id,
            finalized_block_hash: finalized.0,
            finalized_state_root: finalized.1,
            protocol_bundle_hash: intent.protocol_bundle_hash,
            open_height: timing.open_height,
            deadline_height: timing.deadline_height,
        },
        canonical_job_intent: BoundedBytes(intent.encode_canonical(&limits).unwrap()),
    }
}

fn seeded_hash(seed: u8, offset: u8) -> B256 {
    let byte = seed.wrapping_add(offset);
    B256::repeat_byte(if byte == 0 { 0xff } else { byte })
}

fn single_tribute_intent(identity: &FixtureJobIdentity, bundle: &ProtocolBundleV1) -> JobIntentV1 {
    let day = 20_260_901;
    let nominal = U256::ONE;
    let collection = (seeded_hash(identity.seed, 2), seeded_hash(identity.seed, 3));
    JobIntentV1 {
        chain_id: identity.chain_id,
        genesis_hash: identity.genesis_hash,
        fork_id: bundle.fork_id,
        wwd: day,
        pending_nonce: 0,
        attempt: 0,
        protocol_bundle_hash: bundle.protocol_bundle_hash(&poc_schema_limits()).unwrap(),
        ce_sealed_root: seeded_hash(identity.seed, 5),
        sealed_tribute_collection_key: collection.0,
        sealed_tribute_collection_root: collection.1,
        authenticated_day_count: 1,
        authenticated_day_nominal: nominal,
        pre_admission_envelope_hash: seeded_hash(identity.seed, 6),
        source_availability_policy_id: seeded_hash(identity.seed, 7),
        frozen_metadosis_values: frozen_values(nominal, seeded_hash(identity.seed, 8)),
        logical_evaluation_height: identity.cursor,
        logical_evaluation_time: identity.cursor,
        activation_preconditions: single_tribute_preconditions(
            day,
            nominal,
            collection,
            seeded_hash(identity.seed, 9),
        ),
        result_validator_set_epoch: 1,
        result_committee_set_hash: seeded_hash(identity.seed, 10),
        result_ocomp_binding_hash: seeded_hash(identity.seed, 11),
        result_member_count: 4,
        result_quorum_threshold: 3,
        custody_committee_epoch_hash: None,
    }
}

fn frozen_values(nominal: U256, receipt_hash: B256) -> FrozenMetadosisValuesV1 {
    FrozenMetadosisValuesV1 {
        day_type: DayType::Green,
        day_limit: nominal,
        previous_vwap: nominal,
        current_vwap: nominal,
        gratis_demand: U256::ZERO,
        day_gratis_limit_minor: U256::ZERO,
        lysis_limit_minor: nominal,
        desis_limit_minor: U256::ZERO,
        request_limit_split_receipt_hash: receipt_hash,
    }
}

fn single_tribute_preconditions(
    day: u32,
    nominal: U256,
    collection: (B256, B256),
    nod_root: B256,
) -> ActivationPreconditionsV1 {
    ActivationPreconditionsV1 {
        tribute: tribute_binding(day, nominal, collection),
        nod: nod_precondition(day, nod_root),
        contributors: contributor_precondition(day, nominal),
        metadosis: pending_metadosis_precondition(day),
    }
}

fn tribute_binding(day: u32, nominal: U256, collection: (B256, B256)) -> TributeInputBindingV1 {
    TributeInputBindingV1 {
        wwd: day,
        source_generation: 1,
        collection_key: collection.0,
        sealed_collection_root: collection.1,
        exact_count: 1,
        exact_nominal_total: nominal,
    }
}

fn nod_precondition(wwd: u32, namespace_root_before: B256) -> NodTargetPreconditionV1 {
    NodTargetPreconditionV1 {
        wwd,
        target_generation: 1,
        namespace_root_before,
        max_nod_count: 1,
    }
}
fn contributor_precondition(worldwide_day: u32, nominal: U256) -> ContributorTargetPreconditionV1 {
    ContributorTargetPreconditionV1 {
        worldwide_day,
        expected_series_version: 1,
        max_contributor_count: 1,
        max_eligible_nominal_total: nominal,
    }
}
fn pending_metadosis_precondition(wwd: u32) -> MetadosisAttemptPreconditionV1 {
    MetadosisAttemptPreconditionV1 {
        wwd,
        pending_nonce: 0,
        expected_status: MetadosisExpectedStatus::OffchainPending,
        state_version: 1,
    }
}
