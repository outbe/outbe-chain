//! The borrowed child frame must expose a settled, typed VM outcome.
use alloy_consensus::TxLegacy;
use alloy_primitives::{Address, Bytes, TxKind, U256};
use alloy_sol_types::SolCall;
use evm2::{
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::TxEnvelope,
    evm::{AccountInfo, CacheDB, Database as _, EmptyDB},
    interpreter::InstrStop,
    SpecId, TxResult,
};
use outbe_evm::{factory::OutbeEvmTypes, sub_call, test_support::EvmWorld, OutbeEvmFactory};
use outbe_primitives::storage::{SubCallError, SubCallInput, SubCallStatus};
use outbe_system_common::addresses::VAULT_ROUTER_ADDRESS;
use outbe_vaultrouter::precompile::IVaultRouter;
use reth_ethereum::evm::EthEvmEnv;
use reth_primitives_traits::Recovered;

fn vault_read(code: &[u8]) -> TxResult<OutbeEvmTypes> {
    let sender = Address::repeat_byte(0xc0);
    let target = Address::repeat_byte(0xab);
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        &sender,
        AccountInfo {
            balance: U256::from(10_u64.pow(18)),
            ..Default::default()
        },
    );
    let code = Bytecode::new_raw(Bytes::copy_from_slice(code));
    db.insert_account_info(
        &target,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
    let mut evm = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
        .create_evm(
            db,
            EthEvmEnv::new(
                SpecId::PRAGUE,
                BlockEnvExt {
                    number: U256::from(1),
                    gas_limit: U256::from(30_000_000_u64),
                    ..Default::default()
                },
                1,
            ),
        );
    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce: 0,
            gas_price: 0,
            gas_limit: 200_000,
            to: TxKind::Call(VAULT_ROUTER_ADDRESS),
            value: U256::ZERO,
            input: IVaultRouter::sharesBalanceCall { vault: target }
                .abi_encode()
                .into(),
        }),
        sender,
    );
    let result = evm
        .transact(&tx)
        .expect("child VM failure must remain a transaction outcome")
        .commit();
    let next_tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(1),
            nonce: 1,
            gas_price: 0,
            gas_limit: 21_000,
            to: TxKind::Call(Address::repeat_byte(0xee)),
            value: U256::ZERO,
            input: Bytes::new(),
        }),
        sender,
    );
    let next = evm
        .transact(&next_tx)
        .expect("the next transaction still executes")
        .commit();
    assert!(next.is_success());
    result
}

#[test]
fn dispatcher_caps_unbounded_child_budget_to_parent_gas() {
    // GAS; MSTORE(0); RETURN(0,32): observe the actual child allowance.
    let result = vault_read(&[0x5a, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    assert!(result.is_success(), "{result:?}");
    let gas = U256::from_be_slice(&result.output);
    assert!(
        gas < U256::from(200_000),
        "child received {gas} gas from a 200000-gas transaction"
    );
}

#[test]
fn dispatcher_propagates_child_invalid_and_out_of_gas_as_execution_halts() {
    for code in [&[0xfe][..], &[0x5b, 0x60, 0, 0x56][..]] {
        let result = vault_read(code);
        assert!(result.is_halt(), "{result:?}");
        assert_eq!(
            result.tx_gas_used(),
            200_000,
            "evm2 spends the halted frame's gas"
        );
        assert!(
            result.output.is_empty(),
            "halts have no synthetic revert ABI"
        );
    }
}

#[test]
fn static_gem_factory_command_is_denied_before_domain_validation() {
    use outbe_gemfactory::precompile::IGemFactory;
    use outbe_primitives::{
        error::PrecompileError,
        storage::{hashmap::HashMapStorageProvider, StorageHandle},
    };
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_static(true);
    let storage = StorageHandle::new(&mut provider);

    let result = outbe_gemfactory::precompile::dispatch(
        storage,
        &IGemFactory::createGemFactoryPositionCall {
            intexId: U256::from(99),
            units: U256::from(1),
        }
        .abi_encode(),
        Address::repeat_byte(0xc0),
        U256::ZERO,
    );
    assert!(
        matches!(result, Err(PrecompileError::WriteProtection)),
        "static command must never enter its handler: {result:?}"
    );
}

#[test]
fn invalid_opcode_spends_the_child_allowance_and_retains_its_vm_reason() {
    let target = Address::repeat_byte(0xab);
    let mut db = CacheDB::new(EmptyDB::default());
    let code = Bytecode::new_raw(Bytes::from_static(&[0xfe]));
    db.insert_account_info(
        &target,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
    let host = std::sync::Arc::new(outbe_runtime::OutbeRuntime::new());
    let mut world = EvmWorld::new(db, host.clone());
    let result = sub_call::run_with_host(
        &mut world.evm,
        outbe_evm::test_support::fresh_environment(Address::repeat_byte(0xc0), SpecId::PRAGUE),
        SubCallInput {
            target,
            value: U256::ZERO,
            calldata: Bytes::new(),
            gas_limit: 10_000,
            is_static: false,
        },
        host,
    )
    .expect("an invalid opcode is an ordinary child VM outcome");

    assert_eq!(
        result.gas_used, 10_000,
        "exceptional halt spends all forwarded gas"
    );
    assert_eq!(result.gas_refunded, 0);
    assert!(matches!(
        result.status,
        SubCallStatus::Halt(SubCallError::EvmHalt(InstrStop::InvalidFEOpcode))
    ));
}

#[test]
fn gem_static_gate_and_nested_child_failures_settle_inside_evm() {
    use outbe_core_common::addresses::INTEX_NFT1155_ADDRESS;
    use outbe_gemfactory::precompile::IGemFactory;
    use outbe_intex::{CreateSeriesParams, SeriesId};
    use outbe_primitives::{
        block::BlockContext,
        storage::{direct::DirectStorageProvider, StorageHandle},
    };
    use outbe_system_common::addresses::GEM_FACTORY_ADDRESS;
    use outbe_system_common::time::EmissionDate;
    let sender = Address::repeat_byte(0xc0);
    let proxy = Address::repeat_byte(0xac);
    let series = SeriesId::pack(20_261_008, *b"USD", b'U').unwrap();
    let data = IGemFactory::createGemFactoryPositionCall {
        intexId: series.right_id(&[]),
        units: U256::ONE,
    }
    .abi_encode();
    for (is_static, nested) in [(false, false), (true, false), (false, true)] {
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            &sender,
            AccountInfo {
                balance: U256::from(10_u64.pow(18)),
                ..Default::default()
            },
        );
        // The source series is Issued and its reference currency is admitted:
        // a normal command reaches the real writing child and bubbles 0x42.
        let mut seed = DirectStorageProvider::new(
            &mut db,
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 1,
                timestamp: 1_700_000_000,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 1_700_000_000,
                proposer: sender,
                validators: vec![sender],
            },
        );
        StorageHandle::enter(&mut seed, |storage| {
            outbe_intex::api::create_series(
                &storage,
                CreateSeriesParams {
                    series_id: series,
                    emission_day: EmissionDate::new(20_261_008),
                    issued_units: 1,
                    promis_load_minor: 1_000_000,
                    entry_price_minor: U256::from(1_000_000),
                    call_price_minor: U256::ZERO,
                    call_trigger: Default::default(),
                    issued_at: 1_700_000_000,
                    issuance_currency: 840,
                    reference_currency: 840,
                },
            )
            .unwrap();
        });
        seed.flush().unwrap();
        drop(seed);
        let child = if nested {
            let invalid = Address::repeat_byte(0xde);
            let code = Bytecode::new_raw(Bytes::from_static(&[0xfe]));
            db.insert_account_info(
                &invalid,
                AccountInfo {
                    code_hash: code.hash_slow(),
                    code: Some(code),
                    ..Default::default()
                },
            );
            reverting_static_forwarder(
                VAULT_ROUTER_ADDRESS,
                &IVaultRouter::sharesBalanceCall { vault: invalid }.abi_encode(),
            )
        } else {
            Bytecode::new_raw(Bytes::from_static(&[
                0x60, 1, 0x60, 0, 0x55, 0x60, 0x42, 0x60, 0, 0x53, 0x60, 1, 0x60, 0, 0xfd,
            ]))
        };
        db.insert_account_info(
            &INTEX_NFT1155_ADDRESS,
            AccountInfo {
                code_hash: child.hash_slow(),
                code: Some(child),
                ..Default::default()
            },
        );
        // Forward calldata; return child success followed by exact returndata.
        let mut code = vec![
            0x36, 0x60, 0, 0x60, 0, 0x37, 0x60, 0, 0x60, 0, 0x36, 0x60, 0,
        ];
        if !is_static {
            code.extend_from_slice(&[0x60, 0]);
        }
        code.push(0x73);
        code.extend_from_slice(GEM_FACTORY_ADDRESS.as_slice());
        code.extend_from_slice(&[
            0x63,
            0,
            0x0f,
            0x42,
            0x40,
            if is_static { 0xfa } else { 0xf1 },
            0x60,
            0,
            0x52,
            0x3d,
            0x60,
            0,
            0x60,
            32,
            0x3e,
            0x3d,
            0x60,
            32,
            0x01,
            0x60,
            0,
            0xf3,
        ]);
        let code = Bytecode::new_raw(code.into());
        db.insert_account_info(
            &proxy,
            AccountInfo {
                code_hash: code.hash_slow(),
                code: Some(code),
                ..Default::default()
            },
        );
        let mut evm = outbe_evm::factory::OutbeEvmFactory::new(std::sync::Arc::new(
            outbe_runtime::OutbeRuntime::new(),
        ))
        .create_evm(
            &mut db,
            EthEvmEnv::new(
                SpecId::PRAGUE,
                BlockEnvExt {
                    number: U256::ONE,
                    timestamp: U256::from(1_700_000_000),
                    gas_limit: U256::from(30_000_000_u64),
                    ..Default::default()
                },
                1,
            ),
        );
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(1),
                nonce: 0,
                gas_price: 0,
                gas_limit: 500_000,
                to: TxKind::Call(proxy),
                value: U256::ZERO,
                input: Bytes::copy_from_slice(&data),
            }),
            sender,
        );
        let detached = evm
            .transact(&tx)
            .expect("static rejection never aborts execution")
            .detach();
        assert!(
            detached.result.is_success(),
            "proxy catches command failure"
        );
        let output = &detached.result.output;
        assert_eq!(&output[..32], &[0; 32]);
        if is_static {
            assert_eq!(output.len(), 32, "admission halt has no child returndata");
        } else if nested {
            assert_eq!(
                output.len(),
                32,
                "nested halt has no synthetic revert payload"
            );
        } else {
            assert_eq!(
                &output[32..],
                &[0x42],
                "normal CALL reaches the writing child"
            );
        }
        assert!(detached.result.logs.is_empty());
        drop(evm);
        db.commit_pending(&detached.pending_state);
        assert_eq!(
            db.get_storage(&INTEX_NFT1155_ADDRESS, &U256::ZERO).unwrap(),
            U256::ZERO,
            "child write rolled back"
        );
    }
}

fn reverting_static_forwarder(target: Address, calldata: &[u8]) -> Bytecode {
    let len = u8::try_from(calldata.len()).unwrap();
    // Write first, then call the second precompile and bubble its revert.
    let mut code = vec![
        0x60, 1, 0x60, 0, 0x55, 0x60, len, 0x60, 0, 0x60, 0, 0x39, 0x60, 0, 0x60, 0, 0x60, len,
        0x60, 0, 0x73,
    ];
    code.extend_from_slice(target.as_slice());
    code.extend_from_slice(&[
        0x63, 0, 0x0f, 0x42, 0x40, 0xfa, 0x50, 0x3d, 0x60, 0, 0x60, 0, 0x3e, 0x3d, 0x60, 0, 0xfd,
    ]);
    code[8] = u8::try_from(code.len()).unwrap();
    code.extend_from_slice(calldata);
    Bytecode::new_raw(code.into())
}
