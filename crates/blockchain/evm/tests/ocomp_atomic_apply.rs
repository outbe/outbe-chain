use std::sync::Arc;

use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{Address, Bytes, TxKind, U256};
use outbe_evm::factory::OutbeEvmFactory;
use outbe_metadosis::ocomp::test_support::{
    ActivationFixture, ActivationReceiptFault, TEST_LOGICAL_TIME,
};
use outbe_ocomp_protocol::abi::{
    ACTIVATE_LYSIS_SELECTOR, METADOSIS_ADDRESS, OCOMP_ACTIVATION_REJECTED_SELECTOR,
};
use outbe_ocomp_protocol::receipts::ActivationOutcome;
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
use outbe_primitives::{
    addresses::{INTEX_ADDRESS, NOD_ADDRESS, PROMIS_LIMIT_ADDRESS, TRIBUTE_ADDRESS},
    error::PrecompileError,
    system_tx::OcompLifecycleActivation,
};
use reth_ethereum::evm::primitives::EvmEnv;
use reth_ethereum::evm::revm::context_interface::result::ExecutionResult;
use revm::{
    context::{BlockEnv, CfgEnv, TxEnv},
    database::{CacheDB, EmptyDB},
    primitives::hardfork::SpecId,
    state::AccountInfo,
};

const CALLER: Address = Address::new([0x44; 20]);

fn rejection_code(result: ExecutionResult) -> u16 {
    let ExecutionResult::Revert { output, .. } = result else {
        panic!("OCOMP admission must return a typed transaction revert");
    };
    assert_eq!(&output[..4], &OCOMP_ACTIVATION_REJECTED_SELECTOR);
    u16::try_from(U256::from_be_slice(&output[4..36])).unwrap()
}

fn malformed_activation_tx() -> TxEnv {
    activation_tx_with_gas(31_000_000)
}

fn activation_tx_with_gas(gas_limit: u64) -> TxEnv {
    TxEnv::builder()
        .caller(CALLER)
        .kind(TxKind::Call(METADOSIS_ADDRESS))
        .data(Bytes::copy_from_slice(&ACTIVATE_LYSIS_SELECTOR))
        .gas_limit(gas_limit)
        .build_fill()
}

fn activation_tx(data: Bytes) -> TxEnv {
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
            gas_limit: 40_000_000,
            ..Default::default()
        },
    }
}

// OCOMP-TEST-ID: OCM-APL-002
#[test]
fn public_evm_dispatch_allows_one_activation_attempt_per_block() {
    let storage: StorageReaderHandle = Arc::new(MemoryStorage::new());
    let readers = RuntimeBodyReaders::new(storage);
    let factory = OutbeEvmFactory::with_runtime_body_readers(readers);
    factory.install_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(0));
    let mut evm = factory.create_evm(funded_db(), env());

    let first = evm
        .transact_raw(malformed_activation_tx())
        .expect("first activation attempt executes");
    assert_eq!(rejection_code(first.result), 1);

    let second = evm
        .transact_raw(malformed_activation_tx())
        .expect("second activation attempt executes as a revert");
    assert_eq!(rejection_code(second.result), 15);
}

#[test]
fn four_owner_failures_restore_the_complete_activation_checkpoint() {
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
            .expect_err("named owner fault must abort activation");
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
        assert_eq!(
            fixture.rollback_snapshot(),
            before,
            "{owner} failure leaked owner, event, job, or CE state"
        );
        fixture.assert_pending();
    }
}

#[test]
fn four_owner_receipts_and_request_split_mismatch_roll_back_everything() {
    let faults = [
        ("Nod receipt", ActivationReceiptFault::Nod),
        ("Contributor receipt", ActivationReceiptFault::Contributor),
        ("Tribute receipt", ActivationReceiptFault::Tribute),
        ("CarryOver receipt", ActivationReceiptFault::CarryOver),
        (
            "request split receipt",
            ActivationReceiptFault::RequestSplit,
        ),
    ];

    for (receipt, fault) in faults {
        let mut fixture = ActivationFixture::new(20, 1_010, true);
        let before = fixture.rollback_snapshot();

        let error = fixture
            .apply_with_receipt_fault(fault)
            .expect_err("receipt mismatch must reject activation");
        assert_eq!(
            rejection_code_from_precompile(error),
            17,
            "{receipt} mutation must reach receipt verification"
        );
        assert_eq!(
            fixture.rollback_snapshot(),
            before,
            "{receipt} mutation leaked partial owner effects"
        );
        fixture.assert_pending();
    }
}

#[test]
fn apply_exact_retry_conflict_and_different_replay_have_frozen_results() {
    let mut applied = ActivationFixture::new(20, 1_010, true);
    let output = applied.apply().expect("valid q=3 activation must apply");
    assert_eq!(
        ActivationFixture::decoded_outcome(&output),
        ActivationOutcome::Applied
    );
    let completed = applied.rollback_snapshot();
    let event_count = applied.provider.get_ordered_events().len();
    let retry = applied
        .dispatch_current()
        .expect("exact completed retry must be read-only");
    assert_eq!(retry, output);
    assert_eq!(applied.finality.calls(), 1);
    assert_eq!(applied.provider.get_ordered_events().len(), event_count);
    assert_eq!(applied.rollback_snapshot(), completed);

    applied.activation.certificate.ordered_signatures[0].signature_rs[0] ^= 1;
    let error = applied
        .dispatch_current()
        .expect_err("different completed replay must reject");
    assert_eq!(rejection_code_from_precompile(error), 6);
    assert_eq!(applied.rollback_snapshot(), completed);

    let mut conflict = ActivationFixture::new(20, 1_010, false);
    let output = conflict
        .dispatch_current()
        .expect("valid evidence with changed owner preconditions must conflict");
    assert_eq!(
        ActivationFixture::decoded_outcome(&output),
        ActivationOutcome::ConflictResolved
    );
    assert_eq!(conflict.finality.calls(), 1);
}

#[test]
fn completed_exact_retry_uses_the_real_public_evm_selector() {
    let mut fixture = ActivationFixture::new(20, TEST_LOGICAL_TIME + 10, true);
    let expected = fixture.apply().expect("fixture activation must complete");
    let calldata = fixture.calldata();

    let mut db = funded_db();
    for ((address, key), value) in &fixture.provider.storage {
        db.insert_account_storage(*address, *key, *value).unwrap();
    }
    let factory = OutbeEvmFactory::new();
    factory.install_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(0));
    let mut evm = factory.create_evm(db, env());
    let result = evm
        .transact_raw(activation_tx(calldata))
        .expect("public exact retry executes");
    let ExecutionResult::Success { output, .. } = result.result else {
        panic!("public exact retry must succeed");
    };
    assert_eq!(output.data(), expected.as_ref());
}

#[test]
fn activation_selector_is_fork_gated_and_independent_of_body_readers() {
    let disabled = OutbeEvmFactory::new();
    let mut prefork = disabled.create_evm(funded_db(), env());
    let result = prefork
        .transact_raw(malformed_activation_tx())
        .expect("pre-fork legacy dispatch executes")
        .result;
    match result {
        ExecutionResult::Revert { output, .. } => {
            assert_ne!(&output[..4], &OCOMP_ACTIVATION_REJECTED_SELECTOR);
        }
        other => panic!("legacy Metadosis selector handling must revert, got {other:?}"),
    }

    let active_without_readers = OutbeEvmFactory::new();
    active_without_readers
        .install_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(0));
    let mut active = active_without_readers.create_evm(funded_db(), env());
    let result = active
        .transact_raw(malformed_activation_tx())
        .expect("active activation dispatch executes without Mongo readers");
    assert_eq!(rejection_code(result.result), 1);
}

#[test]
fn active_activation_pays_fixed_bounded_work_charge_before_decode() {
    let factory = OutbeEvmFactory::new();
    factory.install_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(0));
    let mut evm = factory.create_evm(funded_db(), env());

    let result = evm
        .transact_raw(activation_tx_with_gas(29_999_999))
        .expect("underfunded activation returns an EVM halt");
    assert!(matches!(result.result, ExecutionResult::Halt { .. }));

    let result = evm
        .transact_raw(malformed_activation_tx())
        .expect("out-of-gas preflight must not consume the block attempt");
    assert_eq!(rejection_code(result.result), 1);
}

fn rejection_code_from_precompile(error: PrecompileError) -> u16 {
    let PrecompileError::RevertBytes(bytes) = error else {
        panic!("expected typed activation rejection");
    };
    assert_eq!(&bytes[..4], &OCOMP_ACTIVATION_REJECTED_SELECTOR);
    u16::try_from(U256::from_be_slice(&bytes[4..36])).unwrap()
}
