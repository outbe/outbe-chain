//! Metadosis mutation authority for system-transaction envelopes.
//!
//! The production host decides which native mutation each envelope may perform.

use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_evm::precompiles::MetadosisMutationCall;
use outbe_evm::EvmRuntimeHost;
use outbe_primitives::{
    addresses::{OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_ADDRESS},
    consensus::{DkgBoundaryArtifact, ReshareResult},
    reshare_artifact::LateFinalizeCreditsArtifact,
    storage::{
        metadosis_advance_due_binding, metadosis_cycle_allocation_binding,
        metadosis_init_genesis_binding, metadosis_process_ready_binding,
        MetadosisMutationEntitlements, MetadosisMutationPurposeTag as Purpose,
    },
    system_tx::SystemTx,
};

const TEST_CHAIN_ID: u64 = 42;
const TEST_BLOCK_NUMBER: u64 = 9;
const TEST_TIMESTAMP: u64 = 1_704_067_200;

fn encoded(input: SystemTx) -> Bytes {
    input.encode().expect("valid system-tx fixture")
}

fn boundary() -> DkgBoundaryArtifact {
    DkgBoundaryArtifact {
        epoch: 8,
        dkg_cycle: 2,
        freeze_height: 40,
        planned_activation_height: 42,
        target_set_hash: B256::repeat_byte(0x33),
        vrf_material_version: 3,
        vrf_group_public_key: B256::repeat_byte(0x44),
        vrf_group_public_key_bytes: Bytes::from(vec![0x44; 96]),
        committee_set_hash: B256::repeat_byte(0x66),
        is_validator_set_change: true,
        outcome: Bytes::from_static(b"boundary"),
        is_full_dkg: false,
        reshare: ReshareResult {
            new_active_set: vec![Address::repeat_byte(0x33)],
            active_set_hash: B256::repeat_byte(0x55),
        },
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
    }
}

fn entitlements(call: MetadosisMutationCall<'_>) -> MetadosisMutationEntitlements {
    outbe_runtime::OutbeRuntime::new().mutation_entitlements(call)
}

fn system_entitlements(input: SystemTx) -> MetadosisMutationEntitlements {
    let cycle_active_utc_day = matches!(input, SystemTx::BeginBlock { step_index: 8 }).then(|| {
        outbe_primitives::time::previous_date_key(outbe_primitives::time::timestamp_to_date_key(
            TEST_TIMESTAMP,
        ))
        .expect("date step")
    });
    let data = encoded(input);
    entitlements(MetadosisMutationCall {
        address: OUTBE_SYSTEM_TX_ADDRESS,
        data: data.as_ref(),
        caller: SYSTEM_ADDRESS,
        is_static: false,
        value: U256::ZERO,
        chain_id: TEST_CHAIN_ID,
        block_number: TEST_BLOCK_NUMBER,
        timestamp: TEST_TIMESTAMP,
        cycle_active_utc_day,
    })
}

fn expected_cycle_entitlements() -> MetadosisMutationEntitlements {
    let current_day = outbe_primitives::time::timestamp_to_date_key(TEST_TIMESTAMP);
    let previous_day = outbe_primitives::time::previous_date_key(current_day).expect("date step");
    let allocation_timestamp =
        outbe_primitives::time::date_key_to_utc_timestamp(previous_day).expect("date timestamp");
    MetadosisMutationEntitlements::exact(
        Purpose::CycleLifecycle,
        metadosis_cycle_allocation_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, allocation_timestamp),
    )
    .union(MetadosisMutationEntitlements::exact(
        Purpose::CycleLifecycle,
        metadosis_advance_due_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP),
    ))
    .union(MetadosisMutationEntitlements::exact(
        Purpose::CycleLifecycle,
        metadosis_process_ready_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP),
    ))
}

#[test]
fn protocol_cycle_entitles_genesis_initialization_at_block_one() {
    let data = encoded(SystemTx::BeginBlock { step_index: 8 });
    let granted = entitlements(MetadosisMutationCall {
        address: OUTBE_SYSTEM_TX_ADDRESS,
        data: data.as_ref(),
        caller: SYSTEM_ADDRESS,
        is_static: false,
        value: U256::ZERO,
        chain_id: TEST_CHAIN_ID,
        block_number: 1,
        timestamp: TEST_TIMESTAMP,
        cycle_active_utc_day: Some(outbe_primitives::time::timestamp_to_date_key(
            TEST_TIMESTAMP,
        )),
    });

    assert!(granted.expects(
        Purpose::CycleLifecycle,
        metadosis_init_genesis_binding(TEST_CHAIN_ID, 1, TEST_TIMESTAMP),
    ));
}

#[test]
fn exact_production_causes_receive_only_their_purpose() {
    assert_eq!(
        system_entitlements(SystemTx::BeginBlock { step_index: 8 }),
        expected_cycle_entitlements(),
    );
    assert_eq!(
        system_entitlements(SystemTx::Consensus {
            step_index: outbe_primitives::system_tx::CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX,
            payload: outbe_primitives::reshare_artifact::encode_late_finalize_credits_artifact(
                &LateFinalizeCreditsArtifact::default()
            )
            .unwrap()
        }),
        MetadosisMutationEntitlements::NONE,
    );
    assert_eq!(
        system_entitlements(SystemTx::Consensus {
            step_index: outbe_primitives::system_tx::CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
            payload: outbe_primitives::reshare_artifact::encode_boundary_artifact(&boundary())
                .unwrap()
        }),
        MetadosisMutationEntitlements::NONE,
    );
}

#[test]
fn protocol_cycle_grants_only_oldest_unallocated_day_after_a_multi_day_halt() {
    let block_day = outbe_primitives::time::timestamp_to_date_key(TEST_TIMESTAMP);
    let mut active_day = block_day;
    for _ in 0..6 {
        active_day = outbe_primitives::time::previous_date_key(active_day).expect("date step");
    }
    let data = encoded(SystemTx::BeginBlock { step_index: 8 });
    let granted = entitlements(MetadosisMutationCall {
        address: OUTBE_SYSTEM_TX_ADDRESS,
        data: data.as_ref(),
        caller: SYSTEM_ADDRESS,
        is_static: false,
        value: U256::ZERO,
        chain_id: TEST_CHAIN_ID,
        block_number: TEST_BLOCK_NUMBER,
        timestamp: TEST_TIMESTAMP,
        cycle_active_utc_day: Some(active_day),
    });

    assert!(granted.expects(
        Purpose::CycleLifecycle,
        metadosis_cycle_allocation_binding(
            TEST_CHAIN_ID,
            TEST_BLOCK_NUMBER,
            outbe_primitives::time::date_key_to_utc_timestamp(active_day).expect("date timestamp"),
        ),
    ));
    let mut day = outbe_primitives::time::next_date_key(active_day).expect("date step");
    while day < block_day {
        assert!(!granted.expects(
            Purpose::CycleLifecycle,
            metadosis_cycle_allocation_binding(
                TEST_CHAIN_ID,
                TEST_BLOCK_NUMBER,
                outbe_primitives::time::date_key_to_utc_timestamp(day).expect("date timestamp"),
            ),
        ));
        day = outbe_primitives::time::next_date_key(day).expect("date step");
    }
    assert!(granted.expects(
        Purpose::CycleLifecycle,
        metadosis_process_ready_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP),
    ));
}

#[test]
fn end_zone_walks_receive_only_process_ready() {
    let ready = metadosis_process_ready_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP);
    for input in [
        SystemTx::EndBlock { step_index: 0 },
        SystemTx::EndBlock { step_index: 1 },
        SystemTx::EndBlock { step_index: 2 },
        SystemTx::EndBlock { step_index: 3 },
        SystemTx::EndBlock { step_index: 4 },
    ] {
        let granted = system_entitlements(input);
        assert!(granted.expects(Purpose::CycleLifecycle, ready));
        assert!(!granted.expects(
            Purpose::CycleLifecycle,
            metadosis_advance_due_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP),
        ));
    }
}

#[test]
fn route_or_envelope_mismatch_grants_no_mutation_authority() {
    let cycle = encoded(SystemTx::BeginBlock { step_index: 8 });
    for (address, caller, is_static, value) in [
        (Address::repeat_byte(1), SYSTEM_ADDRESS, false, U256::ZERO),
        (
            OUTBE_SYSTEM_TX_ADDRESS,
            Address::repeat_byte(2),
            false,
            U256::ZERO,
        ),
        (OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_ADDRESS, true, U256::ZERO),
        (
            OUTBE_SYSTEM_TX_ADDRESS,
            SYSTEM_ADDRESS,
            false,
            U256::from(1),
        ),
    ] {
        assert_eq!(
            entitlements(MetadosisMutationCall {
                address,
                data: cycle.as_ref(),
                caller,
                is_static,
                value,
                chain_id: TEST_CHAIN_ID,
                block_number: TEST_BLOCK_NUMBER,
                timestamp: TEST_TIMESTAMP,
                cycle_active_utc_day: None,
            }),
            MetadosisMutationEntitlements::NONE
        );
    }
    assert_eq!(
        entitlements(MetadosisMutationCall {
            address: OUTBE_SYSTEM_TX_ADDRESS,
            data: b"malformed",
            caller: SYSTEM_ADDRESS,
            is_static: false,
            value: U256::ZERO,
            chain_id: TEST_CHAIN_ID,
            block_number: TEST_BLOCK_NUMBER,
            timestamp: TEST_TIMESTAMP,
            cycle_active_utc_day: None,
        }),
        MetadosisMutationEntitlements::NONE
    );

    assert_eq!(
        system_entitlements(SystemTx::BeginBlock { step_index: 6 }),
        MetadosisMutationEntitlements::NONE,
    );
}

// Trailing bytes must not acquire finality authority even when the artifact decodes.
#[test]
fn noncanonical_late_credits_rejected_by_validation_and_authority() {
    use outbe_primitives::{
        block::BlockHeightRegime,
        reshare_artifact::{encode_late_finalize_credits_artifact, OutbeBlockArtifacts},
        system_tx::CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX,
        system_tx_context::SystemTxValidationContext,
    };
    let mut payload =
        encode_late_finalize_credits_artifact(&LateFinalizeCreditsArtifact::default())
            .unwrap()
            .to_vec();
    let artifacts = OutbeBlockArtifacts::default();
    let ctx = SystemTxValidationContext {
        regime: BlockHeightRegime::SteadyState,
        block_number: TEST_BLOCK_NUMBER,
        parent_hash: B256::ZERO,
        artifacts: &artifacts,
    };
    let step = &outbe_runtime::BLOCK_ORDER.consensus
        [usize::from(CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX)];
    assert!((step.validate_payload)(&payload, &ctx).is_ok());
    payload.push(0xff);
    assert!((step.validate_payload)(&payload, &ctx).is_err());
    assert_eq!(
        system_entitlements(SystemTx::Consensus {
            step_index: CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX,
            payload: payload.into(),
        }),
        MetadosisMutationEntitlements::NONE
    );
}
