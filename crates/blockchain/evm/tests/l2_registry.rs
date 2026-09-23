//! L2Registry operator key rotation through real EVM frames.
//!
//! The registry authorizes the *immediate caller* of the precompile frame, so an
//! L2 operator may be a contract: an EOA sends the transaction, the operator
//! contract forwards the calldata with an ordinary `CALL`, and the registry sees
//! the operator as `msg.sender`. These tests drive that shape through
//! `OutbeEvmFactory`, plus the shapes that must be refused - an unregistered
//! forwarder carrying a registered owner as `tx.origin`, a direct unrelated
//! caller, a write-protected `STATICCALL`, and a calling frame that reverts
//! after the registry frame already succeeded.
//!
//! The forwarding bytecode here is test-only helper code, not a production
//! operator contract.

use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{Address, Bytes, Log, U256};
use alloy_sol_types::{SolCall, SolEvent};
use commonware_cryptography::bls12381::primitives::{ops, variant::MinSig};
use outbe_evm::OutbeEvmFactory;
use outbe_l2registry::precompile::IL2Registry;
use outbe_l2registry::L2RegistryContract;
use outbe_primitives::addresses::L2_REGISTRY_ADDRESS;
use outbe_primitives::block::BlockContext;
use outbe_primitives::storage::{direct::DirectStorageProvider, StorageHandle};
use rand_commonware::rngs::ChaCha20Rng;
use rand_commonware::SeedableRng;
use reth_ethereum::evm::primitives::EvmEnv;
use revm::{
    context::{
        result::{ExecutionResult, Output, ResultAndState},
        BlockEnv, CfgEnv, TxEnv,
    },
    database::{CacheDB, EmptyDB},
    primitives::{hardfork::SpecId, TxKind},
    state::{AccountInfo, Bytecode},
    Database, DatabaseCommit,
};

/// L1 chain id this harness runs on.
const CHAIN_ID: u64 = 1;
/// Registered L2 network id.
const L2_CHAIN_ID: u64 = 0xdead;
const NOW: u64 = 1_700_000_000;
const GAS_LIMIT: u64 = 3_000_000;
/// Registered L1 operator when the operator is an EOA.
const OWNER_EOA: Address = Address::new([0x11; 20]);
/// Unrelated EOA: pays for and sends transactions, owns nothing.
const OUTSIDER: Address = Address::new([0x22; 20]);
/// Contract address registered as the network's L1 operator.
const OPERATOR: Address = Address::new([0xcc; 20]);
/// Contract that is never registered as an operator.
const FORWARDER: Address = Address::new([0xdd; 20]);

/// How a test contract reaches the registry precompile.
#[derive(Clone, Copy, PartialEq)]
enum Forward {
    /// Ordinary `CALL`, the shape a production operator contract uses.
    Call,
    /// `STATICCALL`, i.e. a write-protected frame.
    StaticCall,
    /// `CALL` followed by an unconditional `REVERT` carrying the inner frame's
    /// success flag, so the caller can tell a rotated-then-rolled-back frame
    /// apart from a refused one.
    CallThenRevert,
}

fn test_env() -> EvmEnv {
    EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
        block_env: BlockEnv {
            timestamp: U256::from(NOW),
            gas_limit: 30_000_000,
            ..Default::default()
        },
    }
}

fn funded() -> AccountInfo {
    AccountInfo {
        balance: U256::from(1_000_000_000u64),
        ..Default::default()
    }
}

/// A deterministic BLS MinSig G2 public key in the registry's external
/// EIP-2537 encoding (256 bytes). The registry only requires those bytes to
/// decode as a group element, so keypairs from a seeded RNG are valid fixtures.
fn bls_key(seed: u64) -> Bytes {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let (_, public) = ops::keypair::<_, MinSig>(&mut rng);
    Bytes::from(
        outbe_l2registry::public_key::encode(&public).expect("fixture L2 key encodes as EIP-2537"),
    )
}

/// Test-only calldata forwarder that bubbles the registry's return or revert.
fn forward_code(mode: Forward) -> Bytes {
    let opcode = if mode == Forward::StaticCall {
        0xfa // STATICCALL
    } else {
        0xf1 // CALL
    };
    let mut code = vec![
        0x36, // CALLDATASIZE      size
        0x60, 0x00, // PUSH1 0     offset
        0x60, 0x00, // PUSH1 0     destOffset
        0x37, // CALLDATACOPY
        0x60, 0x00, // PUSH1 0     retLength
        0x60, 0x00, // PUSH1 0     retOffset
        0x36, // CALLDATASIZE      argsLength
        0x60, 0x00, // PUSH1 0     argsOffset
    ];
    if mode != Forward::StaticCall {
        code.extend_from_slice(&[0x60, 0x00]); // PUSH1 0   value
    }
    code.push(0x73); // PUSH20            target
    code.extend_from_slice(L2_REGISTRY_ADDRESS.as_slice());
    code.push(0x5a); // GAS
    code.push(opcode);
    code.extend_from_slice(&[
        0x3d, // RETURNDATASIZE    size
        0x60, 0x00, // PUSH1 0     offset
        0x60, 0x00, // PUSH1 0     destOffset
        0x3e, // RETURNDATACOPY
    ]);

    match mode {
        Forward::Call | Forward::StaticCall => {
            // JUMPI to the return stub when the inner frame succeeded; the
            // fallthrough reverts with the callee's returndata.
            let success = u8::try_from(code.len() + 7).expect("jump target fits PUSH1");
            code.extend_from_slice(&[0x60, success, 0x57]); // PUSH1 success, JUMPI
            code.extend_from_slice(&[0x3d, 0x60, 0x00, 0xfd]); // REVERT(0, returndatasize)
            code.push(0x5b); // JUMPDEST
            code.extend_from_slice(&[0x3d, 0x60, 0x00, 0xf3]); // RETURN(0, returndatasize)
        }
        Forward::CallThenRevert => {
            // Stack: [success] - MEM[0..32] = success, then always revert with it.
            code.extend_from_slice(&[
                0x60, 0x00, // PUSH1 0   offset
                0x52, // MSTORE
                0x60, 0x20, // PUSH1 32  size
                0x60, 0x00, // PUSH1 0   offset
                0xfd, // REVERT
            ]);
        }
    }
    Bytes::from(code)
}

fn deploy_forwarder(db: &mut CacheDB<EmptyDB>, address: Address, mode: Forward) {
    let code = Bytecode::new_raw(forward_code(mode));
    let mut info = funded();
    info.code_hash = code.hash_slow();
    info.code = Some(code);
    db.insert_account_info(address, info);
}

fn with_storage<R>(db: &mut CacheDB<EmptyDB>, f: impl FnOnce(StorageHandle<'_>) -> R) -> R {
    let mut provider =
        DirectStorageProvider::new(db, BlockContext::empty_for_tests(1, NOW, CHAIN_ID));
    let result = StorageHandle::enter(&mut provider, f);
    provider.flush().unwrap();
    result
}

/// A chain with the registry precompile marked (as fresh genesis and the block
/// executor do) and `L2_CHAIN_ID` registered to `l1_address` by the production
/// runtime.
fn db_with_registry(l1_address: Address, public_key: &[u8]) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(OWNER_EOA, funded());
    db.insert_account_info(OUTSIDER, funded());
    let marker = Bytecode::new_raw(Bytes::from_static(&[0xef]));
    db.insert_account_info(
        L2_REGISTRY_ADDRESS,
        AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker),
            ..Default::default()
        },
    );
    with_storage(&mut db, |storage| {
        L2RegistryContract::new(storage)
            .register_network(L2_CHAIN_ID, l1_address, public_key)
            .expect("seeded registration")
    });
    db
}

/// Runs `data` against `to` from `caller` and returns the whole outcome, so
/// callers can assert on post-state as well as on the result.
fn run(db: &mut CacheDB<EmptyDB>, caller: Address, to: Address, data: Vec<u8>) -> ResultAndState {
    let nonce = db
        .basic(caller)
        .expect("caller account loads")
        .map_or(0, |info| info.nonce);
    let mut evm = OutbeEvmFactory::new().create_evm(&mut *db, test_env());
    let tx = TxEnv::builder()
        .caller(caller)
        .nonce(nonce)
        .kind(TxKind::Call(to))
        .value(U256::ZERO)
        .data(Bytes::from(data))
        .gas_limit(GAS_LIMIT)
        .build()
        .expect("tx builds");
    evm.transact_raw(tx).expect("transaction executes")
}

/// Commits a finished transaction so later calls observe its post-state.
fn commit(db: &mut CacheDB<EmptyDB>, outcome: ResultAndState) {
    db.commit(outcome.state);
}

fn revert_output(result: &ExecutionResult) -> Option<&Bytes> {
    match result {
        ExecutionResult::Revert { output, .. } => Some(output),
        _ => None,
    }
}

/// The registry's key-update log as the transaction committed it, if any.
fn key_updated_event(result: &ExecutionResult) -> Option<Log<IL2Registry::L2PublicKeyUpdated>> {
    result
        .logs()
        .iter()
        .find_map(|log| IL2Registry::L2PublicKeyUpdated::decode_log(log).ok())
}

/// Calls the registry like any other EVM caller, without committing the read.
fn view(db: &mut CacheDB<EmptyDB>, data: Vec<u8>) -> Bytes {
    match run(db, OUTSIDER, L2_REGISTRY_ADDRESS, data).result {
        ExecutionResult::Success {
            output: Output::Call(bytes),
            ..
        } => bytes,
        other => panic!("registry view failed: {other:?}"),
    }
}

/// Reads the registered network the way a downstream consumer does.
fn read_network(db: &mut CacheDB<EmptyDB>) -> (Address, Bytes) {
    let data = IL2Registry::getNetworkCall {
        chainId: L2_CHAIN_ID,
    }
    .abi_encode();
    let decoded = IL2Registry::getNetworkCall::abi_decode_returns(&view(db, data))
        .expect("getNetwork return decodes");
    (decoded.l1Address, decoded.publicKey)
}

fn chain_id_by_l1(db: &mut CacheDB<EmptyDB>, l1_address: Address) -> u64 {
    let data = IL2Registry::chainIdByL1AddressCall {
        l1Address: l1_address,
    }
    .abi_encode();
    IL2Registry::chainIdByL1AddressCall::abi_decode_returns(&view(db, data))
        .expect("chainIdByL1Address return decodes")
}

fn rotate_calldata(public_key: &[u8]) -> Vec<u8> {
    IL2Registry::updatePublicKeyCall {
        chainId: L2_CHAIN_ID,
        publicKey: Bytes::copy_from_slice(public_key),
    }
    .abi_encode()
}

/// An L2 operator may be a contract. The outer EOA never touches the registry:
/// it calls the operator contract, which forwards the calldata with an ordinary
/// `CALL`, and the registry authorizes that contract as its caller.
#[test]
fn operator_contract_rotates_key_through_nested_call() {
    let old = bls_key(1);
    let new = bls_key(2);
    let mut db = db_with_registry(OPERATOR, &old);
    deploy_forwarder(&mut db, OPERATOR, Forward::Call);
    assert_eq!(read_network(&mut db), (OPERATOR, old));

    let outcome = run(&mut db, OUTSIDER, OPERATOR, rotate_calldata(&new));
    assert!(
        matches!(outcome.result, ExecutionResult::Success { .. }),
        "nested CALL must succeed, got {:?}",
        outcome.result
    );
    let event = key_updated_event(&outcome.result).expect("rotation emits L2PublicKeyUpdated");
    assert_eq!(event.address, L2_REGISTRY_ADDRESS);
    assert_eq!(event.data.chainId, L2_CHAIN_ID);
    assert_eq!(event.data.publicKey, new);
    commit(&mut db, outcome);

    assert_eq!(read_network(&mut db), (OPERATOR, new));
    assert_eq!(
        chain_id_by_l1(&mut db, OPERATOR),
        L2_CHAIN_ID,
        "the reverse index still resolves to the network"
    );
}

/// Authority follows the immediate caller, not `tx.origin`: the registered owner
/// rotates its own key by calling the registry directly, while a transaction the
/// same owner sends through an unregistered forwarding contract cannot.
#[test]
fn forwarding_contract_cannot_reuse_the_registered_origin_authority() {
    let old = bls_key(3);
    let rotated = bls_key(4);
    let forwarded = bls_key(5);
    let mut db = db_with_registry(OWNER_EOA, &old);
    deploy_forwarder(&mut db, FORWARDER, Forward::Call);

    let outcome = run(
        &mut db,
        OWNER_EOA,
        L2_REGISTRY_ADDRESS,
        rotate_calldata(&rotated),
    );
    assert!(
        matches!(outcome.result, ExecutionResult::Success { .. }),
        "the registered EOA operator rotates directly, got {:?}",
        outcome.result
    );
    commit(&mut db, outcome);
    assert_eq!(read_network(&mut db), (OWNER_EOA, rotated.clone()));

    let outcome = run(&mut db, OWNER_EOA, FORWARDER, rotate_calldata(&forwarded));
    assert!(revert_output(&outcome.result).is_some());
    commit(&mut db, outcome);
    assert_eq!(
        read_network(&mut db),
        (OWNER_EOA, rotated),
        "the refused rotation leaves the key as the owner's last one"
    );
    assert_eq!(chain_id_by_l1(&mut db, OWNER_EOA), L2_CHAIN_ID);
}

/// A caller unrelated to the stored operator cannot rotate, even when the
/// record's operator is a contract.
#[test]
fn unrelated_eoa_cannot_rotate_contract_owned_record() {
    let old = bls_key(6);
    let mut db = db_with_registry(OPERATOR, &old);
    deploy_forwarder(&mut db, OPERATOR, Forward::Call);

    let outcome = run(
        &mut db,
        OUTSIDER,
        L2_REGISTRY_ADDRESS,
        rotate_calldata(&bls_key(7)),
    );
    assert!(
        revert_output(&outcome.result).is_some(),
        "direct unrelated caller must be refused, got {:?}",
        outcome.result
    );
    commit(&mut db, outcome);
    assert_eq!(read_network(&mut db), (OPERATOR, old));
    assert_eq!(chain_id_by_l1(&mut db, OPERATOR), L2_CHAIN_ID);
}

/// The operator contract itself cannot rotate from a write-protected frame: the
/// key writes and the event are refused inside `STATICCALL`.
#[test]
fn static_call_cannot_rotate_key() {
    let old = bls_key(8);
    let mut db = db_with_registry(OPERATOR, &old);
    deploy_forwarder(&mut db, OPERATOR, Forward::StaticCall);

    let outcome = run(&mut db, OUTSIDER, OPERATOR, rotate_calldata(&bls_key(9)));
    assert!(
        !matches!(outcome.result, ExecutionResult::Success { .. }),
        "a static frame cannot rotate, got {:?}",
        outcome.result
    );
    assert!(
        key_updated_event(&outcome.result).is_none(),
        "a refused rotation emits nothing"
    );
    commit(&mut db, outcome);
    assert_eq!(read_network(&mut db), (OPERATOR, old));
}

/// Rotation writes three key words and an event, so it has to roll back with the
/// frame that called it: the registry frame succeeds, the contract then reverts,
/// and the previously stored key is what consumers still read.
#[test]
fn key_rotation_rolls_back_with_the_calling_frame() {
    let old = bls_key(10);
    let new = bls_key(11);
    let mut db = db_with_registry(OPERATOR, &old);
    deploy_forwarder(&mut db, OPERATOR, Forward::CallThenRevert);

    let outcome = run(&mut db, OUTSIDER, OPERATOR, rotate_calldata(&new));
    let revert =
        revert_output(&outcome.result).expect("the operator contract reverts after rotating");
    assert_eq!(revert.len(), 32, "revert carries the inner success flag");
    assert_eq!(
        U256::from_be_slice(revert),
        U256::ONE,
        "the registry frame itself succeeded; only the caller's revert undoes it"
    );
    assert!(
        key_updated_event(&outcome.result).is_none(),
        "a reverted transaction keeps no log"
    );
    commit(&mut db, outcome);
    assert_eq!(read_network(&mut db), (OPERATOR, old));
    assert_eq!(chain_id_by_l1(&mut db, OPERATOR), L2_CHAIN_ID);
}
