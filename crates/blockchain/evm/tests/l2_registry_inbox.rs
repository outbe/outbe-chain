//! Live `IDaInbox.groupPubKey()` key resolution through real EVM frames.
//!
//! A network registered without a pinned key resolves its BLS key by
//! STATICCALLing its registered L1 address on every read, so `getNetwork` and
//! `TributeFactory.offerTribute` both see the inbox's current answer. The inbox
//! here is hand-written runtime bytecode answering `abi.encode(bytes)` with a
//! 256-byte EIP-2537 G2 key; registration goes through
//! `DirectStorageProvider` and reads through `OutbeEvmFactory`. Provider-level
//! coverage of the same resolution lives with the registry.

use std::sync::Arc;

use alloy_evm::{Evm as _, EvmFactory as _};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolError};
use commonware_codec::Encode;
use commonware_cryptography::bls12381::primitives::{
    ops::{self, sign_message},
    variant::MinSig,
};
use outbe_evm::OutbeEvmFactory;
use outbe_l2registry::{
    api::ZK_MERKLE_ROOT_NAMESPACE, errors::L2RegistryError, precompile::IL2Registry, public_key,
    schema::L2RegistryContract,
};
use outbe_offchain_data::RuntimeBodyReaders;
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle};
use outbe_primitives::{
    addresses::{L2_REGISTRY_ADDRESS, TRIBUTE_FACTORY_ADDRESS},
    block::BlockContext,
    storage::{direct::DirectStorageProvider, StorageHandle},
};
use outbe_tributefactory::{errors::TributeFactoryError, precompile::ITributeFactory};
use reth_ethereum::evm::primitives::EvmEnv;
use revm::{
    context::{
        result::{ExecutionResult, Output, ResultAndState},
        BlockEnv, CfgEnv, TxEnv,
    },
    database::{CacheDB, EmptyDB},
    primitives::{hardfork::SpecId, TxKind},
    state::{AccountInfo, Bytecode},
    Database,
};

sol! {
    /// Settlement-side getter: 256 bytes of EIP-2537 G2.
    interface IDaInbox {
        function groupPubKey() external view returns (bytes memory);
    }
}

const CHAIN_ID: u64 = 1;
const L2_CHAIN_ID: u64 = 57_005;
const NOW: u64 = 1_700_000_000;
const DAY: u32 = 20_250_115;
const GAS_LIMIT: u64 = 1_000_000;
const CALLER: Address = Address::repeat_byte(0xcc);

const INBOX: Address = Address::repeat_byte(0x1e);
const REVERTER: Address = Address::repeat_byte(0x2e);
const EMPTY_RETURN: Address = Address::repeat_byte(0x3e);
const JUNK_RETURN: Address = Address::repeat_byte(0x4e);
const SHORT_RETURN: Address = Address::repeat_byte(0x5e);
const WRITER: Address = Address::repeat_byte(0x6e);
/// No account is ever inserted at this address.
const NO_CODE: Address = Address::repeat_byte(0x7e);
const LOOPING: Address = Address::repeat_byte(0x8e);
const INVALID_OPCODE: Address = Address::repeat_byte(0x9e);

const ROOT: [u8; 32] = [0x42; 32];

/// One network key: the canonical EIP-2537 G2 encoding every registry boundary
/// serves, and a real signature over [`ROOT`].
struct NetworkKey {
    public_key: [u8; 256],
    signature: [u8; 48],
}

/// Deterministic MinSig fixture: the key derived from `seed`, its canonical
/// EIP-2537 encoding, and a real signature over [`ROOT`] in the registry
/// namespace.
fn network_key(seed: u8) -> NetworkKey {
    let mut rng =
        <rand_commonware::rngs::StdRng as rand_commonware::SeedableRng>::from_seed([seed; 32]);
    let (private, public) = ops::keypair::<_, MinSig>(&mut rng);
    NetworkKey {
        public_key: public_key::encode(&public).expect("the fixture key encodes"),
        signature: sign_message::<MinSig>(&private, ZK_MERKLE_ROOT_NAMESPACE, &ROOT)
            .encode()
            .to_vec()
            .try_into()
            .expect("MinSig signatures are 48 bytes"),
    }
}

/// Implements `groupPubKey()` by ABI-encoding storage slots 0..8.
/// `prefix` runs after selector validation (used to probe STATICCALL protection).
fn inbox_code(prefix: &[u8]) -> Bytes {
    let mut code = vec![
        0x60, 0x00, 0x35, // CALLDATALOAD(0)
        0x60, 0xe0, 0x1c, // SHR(224)
        0x63, // PUSH4 selector
    ];
    code.extend_from_slice(&IDaInbox::groupPubKeyCall::SELECTOR);
    code.extend_from_slice(&[
        0x14, 0x60, 0x14, 0x57, // EQ; jump to byte 20 on a match
        0x60, 0x00, 0x60, 0x00, 0xfd, // REVERT(0, 0)
        0x5b, // JUMPDEST at byte 20
    ]);
    code.extend_from_slice(prefix);
    code.extend_from_slice(&[
        0x60, 0x20, 0x60, 0x00, 0x52, // MSTORE(0, bytes offset = 32)
        0x61, 0x01, 0x00, 0x60, 0x20, 0x52, // MSTORE(32, bytes length = 256)
    ]);
    for slot in 0u8..8 {
        let [hi, lo] = (0x40 + u16::from(slot) * 32).to_be_bytes();
        code.extend_from_slice(&[
            0x60, slot, 0x54, // SLOAD(slot)
            0x61, hi, lo, 0x52, // MSTORE(64 + slot * 32, word)
        ]);
    }
    code.extend_from_slice(&[0x61, 0x01, 0x40, 0x60, 0x00, 0xf3]); // RETURN(0, 320)
    code.into()
}

/// `PUSH2 0x1234; PUSH1 0x99; SSTORE`: SSTORE pops the key first, so this is
/// a write of [`WRITTEN_VALUE`] to [`WRITTEN_SLOT`], which the STATICCALL
/// must refuse.
const WRITE_ATTEMPT: &[u8] = &[0x61, 0x12, 0x34, 0x60, 0x99, 0x55];
const WRITTEN_SLOT: u64 = 0x99;
const WRITTEN_VALUE: u64 = 0x1234;

/// Stubs that cannot answer `groupPubKey()`.
const REVERT_STUB: &[u8] = &[0x60, 0x00, 0x60, 0x00, 0xfd];
const EMPTY_STUB: &[u8] = &[0x60, 0x00, 0x60, 0x00, 0xf3];
/// 32 zero bytes, which is not ABI `bytes`.
const JUNK_STUB: &[u8] = &[0x60, 0x20, 0x60, 0x00, 0xf3];
/// ABI `bytes` declaring 255 bytes instead of 256.
const SHORT_STUB: &[u8] = &[
    0x60, 0x20, 0x60, 0x00, 0x52, 0x60, 0xff, 0x60, 0x20, 0x52, 0x61, 0x01, 0x3f, 0x60, 0x00, 0xf3,
];

fn insert_code(db: &mut CacheDB<EmptyDB>, address: Address, code: Bytes) {
    let code = Bytecode::new_raw(code);
    let info = AccountInfo {
        code_hash: code.hash_slow(),
        code: Some(code),
        ..Default::default()
    };
    db.insert_account_info(address, info);
}

/// Seeds the 256-byte key into the inbox's own storage slots 0..8.
fn store_key(db: &mut CacheDB<EmptyDB>, address: Address, key: &[u8; 256]) {
    for (slot, word) in key.chunks_exact(32).enumerate() {
        db.insert_account_storage(address, U256::from(slot as u64), U256::from_be_slice(word))
            .expect("inbox key slots seed");
    }
}

/// State holding every stub: a funded caller, the `0xef` precompile markers
/// genesis installs, and `INBOX`/`WRITER` answering `key`.
fn database(key: &[u8; 256]) -> CacheDB<EmptyDB> {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        CALLER,
        AccountInfo {
            balance: U256::from(1_000_000_000u64),
            ..Default::default()
        },
    );
    let marker = Bytecode::new_legacy([0xef].into());
    for address in [L2_REGISTRY_ADDRESS, TRIBUTE_FACTORY_ADDRESS] {
        let info = AccountInfo {
            code_hash: marker.hash_slow(),
            code: Some(marker.clone()),
            ..Default::default()
        };
        db.insert_account_info(address, info);
    }
    insert_code(&mut db, INBOX, inbox_code(&[]));
    store_key(&mut db, INBOX, key);
    insert_code(&mut db, REVERTER, Bytes::from_static(REVERT_STUB));
    insert_code(&mut db, EMPTY_RETURN, Bytes::from_static(EMPTY_STUB));
    insert_code(&mut db, JUNK_RETURN, Bytes::from_static(JUNK_STUB));
    insert_code(&mut db, SHORT_RETURN, Bytes::from_static(SHORT_STUB));
    insert_code(&mut db, WRITER, inbox_code(WRITE_ATTEMPT));
    store_key(&mut db, WRITER, key);
    // JUMPDEST; PUSH1 0; JUMP. The getter's gas budget must stop this loop.
    insert_code(
        &mut db,
        LOOPING,
        Bytes::from_static(&[0x5b, 0x60, 0x00, 0x56]),
    );
    insert_code(&mut db, INVALID_OPCODE, Bytes::from_static(&[0xfe]));
    db
}

/// Registers `chain_id` for `l1_address` with `public_key`; an empty key or 256
/// zero bytes selects live inbox resolution, in the same state the EVM reads.
fn register(db: &mut CacheDB<EmptyDB>, chain_id: u64, l1_address: Address, public_key: &[u8]) {
    let context = BlockContext::empty_for_tests(1, NOW, CHAIN_ID);
    let mut provider = DirectStorageProvider::new(db, context);
    StorageHandle::enter(&mut provider, |storage| {
        L2RegistryContract::new(storage)
            .register_network(chain_id, l1_address, public_key)
            .expect("the network registers");
    });
    provider.flush().expect("the registration flushes");
}

/// Executes and commits a transaction, preserving effects for subsequent reads.
fn call(db: &mut CacheDB<EmptyDB>, to: Address, calldata: Bytes) -> ExecutionResult {
    let nonce = db
        .basic(CALLER)
        .expect("the caller account loads")
        .map_or(0, |info| info.nonce);
    let env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
        block_env: BlockEnv {
            timestamp: U256::from(NOW),
            gas_limit: 30_000_000,
            ..Default::default()
        },
    };
    let reader: StorageReaderHandle = Arc::new(MemoryStorage::new());
    let factory = OutbeEvmFactory::with_runtime_body_readers(RuntimeBodyReaders::new(reader));
    let mut evm = factory.create_evm(&mut *db, env);
    let ResultAndState { result, state } = evm
        .transact_raw(
            TxEnv::builder()
                .caller(CALLER)
                .nonce(nonce)
                .kind(TxKind::Call(to))
                .value(U256::ZERO)
                .data(calldata)
                .gas_limit(GAS_LIMIT)
                .build()
                .expect("the transaction builds"),
        )
        .expect("contract failure must be a transaction result, not a provider error");
    drop(evm);
    revm::DatabaseCommit::commit(db, state);
    result
}

fn get_network(db: &mut CacheDB<EmptyDB>, chain_id: u64) -> ExecutionResult {
    let calldata = IL2Registry::getNetworkCall { chainId: chain_id }.abi_encode();
    call(db, L2_REGISTRY_ADDRESS, calldata.into())
}

/// The decoded `getNetwork` answer; panics when the call did not succeed.
fn network_of(db: &mut CacheDB<EmptyDB>, chain_id: u64) -> IL2Registry::getNetworkReturn {
    match get_network(db, chain_id) {
        ExecutionResult::Success {
            output: Output::Call(bytes),
            ..
        } => IL2Registry::getNetworkCall::abi_decode_returns(&bytes).expect("the answer decodes"),
        other => panic!("expected a successful getNetwork, got {other:?}"),
    }
}

/// Offers a Tribute for `chain_id` signed by `key`, with no ZK proof, so the
/// call stops at the first gate after the key signature is accepted.
fn offer(db: &mut CacheDB<EmptyDB>, chain_id: u64, key: &NetworkKey) -> ExecutionResult {
    let calldata = ITributeFactory::offerTributeCall {
        cipherText: Bytes::new(),
        nonce: Bytes::new(),
        ephemeralPubkey: U256::ZERO,
        worldwideDay: DAY,
        tributeCurrency: 840,
        referenceCurrency: 840,
        excludeFromIntexIssuance: false,
        zkProof: Bytes::new(),
        chainId: u32::try_from(chain_id).expect("the L2 chain id fits uint32"),
        version: "1.1.0".to_owned(),
        zkPublicKey: Bytes::new(),
        zkMerkleRoot: Bytes::copy_from_slice(&ROOT),
        signature: Bytes::copy_from_slice(&key.signature),
    }
    .abi_encode();
    call(db, TRIBUTE_FACTORY_ADDRESS, calldata.into())
}

/// Solidity `Error(string)` reason, when the call reverted with one.
fn revert_reason(outcome: &ExecutionResult) -> Option<String> {
    let ExecutionResult::Revert { output, .. } = outcome else {
        return None;
    };
    alloy_sol_types::Revert::abi_decode(output)
        .ok()
        .map(|revert| revert.reason)
}

#[test]
fn unset_key_resolves_from_the_inbox_and_gates_tribute() {
    let key = network_key(1);
    let rotated = network_key(2);
    let stranger = network_key(3);
    let proof_required = TributeFactoryError::ZkProofRequired.to_string();
    let invalid_signature = L2RegistryError::InvalidZkSignature.to_string();
    let mut db = database(&key.public_key);
    // The empty key selects live resolution instead of a pinned key.
    register(&mut db, L2_CHAIN_ID, INBOX, &[]);

    let answer = network_of(&mut db, L2_CHAIN_ID);
    assert_eq!(answer.l1Address, INBOX);
    assert_eq!(answer.publicKey.as_ref(), key.public_key.as_slice());

    // The resolved key authenticates a real signature, so the offer fails at
    // the proof gate and not at the signature gate.
    let outcome = offer(&mut db, L2_CHAIN_ID, &key);
    assert_eq!(
        revert_reason(&outcome).as_deref(),
        Some(proof_required.as_str())
    );
    let outcome = offer(&mut db, L2_CHAIN_ID, &stranger);
    assert_eq!(
        revert_reason(&outcome).as_deref(),
        Some(invalid_signature.as_str()),
    );

    // Nothing is cached: rotating the inbox answer rotates the accepted key,
    // with no registry write.
    store_key(&mut db, INBOX, &rotated.public_key);
    let answer = network_of(&mut db, L2_CHAIN_ID);
    assert_eq!(answer.l1Address, INBOX);
    assert_eq!(answer.publicKey.as_ref(), rotated.public_key.as_slice());
    assert_eq!(
        revert_reason(&offer(&mut db, L2_CHAIN_ID, &key)).as_deref(),
        Some(invalid_signature.as_str()),
        "the key the inbox no longer reports must stop authenticating"
    );
    let outcome = offer(&mut db, L2_CHAIN_ID, &rotated);
    assert_eq!(
        revert_reason(&outcome).as_deref(),
        Some(proof_required.as_str())
    );
}

#[test]
fn pinned_key_wins_over_the_inbox() {
    let pinned = network_key(4);
    let live = network_key(5);
    let proof_required = TributeFactoryError::ZkProofRequired.to_string();
    // `INBOX` answers `live`, so a pinned network must not consult it.
    let mut db = database(&live.public_key);

    for (index, l1_address) in [INBOX, REVERTER, NO_CODE].into_iter().enumerate() {
        let chain_id = L2_CHAIN_ID + index as u64;
        register(&mut db, chain_id, l1_address, &pinned.public_key);

        let answer = network_of(&mut db, chain_id);
        assert_eq!(answer.l1Address, l1_address);
        assert_eq!(
            answer.publicKey.as_ref(),
            pinned.public_key.as_slice(),
            "a pinned key must be served without the inbox at {l1_address}"
        );
        let outcome = offer(&mut db, chain_id, &pinned);
        assert_eq!(
            revert_reason(&outcome).as_deref(),
            Some(proof_required.as_str())
        );
        assert_eq!(
            revert_reason(&offer(&mut db, chain_id, &live)),
            Some(L2RegistryError::InvalidZkSignature.to_string()),
            "the inbox key must not authenticate a network with a pinned key"
        );
    }
}

#[test]
fn unresolvable_inbox_keys_fail_closed() {
    let key = network_key(6);
    let invalid_key = L2RegistryError::InvalidPublicKey.to_string();
    let failed_call = L2RegistryError::InboxKeyCallFailed.to_string();
    let mut db = database(&key.public_key);

    let targets = [
        (NO_CODE, Some(invalid_key.as_str())),
        (EMPTY_RETURN, Some(invalid_key.as_str())),
        (JUNK_RETURN, Some(invalid_key.as_str())),
        (SHORT_RETURN, Some(invalid_key.as_str())),
        (REVERTER, None),
        (LOOPING, Some(failed_call.as_str())),
        (INVALID_OPCODE, Some(failed_call.as_str())),
    ];
    for (index, (l1_address, reason)) in targets.into_iter().enumerate() {
        let chain_id = L2_CHAIN_ID + index as u64;
        // The explicit all-zero key is the same unset sentinel as `&[]`.
        register(&mut db, chain_id, l1_address, &[0u8; 256]);

        let outcome = get_network(&mut db, chain_id);
        assert!(
            matches!(outcome, ExecutionResult::Revert { .. }),
            "getNetwork must fail closed for {l1_address}: {outcome:?}"
        );
        assert_eq!(revert_reason(&outcome).as_deref(), reason);
        let offered = offer(&mut db, chain_id, &key);
        assert!(matches!(offered, ExecutionResult::Revert { .. }));
        assert_eq!(revert_reason(&offered).as_deref(), reason);
    }
}

#[test]
fn inbox_cannot_write_its_own_state_through_the_resolver() {
    let key = network_key(7);
    let mut db = database(&key.public_key);
    // `WRITER` holds a valid key, so only the write attempt can stop it from
    // answering.
    register(&mut db, L2_CHAIN_ID, WRITER, &[]);

    let outcome = get_network(&mut db, L2_CHAIN_ID);
    assert!(
        matches!(outcome, ExecutionResult::Revert { .. }),
        "a getter that writes storage must revert, not resolve a key: {outcome:?}"
    );
    assert_eq!(
        db.storage(WRITER, U256::from(WRITTEN_SLOT)).unwrap(),
        U256::ZERO
    );
    assert!(matches!(
        offer(&mut db, L2_CHAIN_ID, &key),
        ExecutionResult::Revert { .. }
    ));
    assert_eq!(
        db.storage(WRITER, U256::from(WRITTEN_SLOT)).unwrap(),
        U256::ZERO
    );

    // The same inbox can write and return a key in a non-static call.
    assert!(call(
        &mut db,
        WRITER,
        IDaInbox::groupPubKeyCall {}.abi_encode().into()
    )
    .is_success());
    assert_eq!(
        db.storage(WRITER, U256::from(WRITTEN_SLOT)).unwrap(),
        U256::from(WRITTEN_VALUE)
    );
}
