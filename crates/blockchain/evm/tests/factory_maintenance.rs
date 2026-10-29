//! Exercise lazy factory maintenance through real transaction and STATICCALL frames.

use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_evm::OutbeEvmFactory;
use outbe_nodfactory::precompile::INodFactory;
use outbe_primitives::{
    addresses::{
        CREDIS_FACTORY_ADDRESS, GEM_FACTORY_ADDRESS, INTEX_FACTORY_ADDRESS, NOD_FACTORY_ADDRESS,
    },
    block::{BlockContext, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::date_key_to_utc_timestamp,
};
use reth_ethereum::evm::primitives::EvmEnv;
use revm::{
    context::{
        result::{ExecutionResult, Output, ResultAndState},
        BlockEnv, CfgEnv, TxEnv,
    },
    database::{CacheDB, EmptyDB},
    primitives::{hardfork::SpecId, TxKind},
    state::{AccountInfo, Bytecode},
    DatabaseCommit,
};

const FACTORIES: [Address; 4] = [
    NOD_FACTORY_ADDRESS,
    GEM_FACTORY_ADDRESS,
    CREDIS_FACTORY_ADDRESS,
    INTEX_FACTORY_ADDRESS,
];
const CALLER: Address = Address::repeat_byte(0xc1);
const OTHER: Address = Address::repeat_byte(0xc2);
const PROXY: Address = Address::repeat_byte(0xa1);

fn database(initialized: bool) -> CacheDB<EmptyDB> {
    let mut seed = HashMapStorageProvider::new(1);
    if initialized {
        StorageHandle::enter(&mut seed, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(0, date_key_to_utc_timestamp(20260101), 1),
                storage.clone(),
            );
            outbe_nodfactory::called::initialize_daily(&ctx).unwrap();
            outbe_gemfactory::called::initialize_daily(&ctx).unwrap();
            outbe_credisfactory::called::initialize_daily(&ctx).unwrap();
            outbe_intexfactory::called::initialize_daily(&ctx).unwrap();
            outbe_oracle::schema::OracleContract::new(storage)
                .utc_day_vwap_last_finalized
                .write(20260103)
                .unwrap();
        });
    }
    let mut db = CacheDB::new(EmptyDB::default());
    for caller in [CALLER, OTHER] {
        db.insert_account_info(
            caller,
            AccountInfo {
                balance: U256::from(1_000_000_000),
                ..Default::default()
            },
        );
    }
    for ((address, slot), value) in seed.storage {
        db.insert_account_storage(address, slot, value).unwrap();
    }
    db
}

fn maintain(call: bool, forfeit: bool, cleanup: bool) -> Bytes {
    INodFactory::maintainCall {
        process_call: call,
        process_forfeit: forfeit,
        process_cleanup: cleanup,
    }
    .abi_encode()
    .into()
}

fn query() -> Bytes {
    INodFactory::hasMaintenanceWorkCall {}.abi_encode().into()
}

fn execute(
    db: &mut CacheDB<EmptyDB>,
    target: Address,
    caller: Address,
    data: Bytes,
    value: u64,
    height: u64,
) -> ResultAndState {
    let nonce = db
        .cache
        .accounts
        .get(&caller)
        .map_or(0, |account| account.info.nonce);
    let env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(1)
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
        block_env: BlockEnv {
            number: U256::from(height),
            timestamp: U256::from(date_key_to_utc_timestamp(20260104)),
            gas_limit: 30_000_000,
            ..Default::default()
        },
    };
    let mut evm = OutbeEvmFactory::default().create_evm(db, env);
    evm.transact_raw(
        TxEnv::builder()
            .caller(caller)
            .nonce(nonce)
            .kind(TxKind::Call(target))
            .data(data)
            .value(U256::from(value))
            .gas_limit(3_000_000)
            .build()
            .unwrap(),
    )
    .unwrap()
}

fn writes(out: &ResultAndState) -> usize {
    out.state
        .values()
        .map(|account| {
            account
                .storage
                .values()
                .filter(|slot| slot.present_value != slot.original_value)
                .count()
        })
        .sum()
}

fn output(out: &ResultAndState) -> &[u8] {
    match &out.result {
        ExecutionResult::Success {
            output: Output::Call(bytes),
            ..
        } => bytes,
        other => panic!("expected success, got {other:?}"),
    }
}

fn install_proxy(db: &mut CacheDB<EmptyDB>, target: Address, opcode: u8) {
    // Return the child success word followed by its returndata, including empty halts.
    let mut code = vec![
        0x36, 0x60, 0, 0x60, 0, 0x37, 0x60, 0, 0x60, 0, 0x36, 0x60, 0,
    ];
    if opcode == 0xf2 {
        code.push(0x34);
    }
    code.push(0x73);
    code.extend_from_slice(target.as_slice());
    code.extend_from_slice(&[
        0x5a, opcode, 0x60, 0, 0x52, 0x3d, 0x60, 0, 0x60, 32, 0x3e, 0x3d, 0x60, 32, 0x01, 0x60, 0,
        0xf3,
    ]);
    let code = Bytecode::new_raw(code.into());
    db.insert_account_info(
        PROXY,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
}

#[test]
fn all_false_is_a_gas_paying_noop_even_without_bootstrap() {
    for factory in FACTORIES {
        let out = execute(
            &mut database(false),
            factory,
            CALLER,
            maintain(false, false, false),
            0,
            42,
        );
        assert!(output(&out).is_empty());
        assert_eq!(writes(&out), 0);
        assert!(out.result.logs().is_empty());
        assert!(out.result.tx_gas_used() > 21_000);
    }
}

#[test]
fn static_queries_report_obligations_without_writes_or_logs() {
    for factory in FACTORIES {
        let mut db = database(true);
        install_proxy(&mut db, factory, 0xfa);
        let out = execute(&mut db, PROXY, CALLER, query(), 0, 42);
        assert_eq!(U256::from_be_slice(&output(&out)[..32]), U256::ONE);
        let flags =
            INodFactory::hasMaintenanceWorkCall::abi_decode_returns(&output(&out)[32..]).unwrap();
        assert!(flags.call_pending);
        assert!(flags.forfeit_pending);
        assert_eq!(flags.cleanup_pending, factory == CREDIS_FACTORY_ADDRESS);
        assert_eq!(writes(&out), 0);
        assert!(out.result.logs().is_empty());
    }
}

#[test]
fn maintenance_rejects_value_static_mutation_and_borrowed_frames() {
    for factory in FACTORIES {
        let out = execute(
            &mut database(true),
            factory,
            CALLER,
            maintain(true, true, true),
            1,
            42,
        );
        assert!(!out.result.is_success());
        assert_eq!(writes(&out), 0);
        for opcode in [0xfa, 0xf2, 0xf4] {
            let mut db = database(true);
            install_proxy(&mut db, factory, opcode);
            let out = execute(&mut db, PROXY, CALLER, maintain(false, false, false), 0, 42);
            assert_eq!(U256::from_be_slice(&output(&out)[..32]), U256::ZERO);
            assert_eq!(writes(&out), 0);
            assert!(out.result.logs().is_empty());
        }
    }
}

#[test]
fn different_callers_and_flags_cannot_execute_a_second_day_at_one_height() {
    for factory in FACTORIES {
        let mut db = database(true);
        let out = execute(
            &mut db,
            factory,
            CALLER,
            maintain(true, false, false),
            0,
            42,
        );
        assert!(output(&out).is_empty());
        assert!(writes(&out) > 0);
        db.commit(out.state);
        let out = execute(&mut db, factory, OTHER, maintain(false, true, true), 0, 42);
        assert!(output(&out).is_empty());
        db.commit(out.state);
        let out = execute(&mut db, factory, CALLER, maintain(true, true, false), 0, 42);
        assert!(output(&out).is_empty());
        assert_eq!(
            writes(&out),
            0,
            "a different flag set must not select tomorrow's pass"
        );
        db.commit(out.state);
        let out = execute(&mut db, factory, OTHER, maintain(true, true, false), 0, 43);
        assert!(output(&out).is_empty());
        assert!(
            writes(&out) > 0,
            "the next height may resume the queued day"
        );
    }
}

#[test]
fn failed_maintenance_rolls_back_scheduling_and_allowance() {
    let mut db = database(true);
    let mut seed = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut seed, |storage| {
        outbe_nod::schema::NodContract::new(storage)
            .config_profile
            .write(u8::MAX)
            .unwrap();
    });
    for ((address, slot), value) in seed.storage {
        db.insert_account_storage(address, slot, value).unwrap();
    }
    let out = execute(
        &mut db,
        NOD_FACTORY_ADDRESS,
        CALLER,
        maintain(true, true, true),
        0,
        42,
    );
    assert!(!out.result.is_success());
    assert_eq!(writes(&out), 0);
    assert!(out.result.logs().is_empty());
}
