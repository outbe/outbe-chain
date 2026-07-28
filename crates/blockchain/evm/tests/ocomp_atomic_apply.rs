// OCOMP-TEST-ID: OCM-APL-002
use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{Address, Bytes, TxKind, U256};
use outbe_evm::factory::OutbeEvmFactory;
use outbe_metadosis::ocomp::test_support::{ActivationFixture, ActivationReceiptFault};
use outbe_ocomp_protocol::{abi::METADOSIS_ADDRESS, receipts::ActivationOutcome};
use outbe_primitives::{
    addresses::{INTEX_ADDRESS, NOD_ADDRESS, PROMIS_LIMIT_ADDRESS, TRIBUTE_ADDRESS},
    error::PrecompileError,
    system_tx::OcompLifecycleActivation,
};
use reth_ethereum::evm::{primitives::EvmEnv, revm::context_interface::result::ExecutionResult};
use revm::{
    context::{BlockEnv, CfgEnv, TxEnv},
    database::{CacheDB, EmptyDB},
    primitives::hardfork::SpecId,
    state::AccountInfo,
};

const CALLER: Address = Address::new([0x44; 20]);

fn result_vote_tx(data: Bytes) -> TxEnv {
    TxEnv::builder()
        .caller(CALLER)
        .kind(TxKind::Call(METADOSIS_ADDRESS))
        .data(data)
        .gas_limit(31_000_000)
        .build_fill()
}

fn funded_db() -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        CALLER,
        AccountInfo {
            balance: U256::MAX,
            ..Default::default()
        },
    );
    db
}

fn env() -> EvmEnv {
    EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(1)
            .with_spec_and_mainnet_gas_params(SpecId::SHANGHAI),
        block_env: BlockEnv {
            number: U256::from(20),
            timestamp: U256::from(1_010),
            gas_limit: 40_000_000,
            ..Default::default()
        },
    }
}

// Stable historical ID retained after moving this checkpoint invariant out
// of the four-node E2E lane.
// OCOMP-TEST-ID: OCM-E2E-006
#[test]
fn q_forming_owner_failures_restore_slot_quorum_and_every_owner_effect() {
    for (owner, address) in [
        ("Nod", NOD_ADDRESS),
        ("Contributor", INTEX_ADDRESS),
        ("Tribute", TRIBUTE_ADDRESS),
        ("CarryOver", PROMIS_LIMIT_ADDRESS),
    ] {
        let mut fixture = ActivationFixture::new(20, 1_010, true);
        let before = fixture.rollback_snapshot();
        fixture.provider.fail_mutation_at_address(address);

        let error = fixture
            .apply()
            .expect_err("q-forming owner fault must abort the whole vote");
        assert!(
            matches!(
                error,
                PrecompileError::Storage(_)
                    | PrecompileError::Fatal(_)
                    | PrecompileError::Revert(_)
                    | PrecompileError::RevertBytes(_)
            ),
            "{owner} failure returned the wrong error class"
        );
        fixture.provider.clear_mutation_failure();
        assert_eq!(fixture.rollback_snapshot(), before);
        fixture.assert_pending();
        assert_eq!(
            fixture.apply().expect("exact retry after owner fault"),
            Bytes::new()
        );
        assert_eq!(fixture.terminal_outcome(), ActivationOutcome::Applied);
    }
}

#[test]
fn q_forming_receipt_mismatch_restores_slot_quorum_and_every_owner_effect() {
    for fault in [
        ActivationReceiptFault::Nod,
        ActivationReceiptFault::Contributor,
        ActivationReceiptFault::Tribute,
        ActivationReceiptFault::CarryOver,
        ActivationReceiptFault::RequestSplit,
    ] {
        let mut fixture = ActivationFixture::new(20, 1_010, true);
        let before = fixture.rollback_snapshot();

        assert!(matches!(
            fixture.apply_with_receipt_fault(fault),
            Err(PrecompileError::RevertBytes(_))
        ));
        assert_eq!(fixture.rollback_snapshot(), before);
        fixture.assert_pending();
    }
}

#[test]
fn third_matching_full_result_applies_or_conflicts_without_a_second_transaction() {
    let mut applied = ActivationFixture::new(20, 1_010, true);
    assert_eq!(applied.apply().unwrap(), Bytes::new());
    assert_eq!(applied.terminal_outcome(), ActivationOutcome::Applied);
    let completed = applied.rollback_snapshot();
    let event_count = applied.provider.get_ordered_events().len();

    assert_eq!(applied.dispatch_current().unwrap(), Bytes::new());
    assert_eq!(applied.provider.get_ordered_events().len(), event_count);
    assert_eq!(applied.rollback_snapshot(), completed);

    let mut conflict = ActivationFixture::new(20, 1_010, false);
    assert_eq!(conflict.dispatch_current().unwrap(), Bytes::new());
    assert_eq!(
        conflict.terminal_outcome(),
        ActivationOutcome::ConflictResolved
    );
}

#[test]
fn completed_vote_retry_uses_the_real_public_result_selector() {
    let mut fixture = ActivationFixture::new(20, 1_010, true);
    fixture.apply().unwrap();
    let calldata = fixture.calldata();

    let mut db = funded_db();
    for ((address, key), value) in &fixture.provider.storage {
        db.insert_account_storage(*address, *key, *value).unwrap();
    }
    let factory = OutbeEvmFactory::new();
    factory.install_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(0));
    let mut evm = factory.create_evm(db, env());
    let result = evm
        .transact_raw(result_vote_tx(calldata))
        .expect("public exact retry executes");
    let ExecutionResult::Success { output, .. } = result.result else {
        panic!("public exact retry must succeed, got {:?}", result.result);
    };
    assert!(output.data().is_empty());
}
