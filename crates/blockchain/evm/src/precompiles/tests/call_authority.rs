use super::super::call_authority::{
    metadosis_mutation_entitlements, MetadosisMutationCall, ResultVoteCall,
};
use super::super::map_outbe_precompile_result;
use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_ocomp_protocol::abi::{
    GET_OFFCHAIN_JOB_SELECTOR, METADOSIS_ADDRESS, SUBMIT_LYSIS_RESULT_SELECTOR,
};
use outbe_primitives::{
    addresses::{OUTBE_SYSTEM_TX_ADDRESS, SYSTEM_ADDRESS},
    consensus::{DkgBoundaryArtifact, ReshareResult},
    consensus_metadata::CertifiedParentAccountingMetadata,
    reshare_artifact::LateFinalizeCreditsArtifact,
    storage::{
        metadosis_cycle_allocation_binding, metadosis_init_genesis_binding,
        metadosis_late_settlement_binding, metadosis_ocomp_lifecycle_begin_binding,
        metadosis_ocomp_terminal_request_binding, metadosis_process_ready_binding,
        metadosis_verified_vote_binding, MetadosisCertifiedFinalityBinding,
        MetadosisMutationEntitlements, MetadosisMutationPurposeTag as Purpose,
    },
    system_tx::SystemTxInputV2,
};

const TEST_CHAIN_ID: u64 = 42;
const TEST_BLOCK_NUMBER: u64 = 9;
const TEST_TIMESTAMP: u64 = 1_704_067_200;

fn certified_root() -> B256 {
    B256::repeat_byte(0xa1)
}

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

fn system_entitlements(
    input: SystemTxInputV2,
    lifecycle_active: bool,
) -> MetadosisMutationEntitlements {
    let cycle_active_utc_day = matches!(input, SystemTxInputV2::CycleTick).then(|| {
        outbe_primitives::time::previous_date_key(outbe_primitives::time::timestamp_to_date_key(
            TEST_TIMESTAMP,
        ))
    });
    let data = encoded(input);
    metadosis_mutation_entitlements(MetadosisMutationCall {
        address: OUTBE_SYSTEM_TX_ADDRESS,
        data: data.as_ref(),
        caller: SYSTEM_ADDRESS,
        is_static: false,
        value: U256::ZERO,
        ocomp_lifecycle_active: lifecycle_active,
        result_vote: ResultVoteCall::NotVote,
        chain_id: TEST_CHAIN_ID,
        block_number: TEST_BLOCK_NUMBER,
        timestamp: TEST_TIMESTAMP,
        cycle_active_utc_day,
        preloaded_certified_state_root: Some(certified_root()),
        ocomp_fork_install: None,
    })
}

fn expected_cycle_entitlements() -> MetadosisMutationEntitlements {
    let current_day = outbe_primitives::time::timestamp_to_date_key(TEST_TIMESTAMP);
    let previous_day = outbe_primitives::time::previous_date_key(current_day);
    let allocation_timestamp = outbe_primitives::time::date_key_to_utc_timestamp(previous_day);
    MetadosisMutationEntitlements::exact(
        Purpose::CycleLifecycle,
        metadosis_cycle_allocation_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, allocation_timestamp),
    )
    .union(MetadosisMutationEntitlements::exact(
        Purpose::CycleLifecycle,
        metadosis_process_ready_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP),
    ))
}

#[test]
fn protocol_cycle_entitles_genesis_initialization_at_fallback_activation_height() {
    let data = encoded(SystemTxInputV2::CycleTick);
    let entitlements = metadosis_mutation_entitlements(MetadosisMutationCall {
        address: OUTBE_SYSTEM_TX_ADDRESS,
        data: data.as_ref(),
        caller: SYSTEM_ADDRESS,
        is_static: false,
        value: U256::ZERO,
        ocomp_lifecycle_active: false,
        result_vote: ResultVoteCall::NotVote,
        chain_id: TEST_CHAIN_ID,
        block_number: 1,
        timestamp: TEST_TIMESTAMP,
        cycle_active_utc_day: Some(outbe_primitives::time::timestamp_to_date_key(
            TEST_TIMESTAMP,
        )),
        preloaded_certified_state_root: None,
        ocomp_fork_install: None,
    });

    assert!(entitlements.expects(
        Purpose::CycleLifecycle,
        metadosis_init_genesis_binding(TEST_CHAIN_ID, 1, TEST_TIMESTAMP),
    ));
}

#[test]
fn inactive_lysis_selector_does_not_abort_block_execution() {
    use alloy_sol_types::SolCall;
    use outbe_primitives::storage::gas::PRECOMPILE_BASE_GAS;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;

    let call = outbe_metadosis::precompile::IMetadosis::submitLysisResultCall {
        resultVoteV1: Bytes::from(vec![0_u8; 8]),
    };
    let mut provider = HashMapStorageProvider::new(TEST_CHAIN_ID);
    let result = StorageHandle::enter(&mut provider, |storage| {
        outbe_metadosis::precompile::dispatch(
            storage,
            &call.abi_encode(),
            Address::ZERO,
            U256::ZERO,
        )
    });

    // With the OCOMP lifecycle inactive, the selector reaches the view
    // dispatcher. The mapped outcome must be an ordinary revert output.
    // An `Err` here becomes a revm `Fatal` that aborts the whole payload
    // build for a transaction any external account can submit.
    let output = map_outbe_precompile_result(result, PRECOMPILE_BASE_GAS)
        .expect("inactive lysis vote must map to a revert, not a block-aborting error");
    assert!(output.is_revert());
    let mut expected = Vec::with_capacity(36);
    expected.extend_from_slice(&outbe_ocomp_protocol::abi::OCOMP_RESULT_VOTE_REJECTED_SELECTOR);
    expected.extend_from_slice(&U256::from(5_u64).to_be_bytes::<32>());
    assert_eq!(output.bytes, Bytes::from(expected));
}

#[test]
fn only_exact_non_static_value_free_metadosis_result_vote_is_entitled() {
    assert_eq!(
        ResultVoteCall::classify(
            METADOSIS_ADDRESS,
            &SUBMIT_LYSIS_RESULT_SELECTOR,
            false,
            U256::ZERO,
            true,
        ),
        ResultVoteCall::Entitled,
    );
    assert_eq!(
        ResultVoteCall::classify(
            Address::repeat_byte(1),
            &SUBMIT_LYSIS_RESULT_SELECTOR,
            false,
            U256::ZERO,
            true,
        ),
        ResultVoteCall::NotVote,
    );
    assert_eq!(
        ResultVoteCall::classify(
            METADOSIS_ADDRESS,
            &GET_OFFCHAIN_JOB_SELECTOR,
            false,
            U256::ZERO,
            true,
        ),
        ResultVoteCall::NotVote,
    );
    assert_eq!(
        ResultVoteCall::classify(
            METADOSIS_ADDRESS,
            &SUBMIT_LYSIS_RESULT_SELECTOR,
            true,
            U256::ZERO,
            true,
        ),
        ResultVoteCall::WrongMode,
    );
    assert_eq!(
        ResultVoteCall::classify(
            METADOSIS_ADDRESS,
            &SUBMIT_LYSIS_RESULT_SELECTOR,
            false,
            U256::from(1),
            true,
        ),
        ResultVoteCall::WrongMode,
    );
    assert_eq!(
        ResultVoteCall::classify(
            METADOSIS_ADDRESS,
            &SUBMIT_LYSIS_RESULT_SELECTOR,
            false,
            U256::ZERO,
            false,
        ),
        ResultVoteCall::NotVote,
    );
}

#[test]
fn exact_production_causes_receive_only_their_purpose() {
    let metadata = CertifiedParentAccountingMetadata::default();
    let certified = MetadosisCertifiedFinalityBinding::new(
        TEST_CHAIN_ID,
        TEST_BLOCK_NUMBER,
        metadata.finalized_block_number,
        metadata.finalized_block_hash,
        certified_root(),
    );
    assert_eq!(
        system_entitlements(
            SystemTxInputV2::CertifiedParentAccounting { metadata },
            false,
        ),
        MetadosisMutationEntitlements::exact(Purpose::CertifiedFinality, certified.binding(),),
    );
    assert_eq!(
        system_entitlements(SystemTxInputV2::CycleTick, false),
        expected_cycle_entitlements(),
    );
    assert_eq!(
        system_entitlements(
            SystemTxInputV2::LateFinalizeCredits {
                artifact: LateFinalizeCreditsArtifact::default(),
            },
            false,
        ),
        MetadosisMutationEntitlements::exact(
            Purpose::CertifiedFinality,
            metadosis_late_settlement_binding(TEST_CHAIN_ID, TEST_BLOCK_NUMBER, TEST_TIMESTAMP,),
        ),
    );
    assert_eq!(
        system_entitlements(SystemTxInputV2::OcompLifecycleBegin, true),
        MetadosisMutationEntitlements::exact(
            Purpose::OcompLifecycle,
            metadosis_ocomp_lifecycle_begin_binding(
                TEST_CHAIN_ID,
                TEST_BLOCK_NUMBER,
                TEST_TIMESTAMP,
            ),
        ),
    );
    assert_eq!(
        system_entitlements(SystemTxInputV2::OcompTerminalRequest, true),
        MetadosisMutationEntitlements::exact(
            Purpose::OcompLifecycle,
            metadosis_ocomp_terminal_request_binding(
                TEST_CHAIN_ID,
                TEST_BLOCK_NUMBER,
                TEST_TIMESTAMP,
            ),
        ),
    );
    assert_eq!(
        system_entitlements(
            SystemTxInputV2::BoundaryOutcome {
                artifact: boundary(),
            },
            false,
        ),
        MetadosisMutationEntitlements::NONE,
    );
    assert_eq!(
        metadosis_mutation_entitlements(MetadosisMutationCall {
            address: METADOSIS_ADDRESS,
            data: &SUBMIT_LYSIS_RESULT_SELECTOR,
            caller: Address::repeat_byte(0x99),
            is_static: false,
            value: U256::ZERO,
            ocomp_lifecycle_active: true,
            result_vote: ResultVoteCall::Entitled,
            chain_id: TEST_CHAIN_ID,
            block_number: TEST_BLOCK_NUMBER,
            timestamp: TEST_TIMESTAMP,
            cycle_active_utc_day: None,
            preloaded_certified_state_root: None,
            ocomp_fork_install: None,
        }),
        MetadosisMutationEntitlements::exact(
            Purpose::VerifiedResultVote,
            metadosis_verified_vote_binding(&SUBMIT_LYSIS_RESULT_SELECTOR),
        ),
    );
}

#[test]
fn protocol_cycle_grants_no_daily_allocation_after_a_multi_day_halt() {
    let block_day = outbe_primitives::time::timestamp_to_date_key(TEST_TIMESTAMP);
    let mut active_day = block_day;
    for _ in 0..6 {
        active_day = outbe_primitives::time::previous_date_key(active_day);
    }
    let data = encoded(SystemTxInputV2::CycleTick);
    let entitlements = metadosis_mutation_entitlements(MetadosisMutationCall {
        address: OUTBE_SYSTEM_TX_ADDRESS,
        data: data.as_ref(),
        caller: SYSTEM_ADDRESS,
        is_static: false,
        value: U256::ZERO,
        ocomp_lifecycle_active: false,
        result_vote: ResultVoteCall::NotVote,
        chain_id: TEST_CHAIN_ID,
        block_number: TEST_BLOCK_NUMBER,
        timestamp: TEST_TIMESTAMP,
        cycle_active_utc_day: Some(active_day),
        preloaded_certified_state_root: None,
        ocomp_fork_install: None,
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
        day = outbe_primitives::time::next_date_key(day);
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
                ocomp_lifecycle_active: false,
                result_vote: ResultVoteCall::NotVote,
                chain_id: TEST_CHAIN_ID,
                block_number: TEST_BLOCK_NUMBER,
                timestamp: TEST_TIMESTAMP,
                cycle_active_utc_day: None,
                preloaded_certified_state_root: None,
                ocomp_fork_install: None,
            }),
            MetadosisMutationEntitlements::NONE,
        );
    }

    assert_eq!(
        metadosis_mutation_entitlements(MetadosisMutationCall {
            address: OUTBE_SYSTEM_TX_ADDRESS,
            data: b"malformed",
            caller: SYSTEM_ADDRESS,
            is_static: false,
            value: U256::ZERO,
            ocomp_lifecycle_active: false,
            result_vote: ResultVoteCall::NotVote,
            chain_id: TEST_CHAIN_ID,
            block_number: TEST_BLOCK_NUMBER,
            timestamp: TEST_TIMESTAMP,
            cycle_active_utc_day: None,
            preloaded_certified_state_root: None,
            ocomp_fork_install: None,
        }),
        MetadosisMutationEntitlements::NONE,
    );
    assert_eq!(
        system_entitlements(SystemTxInputV2::OracleSlashWindow, false),
        MetadosisMutationEntitlements::NONE,
    );
    assert_eq!(
        system_entitlements(SystemTxInputV2::OcompLifecycleBegin, false),
        MetadosisMutationEntitlements::NONE,
    );
    assert_eq!(
        system_entitlements(SystemTxInputV2::OcompTerminalRequest, false),
        MetadosisMutationEntitlements::NONE,
    );
    assert_eq!(
        metadosis_mutation_entitlements(MetadosisMutationCall {
            address: METADOSIS_ADDRESS,
            data: &SUBMIT_LYSIS_RESULT_SELECTOR,
            caller: Address::repeat_byte(3),
            is_static: false,
            value: U256::ZERO,
            ocomp_lifecycle_active: false,
            result_vote: ResultVoteCall::NotVote,
            chain_id: TEST_CHAIN_ID,
            block_number: TEST_BLOCK_NUMBER,
            timestamp: TEST_TIMESTAMP,
            cycle_active_utc_day: None,
            preloaded_certified_state_root: None,
            ocomp_fork_install: None,
        }),
        MetadosisMutationEntitlements::NONE,
    );
    assert_eq!(
        metadosis_mutation_entitlements(MetadosisMutationCall {
            address: METADOSIS_ADDRESS,
            data: &GET_OFFCHAIN_JOB_SELECTOR,
            caller: Address::repeat_byte(3),
            is_static: false,
            value: U256::ZERO,
            ocomp_lifecycle_active: true,
            result_vote: ResultVoteCall::NotVote,
            chain_id: TEST_CHAIN_ID,
            block_number: TEST_BLOCK_NUMBER,
            timestamp: TEST_TIMESTAMP,
            cycle_active_utc_day: None,
            preloaded_certified_state_root: None,
            ocomp_fork_install: None,
        }),
        MetadosisMutationEntitlements::NONE,
    );
}
