//! CCA custody and registration through real EVM transactions.
use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{SolCall, SolEvent};
use outbe_ccaregistry::{
    api,
    constants::{BOND_REQUIREMENT, UNBOND_COOLDOWN_SECONDS},
    precompile::ICcaRegistry,
};
use outbe_evm::OutbeEvmFactory;
use outbe_primitives::{
    addresses::CCA_REGISTRY_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    storage::{direct::DirectStorageProvider, StorageHandle},
    units::checked_protocol_to_native,
};
use reth_ethereum::evm::primitives::EvmEnv;
use revm::{
    context::{result::ExecutionResult, BlockEnv, CfgEnv, TxEnv},
    database::{CacheDB, EmptyDB},
    primitives::{hardfork::SpecId, TxKind},
    state::{AccountInfo, Bytecode},
    DatabaseCommit,
};

const CCA: Address = Address::repeat_byte(0xc1);
const NOW: u64 = 1_700_000_000;

fn tx(db: &mut CacheDB<EmptyDB>, value: U256, data: Vec<u8>, now: u64) -> ExecutionResult {
    let nonce = db.cache.accounts.get(&CCA).unwrap().info.nonce;
    let env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(1)
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
        block_env: BlockEnv {
            timestamp: U256::from(now),
            gas_limit: 30_000_000,
            ..Default::default()
        },
    };
    let outcome = {
        let mut evm = OutbeEvmFactory::new().create_evm(&mut *db, env);
        evm.transact_raw(
            TxEnv::builder()
                .caller(CCA)
                .nonce(nonce)
                .kind(TxKind::Call(CCA_REGISTRY_ADDRESS))
                .value(value)
                .data(Bytes::from(data))
                .gas_limit(3_000_000)
                .build()
                .unwrap(),
        )
        .unwrap()
    };
    db.commit(outcome.state);
    outcome.result
}

fn with_storage<R>(db: &mut CacheDB<EmptyDB>, f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    let mut provider = DirectStorageProvider::new(db, BlockContext::empty_for_tests(1, NOW, 1));
    let result = StorageHandle::enter(&mut provider, f);
    provider.flush().unwrap();
    result
}

#[test]
fn evm_bond_rewards_and_exit_preserve_custody_and_history() {
    let mut db = CacheDB::new(EmptyDB::default());
    let initial = BOND_REQUIREMENT * U256::from(2);
    db.insert_account_info(
        CCA,
        AccountInfo {
            balance: initial,
            ..Default::default()
        },
    );
    // Same marker as fresh genesis and the block executor, preserving empty-balance state.
    let code = Bytecode::new_raw(Bytes::from_static(&[0xef]));
    db.insert_account_info(
        CCA_REGISTRY_ADDRESS,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );

    assert!(!tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::getCcaCall { cca: CCA }.abi_encode(),
        NOW
    )
    .is_success());
    assert!(!tx(
        &mut db,
        U256::ONE,
        ICcaRegistry::bondCall {
            name: String::new()
        }
        .abi_encode(),
        NOW
    )
    .is_success());
    assert_eq!(db.cache.accounts[&CCA].info.balance, initial);
    assert!(tx(
        &mut db,
        BOND_REQUIREMENT - U256::ONE,
        ICcaRegistry::bondCall {
            name: "Test CCA".into()
        }
        .abi_encode(),
        NOW
    )
    .is_success());
    with_storage(&mut db, |s| {
        assert!(!api::is_active(&s, CCA).unwrap());
        let record = api::get_cca(&s, CCA).unwrap();
        assert_eq!(record.state, ICcaRegistry::State::Bonding);
        assert_eq!(record.name, "Test CCA");
        assert_eq!(record.bondedAmount, BOND_REQUIREMENT - U256::ONE);
    });
    let bonded = tx(
        &mut db,
        U256::ONE,
        ICcaRegistry::bondCall {
            name: "Test CCA".into(),
        }
        .abi_encode(),
        NOW,
    );
    assert!(bonded.is_success());
    assert_eq!(bonded.logs().len(), 1);
    let log = &bonded.logs()[0];
    assert_eq!(log.address, CCA_REGISTRY_ADDRESS);
    let event = ICcaRegistry::Bonded::decode_log(log).unwrap().data;
    assert_eq!(event.cca, CCA);
    assert_eq!(event.amount, U256::ONE);
    assert_eq!(event.state, ICcaRegistry::State::Active);
    assert_eq!(
        db.cache.accounts[&CCA].info.balance,
        initial - BOND_REQUIREMENT
    );
    assert_eq!(
        db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance,
        BOND_REQUIREMENT
    );
    // Malformed and nonpayable funded calls refund value and preserve registration.
    for data in [
        vec![],
        vec![0, 1, 2, 3],
        ICcaRegistry::unbondCall {}.abi_encode(),
    ] {
        assert!(!tx(&mut db, U256::ONE, data, NOW).is_success());
        assert_eq!(
            db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance,
            BOND_REQUIREMENT
        );
    }
    with_storage(&mut db, |s| {
        api::position_opened(&s, CCA, 20231115, U256::from(100)).unwrap();
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, NOW, 1), s.clone());
        assert_eq!(
            outbe_ccaregistry::emission_sink::distribute_daily(&ctx, 20231115, U256::from(23))
                .unwrap(),
            U256::ZERO
        );
    });
    let reward = checked_protocol_to_native(U256::from(23)).unwrap();
    assert!(tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::unbondCall {}.abi_encode(),
        NOW
    )
    .is_success());
    assert!(!tx(
        &mut db,
        U256::ONE,
        ICcaRegistry::bondCall {
            name: "Test CCA".into()
        }
        .abi_encode(),
        NOW
    )
    .is_success());
    with_storage(&mut db, |s| {
        let record = api::get_cca(&s, CCA).unwrap();
        assert_eq!(record.state as u8, ICcaRegistry::State::Deregistering as u8);
        assert_eq!(record.bondedAmount, BOND_REQUIREMENT);
        assert_eq!(record.unbondUnlocksAfter, NOW + UNBOND_COOLDOWN_SECONDS);
        assert!(!api::is_active(&s, CCA).unwrap());
        assert!(api::position_opened(&s, CCA, 20231115, U256::ONE).is_err());
    });
    assert!(tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::claimRewardsCall {}.abi_encode(),
        NOW
    )
    .is_success());
    assert_eq!(
        db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance,
        BOND_REQUIREMENT
    );
    assert!(!tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::claimUnbondedCall {}.abi_encode(),
        NOW + UNBOND_COOLDOWN_SECONDS - 1
    )
    .is_success());
    assert!(tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::claimUnbondedCall {}.abi_encode(),
        NOW + UNBOND_COOLDOWN_SECONDS
    )
    .is_success());
    assert_eq!(
        db.cache.accounts[&CCA_REGISTRY_ADDRESS].info.balance,
        U256::ZERO
    );
    assert_eq!(db.cache.accounts[&CCA].info.balance, initial + reward);
    with_storage(&mut db, |s| {
        let record = api::get_cca(&s, CCA).unwrap();
        assert_eq!(record.state as u8, ICcaRegistry::State::Deregistered as u8);
        assert_eq!(record.bondedAmount, U256::ZERO);
        assert_eq!(record.name, "Test CCA");
        assert_eq!(
            api::reward_weight(&s, CCA, 20231115).unwrap(),
            U256::from(100)
        );
    });
    assert!(!tx(
        &mut db,
        U256::ZERO,
        ICcaRegistry::claimUnbondedCall {}.abi_encode(),
        NOW + UNBOND_COOLDOWN_SECONDS
    )
    .is_success());
}
