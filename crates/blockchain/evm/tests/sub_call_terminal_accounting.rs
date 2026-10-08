//! The borrowed child frame must expose a settled, typed VM outcome.
mod sub_call_support;

use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_evm::sub_call;
use outbe_evm::OutbeEvmFactory;
use outbe_primitives::addresses::VAULT_ROUTER_ADDRESS;
use outbe_primitives::storage::{SubCallError, SubCallInput, SubCallStatus};
use outbe_vaultrouter::api::IVaultRouter;
use reth_ethereum::evm::primitives::EvmEnv;
use revm::{
    context::result::HaltReason,
    database::{CacheDB, EmptyDB},
    handler::MainContext as _,
    primitives::hardfork::SpecId,
    state::{AccountInfo, Bytecode},
    Context, DatabaseCommit,
};

fn vault_read(code: &[u8]) -> revm::context::result::ExecutionResult {
    use revm::context::{BlockEnv, CfgEnv, TxEnv};
    let sender = Address::repeat_byte(0xc0);
    let target = Address::repeat_byte(0xab);
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        sender,
        AccountInfo {
            balance: U256::from(10_u64.pow(18)),
            ..Default::default()
        },
    );
    let code = Bytecode::new_raw(Bytes::copy_from_slice(code));
    db.insert_account_info(
        target,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
    let mut evm = OutbeEvmFactory::default().create_evm(
        db,
        EvmEnv {
            cfg_env: CfgEnv::new()
                .with_chain_id(1)
                .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
            block_env: BlockEnv {
                number: U256::from(1),
                gas_limit: 30_000_000,
                ..Default::default()
            },
        },
    );
    let outcome = evm
        .transact_raw(
            TxEnv::builder()
                .caller(sender)
                .nonce(0)
                .kind(revm::primitives::TxKind::Call(VAULT_ROUTER_ADDRESS))
                .data(
                    IVaultRouter::sharesBalanceCall { vault: target }
                        .abi_encode()
                        .into(),
                )
                .gas_limit(200_000)
                .build()
                .unwrap(),
        )
        .expect("child VM failure must remain a transaction outcome");
    evm.db_mut().commit(outcome.state);
    let next = evm
        .transact_raw(
            TxEnv::builder()
                .caller(sender)
                .nonce(1)
                .kind(revm::primitives::TxKind::Call(Address::repeat_byte(0xee)))
                .gas_limit(21_000)
                .build()
                .unwrap(),
        )
        .expect("the next transaction still executes");
    assert!(next.result.is_success());
    outcome.result
}

#[test]
fn dispatcher_caps_unbounded_child_budget_to_parent_gas() {
    // GAS; MSTORE(0); RETURN(0,32): observe the actual child allowance.
    let result = vault_read(&[0x5a, 0x60, 0, 0x52, 0x60, 32, 0x60, 0, 0xf3]);
    let bytes = result
        .output()
        .expect("successful getter returns the child's GAS");
    assert!(result.is_success(), "{result:?}");
    let gas = U256::from_be_slice(bytes);
    assert!(
        gas < U256::from(200_000),
        "child received {gas} gas from a 200000-gas transaction"
    );
}

#[test]
fn dispatcher_returns_an_ordinary_revert_for_child_invalid_or_out_of_gas() {
    for code in [&[0xfe][..], &[0x5b, 0x60, 0, 0x56][..]] {
        let result = vault_read(code);
        assert!(
            matches!(
                result,
                revm::context::result::ExecutionResult::Revert { .. }
            ),
            "{result:?}"
        );
        assert!(
            result.tx_gas_used() > 190_000,
            "executed exceptional halt must spend its allowance"
        );
        assert!(
            result.tx_gas_used() < 200_000,
            "the wrapper retains the parent's EIP-150 remainder"
        );
        assert_eq!(
            result.output().unwrap().len(),
            36,
            "stable typed child-halt ABI"
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
        &IGemFactory::issueGemPositionCall {
            sourceIntexId: alloy_primitives::FixedBytes::repeat_byte(99),
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
        target,
        AccountInfo {
            code_hash: code.hash_slow(),
            code: Some(code),
            ..Default::default()
        },
    );
    let mut ctx = Context::mainnet().with_db(db);
    let result = sub_call::run(
        &mut ctx,
        sub_call_support::fresh_environment(Address::repeat_byte(0xc0), SpecId::PRAGUE),
        SubCallInput {
            target,
            value: U256::ZERO,
            calldata: Bytes::new(),
            gas_limit: 10_000,
            is_static: false,
        },
    )
    .expect("an invalid opcode is an ordinary child VM outcome");

    assert_eq!(
        result.gas_used, 10_000,
        "exceptional halt spends all forwarded gas"
    );
    assert_eq!(result.gas_refunded, 0);
    assert!(matches!(
        result.status,
        SubCallStatus::Halt(SubCallError::EvmHalt(HaltReason::InvalidFEOpcode))
    ));
}

#[test]
fn gem_static_gate_and_nested_child_failures_settle_inside_evm() {
    use outbe_gemfactory::precompile::IGemFactory;
    use outbe_intex::{CreateSeriesParams, SeriesId};
    use outbe_primitives::{
        addresses::{GEM_FACTORY_ADDRESS, INTEX_NFT1155_ADDRESS},
        block::BlockContext,
        storage::{direct::DirectStorageProvider, StorageHandle},
        time::WorldwideDay,
    };
    use revm::context::{BlockEnv, CfgEnv, TxEnv};
    let sender = Address::repeat_byte(0xc0);
    let proxy = Address::repeat_byte(0xac);
    let series = SeriesId::pack(WorldwideDay::new(20_261_008), *b"USD", b'U').unwrap();
    let data = IGemFactory::issueGemPositionCall {
        sourceIntexId: series.into(),
        units: U256::ONE,
    }
    .abi_encode();
    for (is_static, nested) in [(false, false), (true, false), (false, true)] {
        let mut db = CacheDB::new(EmptyDB::default());
        db.insert_account_info(
            sender,
            AccountInfo {
                balance: U256::from(10_u64.pow(18)),
                ..Default::default()
            },
        );
        // The source series is Issued and its reference currency is admitted:
        // a normal command reaches the real writing child and bubbles 0x42.
        let mut seed = DirectStorageProvider::new(
            &mut db,
            BlockContext::new(1, 1_700_000_000, 1, sender, vec![sender]),
        );
        StorageHandle::enter(&mut seed, |storage| {
            outbe_oracle::schema::OracleContract::new(storage.clone())
                .reference_currencies
                .push(840)
                .unwrap();
            outbe_intex::api::create_series(
                &storage,
                CreateSeriesParams {
                    series_id: series,
                    worldwide_day: WorldwideDay::new(20_261_008),
                    issued_units: 1,
                    promis_load_minor: 1_000_000,
                    entry_price_minor: U256::from(1_000_000),
                    floor_price_minor: U256::ONE,
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
                invalid,
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
            INTEX_NFT1155_ADDRESS,
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
            proxy,
            AccountInfo {
                code_hash: code.hash_slow(),
                code: Some(code),
                ..Default::default()
            },
        );
        let mut evm = OutbeEvmFactory::default().create_evm(
            &mut db,
            EvmEnv {
                cfg_env: CfgEnv::new()
                    .with_chain_id(1)
                    .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
                block_env: BlockEnv {
                    number: U256::ONE,
                    timestamp: U256::from(1_700_000_000),
                    gas_limit: 30_000_000,
                    ..Default::default()
                },
            },
        );
        let outcome = evm
            .transact_raw(
                TxEnv::builder()
                    .caller(sender)
                    .nonce(0)
                    .kind(revm::primitives::TxKind::Call(proxy))
                    .data(Bytes::copy_from_slice(&data))
                    .gas_limit(500_000)
                    .build()
                    .unwrap(),
            )
            .expect("static rejection never aborts execution");
        assert!(outcome.result.is_success(), "proxy catches command failure");
        let output = outcome.result.output().unwrap();
        assert_eq!(&output[..32], &[0; 32]);
        if is_static {
            assert_eq!(output.len(), 32, "admission halt has no child returndata");
        } else if nested {
            assert_eq!(
                output.len(),
                68,
                "nested INVALID bubbles the typed VM error"
            );
            assert_eq!(
                &output[32..36],
                &alloy_primitives::keccak256("SubCallHalted(uint8)")[..4]
            );
            assert_eq!(U256::from_be_slice(&output[36..]), U256::from(7));
        } else {
            assert_eq!(
                &output[32..],
                &[0x42],
                "normal CALL reaches the writing child"
            );
        }
        assert!(outcome.result.logs().is_empty());
        evm.db_mut().commit(outcome.state);
        use revm::Database as _;
        assert_eq!(
            evm.db_mut()
                .storage(INTEX_NFT1155_ADDRESS, U256::ZERO)
                .unwrap(),
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
