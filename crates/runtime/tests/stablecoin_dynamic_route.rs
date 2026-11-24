use alloy_consensus::TxLegacy;
use alloy_primitives::{Address, Bytes, TxKind, U256};
use alloy_sol_types::SolCall;
use evm2::{
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::TxEnvelope,
    evm::{AccountInfo, CacheDB, EmptyDB},
    NoopInspector, SpecId,
};
use outbe_evm::{factory::OutbeEvmTypes, sub_call, test_support::EvmWorld, OutbeEvmFactory};
use outbe_primitives::{
    block::BlockContext,
    storage::{
        direct::DirectStorageProvider, StorageHandle, SubCallError, SubCallInput, SubCallStatus,
    },
};
use outbe_stablecoin::{
    api::{FactoryTokenInitialization, StablecoinFactoryApi as TokenFactoryApi},
    precompile::IStablecoin,
};
use outbe_stablecoinfactory::{FactoryReservation, StablecoinFactoryApi as RegistryApi};
use outbe_system_common::addresses::{
    STABLECOIN_ADDRESS_PREFIX, STABLECOIN_FACTORY_ADDRESS, STABLECOIN_MARKER_CODE,
};
use outbe_system_common::stablecoin::{predict_stablecoin, StablecoinCreatePayload};
use reth_ethereum::evm::EthEvmEnv;
use reth_primitives_traits::Recovered;

const CHAIN_ID: u64 = 1;
const CALLER: Address = Address::new([0xc0; 20]);
const ISSUER: Address = Address::new([0x44; 20]);
const GAS_LIMIT: u64 = 1_000_000;

fn test_env() -> EthEvmEnv<OutbeEvmTypes> {
    EthEvmEnv::new(
        SpecId::PRAGUE,
        BlockEnvExt {
            gas_limit: U256::from(30_000_000_u64),
            ..Default::default()
        },
        CHAIN_ID,
    )
}

fn funded_account() -> AccountInfo {
    AccountInfo {
        balance: U256::from(10u64).pow(U256::from(20u64)),
        ..Default::default()
    }
}

fn seed_registered_tokens(definitions: &[(&str, &str)]) -> (CacheDB<EmptyDB>, Vec<Address>) {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&CALLER, funded_account());

    let mut initializations = Vec::with_capacity(definitions.len());
    for (index, (name, ticker)) in definitions.iter().enumerate() {
        let payload = StablecoinCreatePayload {
            issuer: ISSUER,
            name: (*name).to_owned(),
            ticker: (*ticker).to_owned(),
            iso4217: 840,
            decimals: 6,
            supply_cap: U256::from(1_000_000u64),
            policy_id: U256::from(1u64),
        };
        let (token_id, token) = predict_stablecoin(
            CHAIN_ID,
            STABLECOIN_FACTORY_ADDRESS,
            ISSUER,
            ticker,
            STABLECOIN_ADDRESS_PREFIX,
        )
        .unwrap();
        let marker = Bytecode::new_raw(Bytes::from_static(&STABLECOIN_MARKER_CODE));
        db.insert_account_info(
            &token,
            AccountInfo {
                code_hash: marker.hash_slow(),
                code: Some(marker),
                ..Default::default()
            },
        );
        initializations.push((
            FactoryReservation {
                proposal_id: U256::from(index + 1),
                token_id,
                ticker: (*ticker).to_owned(),
                token,
            },
            FactoryTokenInitialization {
                token_address: token,
                token_id,
                creation_protocol_version: 0,
                payload,
            },
        ));
    }

    {
        let mut provider = DirectStorageProvider::new(
            &mut db,
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 1,
                timestamp: 1,
                chain_id: CHAIN_ID,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: 1,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
        );
        StorageHandle::enter(&mut provider, |storage| {
            for (reservation, initialization) in &initializations {
                RegistryApi::reserve(storage.clone(), reservation).unwrap();
                TokenFactoryApi::new(storage.clone())
                    .initialize(initialization)
                    .unwrap();
                RegistryApi::consume(storage.clone(), reservation.proposal_id).unwrap();
            }
        });
        provider.flush().unwrap();
    }

    let tokens = initializations
        .iter()
        .map(|(_, initialization)| initialization.token_address)
        .collect();
    (db, tokens)
}

fn symbol_calldata() -> Bytes {
    IStablecoin::symbolCall {}.abi_encode().into()
}

fn class_address(last: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[..2].copy_from_slice(&STABLECOIN_ADDRESS_PREFIX);
    bytes[19] = last;
    Address::from(bytes)
}

#[test]
fn top_level_calls_route_by_actual_stablecoin_callee_and_keep_instances_isolated() {
    let (db, tokens) =
        seed_registered_tokens(&[("Example Dollar", "EXUSD"), ("Second Dollar", "SCD")]);
    let mut evm = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
        .create_evm(db, test_env());

    for (token, expected) in tokens.into_iter().zip(["EXUSD", "SCD"]) {
        let tx = Recovered::new_unchecked(
            TxEnvelope::Legacy(TxLegacy {
                chain_id: Some(CHAIN_ID),
                nonce: 0,
                gas_price: 0,
                gas_limit: GAS_LIMIT,
                to: TxKind::Call(token),
                value: U256::ZERO,
                input: symbol_calldata(),
            }),
            CALLER,
        );
        let result = evm.transact(&tx).unwrap().discard();
        assert!(
            result.is_success(),
            "stablecoin symbol call did not succeed: {result:?}"
        );
        assert_eq!(
            IStablecoin::symbolCall::abi_decode_returns(&result.output).unwrap(),
            expected
        );
    }
}

#[test]
fn nested_staticcall_uses_the_same_authenticated_dynamic_route() {
    let (db, tokens) = seed_registered_tokens(&[("Example Dollar", "EXUSD")]);
    let token = tokens[0];
    let host = std::sync::Arc::new(outbe_runtime::OutbeRuntime::new());
    let mut world = EvmWorld::new(db, host.clone());

    let result = sub_call::run_with_host(
        &mut world.evm,
        outbe_evm::test_support::fresh_environment(CALLER, SpecId::PRAGUE),
        SubCallInput {
            target: token,
            value: U256::ZERO,
            calldata: symbol_calldata(),
            gas_limit: GAS_LIMIT,
            is_static: true,
        },
        host,
    )
    .unwrap();

    assert!(matches!(result.status, SubCallStatus::Success));
    assert_eq!(
        IStablecoin::symbolCall::abi_decode_returns(&result.returndata).unwrap(),
        "EXUSD"
    );
}

#[test]
fn nested_static_mutation_halts_without_committing_an_event() {
    let (db, tokens) = seed_registered_tokens(&[("Example Dollar", "EXUSD")]);
    let token = tokens[0];
    let host = std::sync::Arc::new(outbe_runtime::OutbeRuntime::new());
    let mut world = EvmWorld::new(db, host.clone());
    let spender = Address::repeat_byte(0x77);
    let calldata = IStablecoin::approveCall {
        spender,
        value: U256::from(1u64),
    }
    .abi_encode();

    let result = sub_call::run_with_host(
        &mut world.evm,
        outbe_evm::test_support::fresh_environment(CALLER, SpecId::PRAGUE),
        SubCallInput {
            target: token,
            value: U256::ZERO,
            calldata: calldata.into(),
            gas_limit: GAS_LIMIT,
            is_static: true,
        },
        host.clone(),
    )
    .unwrap();

    assert!(matches!(result.status, SubCallStatus::Halt(_)));
    assert!(
        world.logs().is_empty(),
        "static denial must not commit an Approval event to the EVM journal"
    );

    let allowance = sub_call::run_with_host(
        &mut world.evm,
        outbe_evm::test_support::fresh_environment(CALLER, SpecId::PRAGUE),
        SubCallInput {
            target: token,
            value: U256::ZERO,
            calldata: IStablecoin::allowanceCall {
                owner: CALLER,
                spender,
            }
            .abi_encode()
            .into(),
            gas_limit: GAS_LIMIT,
            is_static: true,
        },
        host,
    )
    .unwrap();
    assert!(matches!(allowance.status, SubCallStatus::Success));
    assert_eq!(
        IStablecoin::allowanceCall::abi_decode_returns(&allowance.returndata).unwrap(),
        U256::ZERO
    );
}

#[test]
fn nested_call_with_insufficient_gas_halts_out_of_gas() {
    let (db, tokens) = seed_registered_tokens(&[("Example Dollar", "EXUSD")]);
    let token = tokens[0];
    let host = std::sync::Arc::new(outbe_runtime::OutbeRuntime::new());
    let mut world = EvmWorld::new(db, host.clone());

    let result = sub_call::run_with_host(
        &mut world.evm,
        outbe_evm::test_support::fresh_environment(CALLER, SpecId::PRAGUE),
        SubCallInput {
            target: token,
            value: U256::ZERO,
            calldata: symbol_calldata(),
            gas_limit: 1,
            is_static: true,
        },
        host,
    )
    .unwrap();

    assert!(matches!(
        result.status,
        SubCallStatus::Halt(SubCallError::OutOfGas)
    ));
}

#[test]
fn unregistered_class_member_reverts_without_executing_installed_bytecode() {
    let unknown = class_address(0xfe);
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&CALLER, funded_account());

    // This runtime would return success if the reserved address ever fell
    // through to ordinary EVM bytecode.
    let bytecode = Bytecode::new_raw(Bytes::from_static(&[0x00]));
    db.insert_account_info(
        &unknown,
        AccountInfo {
            code_hash: bytecode.hash_slow(),
            code: Some(bytecode),
            ..Default::default()
        },
    );

    let tx = Recovered::new_unchecked(
        TxEnvelope::Legacy(TxLegacy {
            chain_id: Some(CHAIN_ID),
            nonce: 0,
            gas_price: 0,
            gas_limit: GAS_LIMIT,
            to: TxKind::Call(unknown),
            value: U256::ZERO,
            input: Bytes::new(),
        }),
        CALLER,
    );
    let mut evm = OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
        .create_evm(db, test_env());
    let result = evm.transact(&tx).unwrap().commit();

    assert!(result.is_revert(), "{result:?}");
}

#[test]
fn immediate_delegation_to_native_class_executes_empty_code_regardless_of_issuance() {
    let recipient = Address::new([0xaa; 20]);
    for registered in [false, true] {
        for inspect in [false, true] {
            let (mut db, tokens) = seed_registered_tokens(&[("Example Dollar", "EXUSD")]);
            let target = if registered {
                tokens[0]
            } else {
                class_address(0x99)
            };
            if !registered {
                let marker = Bytecode::new_raw(Bytes::from_static(&STABLECOIN_MARKER_CODE));
                db.insert_account_info(
                    &target,
                    AccountInfo {
                        code_hash: marker.hash_slow(),
                        code: Some(marker),
                        ..Default::default()
                    },
                );
            }
            let delegation = Bytecode::new_eip7702(target);
            db.insert_account_info(
                &recipient,
                AccountInfo {
                    nonce: 2,
                    balance: U256::from(1),
                    code_hash: delegation.hash_slow(),
                    code: Some(delegation.clone()),
                    ..Default::default()
                },
            );
            let mut evm =
                OutbeEvmFactory::new(std::sync::Arc::new(outbe_runtime::OutbeRuntime::new()))
                    .create_evm(db, test_env());
            let tx = Recovered::new_unchecked(
                TxEnvelope::Legacy(TxLegacy {
                    chain_id: Some(CHAIN_ID),
                    nonce: 0,
                    gas_price: 0,
                    gas_limit: GAS_LIMIT,
                    to: TxKind::Call(recipient),
                    value: U256::from(10),
                    input: symbol_calldata(),
                }),
                CALLER,
            );
            let outcome = if inspect {
                evm.transact_with_inspector(&tx, &mut NoopInspector::default())
                    .unwrap()
                    .detach()
            } else {
                evm.transact(&tx).unwrap().detach()
            };
            assert!(
                outcome.result.is_success() && outcome.result.output.is_empty(),
                "native delegate must not execute marker or symbol ABI: {:?}",
                outcome.result
            );
            let recipient_info = outcome
                .pending_state
                .account_changes_for(&recipient)
                .and_then(|acc| acc.current)
                .expect("recipient modified");
            assert_eq!(recipient_info.balance, U256::from(11));
            assert_eq!(recipient_info.nonce, 2);
            assert_eq!(recipient_info.code_hash, delegation.hash_slow());
            assert!(
                outcome
                    .pending_state
                    .account_changes_for(&target)
                    .is_none_or(|acc| !acc.touched && !acc.is_changed()),
                "native delegate target must not be touched"
            );
        }
    }
}
