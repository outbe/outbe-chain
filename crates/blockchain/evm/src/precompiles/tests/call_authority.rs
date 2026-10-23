use super::super::call_authority::{metadosis_mutation_entitlements, MetadosisMutationCall};
use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_primitives::{
    addresses::{OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_ADDRESS},
    consensus::{DkgBoundaryArtifact, ReshareResult},
    reshare_artifact::LateFinalizeCreditsArtifact,
    storage::{
        metadosis_cycle_allocation_binding, metadosis_init_genesis_binding,
        metadosis_late_settlement_binding, metadosis_process_ready_binding,
        MetadosisMutationEntitlements, MetadosisMutationPurposeTag as Purpose,
    },
    system_tx::SystemTxInputV2,
};

const TEST_CHAIN_ID: u64 = 42;
const TEST_BLOCK_NUMBER: u64 = 9;
const TEST_TIMESTAMP: u64 = 1_704_067_200;

fn encoded(input: SystemTxInputV2) -> Bytes {
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
        tee_recipient_pubkeys: Vec::new(),
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
    }
}

fn system_entitlements(input: SystemTxInputV2) -> MetadosisMutationEntitlements {
    let cycle_active_utc_day = matches!(input, SystemTxInputV2::CycleTick).then(|| {
        outbe_primitives::time::previous_date_key(outbe_primitives::time::timestamp_to_date_key(
            TEST_TIMESTAMP,
        ))
        .expect("date step")
    });
    let data = encoded(input);
    metadosis_mutation_entitlements(MetadosisMutationCall {
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
    let allocation_timestamp = outbe_primitives::time::date_key_to_utc_timestamp(previous_day);
    MetadosisMutationEntitlements::exact(
        Purpose::CycleLifecycle,
        metadosis_cycle_allocation_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, allocation_timestamp),
    )
    .union(MetadosisMutationEntitlements::exact(
        Purpose::CycleLifecycle,
        metadosis_process_ready_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP),
    ))
    .union(MetadosisMutationEntitlements::exact(
        Purpose::CertifiedFinality,
        metadosis_late_settlement_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP),
    ))
}

#[test]
fn protocol_cycle_entitles_genesis_initialization_at_block_one() {
    let data = encoded(SystemTxInputV2::CycleTick);
    let entitlements = metadosis_mutation_entitlements(MetadosisMutationCall {
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

    assert!(entitlements.expects(
        Purpose::CycleLifecycle,
        metadosis_init_genesis_binding(TEST_CHAIN_ID, 1, TEST_TIMESTAMP),
    ));
}

#[test]
fn exact_production_causes_receive_only_their_purpose() {
    assert_eq!(
        system_entitlements(SystemTxInputV2::CycleTick),
        expected_cycle_entitlements(),
    );
    assert_eq!(
        system_entitlements(SystemTxInputV2::LateFinalizeCredits {
            artifact: LateFinalizeCreditsArtifact::default(),
        }),
        MetadosisMutationEntitlements::exact(
            Purpose::CertifiedFinality,
            metadosis_late_settlement_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP,),
        ),
    );
    assert_eq!(
        system_entitlements(SystemTxInputV2::BoundaryOutcome {
            artifact: boundary(),
        }),
        MetadosisMutationEntitlements::NONE,
    );
}

#[test]
fn protocol_cycle_grants_no_daily_allocation_after_a_multi_day_halt() {
    let block_day = outbe_primitives::time::timestamp_to_date_key(TEST_TIMESTAMP);
    let mut active_day = block_day;
    for _ in 0..6 {
        active_day = outbe_primitives::time::previous_date_key(active_day).expect("date step");
    }
    let data = encoded(SystemTxInputV2::CycleTick);
    let entitlements = metadosis_mutation_entitlements(MetadosisMutationCall {
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

    let mut day = active_day;
    while day < block_day {
        assert!(!entitlements.expects(
            Purpose::CycleLifecycle,
            metadosis_cycle_allocation_binding(
                TEST_CHAIN_ID,
                TEST_BLOCK_NUMBER,
                outbe_primitives::time::date_key_to_utc_timestamp(day),
            ),
        ));
        day = outbe_primitives::time::next_date_key(day).expect("date step");
    }
    assert!(entitlements.expects(
        Purpose::CycleLifecycle,
        metadosis_process_ready_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP),
    ));
}

#[test]
fn route_or_envelope_mismatch_grants_no_mutation_authority() {
    let cycle = encoded(SystemTxInputV2::CycleTick);
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
            metadosis_mutation_entitlements(MetadosisMutationCall {
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
        metadosis_mutation_entitlements(MetadosisMutationCall {
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
        system_entitlements(SystemTxInputV2::OracleSlashWindow),
        MetadosisMutationEntitlements::NONE,
    );
}
