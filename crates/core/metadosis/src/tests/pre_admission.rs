use alloy_primitives::{B256, U256};
use outbe_fidelity::FidelityOcompProjection;
use outbe_ocomp_protocol::{codec::CodecLimits, profile::CapacityProfileV1, SchemaLimits};
use outbe_oracle::api::{OcompAuctionEntryPriceSource, OcompOraclePreAdmissionProjection};
use outbe_tribute::TributePreAdmissionProjection;

use crate::pre_admission::{
    evaluate_pre_admission, PreAdmissionContext, PreAdmissionDecision, PreAdmissionDeferredReason,
    PreAdmissionInputs,
};
use crate::tests::with_contract;

const SCHEMA_LIMITS: SchemaLimits = SchemaLimits {
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

fn capacity() -> CapacityProfileV1 {
    CapacityProfileV1 {
        profile_id: B256::repeat_byte(0x44),
        max_tributes_per_work_shard: 256,
        max_workers_per_domain: 4,
        max_pending_jobs: 2,
        max_intents_per_block: 1,
        max_activations_per_block: 1,
        max_ready_inspections_per_block: 4,
        max_expirations_per_block: 4,
        retry_backoff_blocks: 2,
        max_terminal_job_records: 365,
        max_reference_currencies: 16,
        max_fidelity_cohorts_per_owner: 64,
        max_oracle_wwd_pair_entries: 256,
        max_active_scurve_entries: 256,
        result_deadline_blocks: 10,
        source_retention_after_terminal_blocks: 64,
        generated_limits_manifest_hash: B256::repeat_byte(0x45),
    }
}

fn context() -> PreAdmissionContext {
    PreAdmissionContext {
        chain_id: 7,
        genesis_hash: B256::repeat_byte(0x11),
        fork_id: B256::repeat_byte(0x22),
        correctness_profile_id: B256::repeat_byte(0x33),
        capacity_profile: capacity(),
    }
}

fn inputs() -> PreAdmissionInputs {
    PreAdmissionInputs {
        tribute: TributePreAdmissionProjection {
            worldwide_day: 20260723u32.into(),
            source_generation: 0,
            profile_ready: true,
            is_sealed: true,
            sealed_collection_root: B256::repeat_byte(0x51),
            tribute_count: 256,
            tribute_nominal_amount: U256::from(1_000),
            canonical_body_bytes: 40_000,
            distinct_owner_count: 256,
            distinct_reference_currency_count: 16,
        },
        fidelity: FidelityOcompProjection {
            profile_ready: true,
            max_cohorts_per_owner: 64,
            max_cohorts_observed: 64,
        },
        oracle: OcompOraclePreAdmissionProjection {
            profile_ready: true,
            auction_entry_price: U256::from(12),
            auction_entry_price_source: OcompAuctionEntryPriceSource::LastClosedDayVwap,
            auction_entry_price_source_day: 20260722,
            oracle_state_version: 91,
            wwd_pair_entries: 256,
            active_scurve_entries: 256,
        },
    }
}

#[test]
fn pre_admission_never_turns_the_work_shard_size_into_a_total_tribute_cap() {
    let accepted = evaluate_pre_admission(&context(), &inputs()).unwrap();
    let PreAdmissionDecision::Eligible(envelope) = accepted else {
        panic!("exact capacity boundary must be eligible");
    };
    assert_eq!(envelope.sealed_tribute_count, 256);
    assert_eq!(envelope.max_fidelity_cohorts_observed, 64);
    assert_eq!(envelope.oracle_wwd_pair_entries_observed, 256);
    assert_eq!(envelope.active_scurve_entries_observed, 256);

    let mut over_tributes = inputs();
    over_tributes.tribute.tribute_count = 257;
    over_tributes.tribute.distinct_owner_count = 257;
    let PreAdmissionDecision::Eligible(envelope) =
        evaluate_pre_admission(&context(), &over_tributes).unwrap()
    else {
        panic!("work-shard cap plus one must start another shard");
    };
    assert_eq!(envelope.sealed_tribute_count, 257);

    let mut ten_thousand = inputs();
    ten_thousand.tribute.tribute_count = 10_000;
    ten_thousand.tribute.distinct_owner_count = 10_000;
    ten_thousand.tribute.canonical_body_bytes = 1_600_000;
    let PreAdmissionDecision::Eligible(envelope) =
        evaluate_pre_admission(&context(), &ten_thousand).unwrap()
    else {
        panic!("total Tribute population must not be bounded by work-shard capacity");
    };
    assert_eq!(envelope.sealed_tribute_count, 10_000);
    assert_eq!(envelope.fidelity_opening_upper_bound, 10_000);

    let mut over_fidelity = inputs();
    over_fidelity.fidelity.max_cohorts_observed = 65;
    assert!(matches!(
        evaluate_pre_admission(&context(), &over_fidelity).unwrap(),
        PreAdmissionDecision::Deferred(
            PreAdmissionDeferredReason::FidelityCohortCapExceeded { .. }
        )
    ));

    let mut over_oracle_pairs = inputs();
    over_oracle_pairs.oracle.wwd_pair_entries = 257;
    assert!(matches!(
        evaluate_pre_admission(&context(), &over_oracle_pairs).unwrap(),
        PreAdmissionDecision::Deferred(
            PreAdmissionDeferredReason::OracleWwdPairEntriesExceeded { .. }
        )
    ));

    let mut over_scurves = inputs();
    over_scurves.oracle.active_scurve_entries = 257;
    assert!(matches!(
        evaluate_pre_admission(&context(), &over_scurves).unwrap(),
        PreAdmissionDecision::Deferred(
            PreAdmissionDeferredReason::ActiveScurveEntriesExceeded { .. }
        )
    ));

    let mut oracle_not_ready = inputs();
    oracle_not_ready.oracle.profile_ready = false;
    assert_eq!(
        evaluate_pre_admission(&context(), &oracle_not_ready).unwrap(),
        PreAdmissionDecision::Deferred(PreAdmissionDeferredReason::OracleProfileNotReady)
    );

    let mut tribute_not_ready = inputs();
    tribute_not_ready.tribute.profile_ready = false;
    assert_eq!(
        evaluate_pre_admission(&context(), &tribute_not_ready).unwrap(),
        PreAdmissionDecision::Deferred(PreAdmissionDeferredReason::TributeProfileNotReady)
    );
}

#[test]
fn metadosis_seals_the_canonical_envelope_once_and_exposes_real_state() {
    let PreAdmissionDecision::Eligible(envelope) =
        evaluate_pre_admission(&context(), &inputs()).unwrap()
    else {
        panic!("fixture must be eligible");
    };
    let wwd = inputs().tribute.worldwide_day;
    let expected_hash = envelope.envelope_hash(&SCHEMA_LIMITS).unwrap();

    with_contract(|metadosis| {
        let empty = metadosis.ocomp_pre_admission_projection(wwd).unwrap();
        assert!(!empty.initialized);
        assert_eq!(empty.state_version, 0);
        assert_eq!(empty.envelope_hash, B256::ZERO);

        let initialized = metadosis.initialize_ocomp_pre_admission(wwd).unwrap();
        assert!(initialized.initialized);
        assert_eq!(initialized.state_version, 1);
        assert_eq!(initialized.envelope_hash, B256::ZERO);
        assert_eq!(
            metadosis.initialize_ocomp_pre_admission(wwd).unwrap(),
            initialized,
            "exact fork initialization must be idempotent"
        );

        let sealed = metadosis
            .seal_pre_admission_envelope(wwd, &envelope, &SCHEMA_LIMITS)
            .unwrap();
        assert_eq!(sealed.state_version, 2);
        assert_eq!(sealed.envelope_hash, expected_hash);
        assert_eq!(
            metadosis.ocomp_pre_admission_projection(wwd).unwrap(),
            sealed
        );

        let mut different = envelope;
        different.oracle_state_version += 1;
        assert!(metadosis
            .seal_pre_admission_envelope(wwd, &different, &SCHEMA_LIMITS)
            .is_err());
        assert_eq!(
            metadosis.ocomp_pre_admission_projection(wwd).unwrap(),
            sealed,
            "failed reseal must not mutate the committed state"
        );
    });
}
