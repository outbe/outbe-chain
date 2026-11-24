use alloy_primitives::{Address, Bytes, LogData, U256};
use evm2::{
    evm::{CacheDB, DynDatabase, EmptyDB},
    StorageKey,
};
use outbe_primitives::{
    block::BlockContext,
    error::PrecompileError,
    storage::{direct::DirectStorageProvider, AccountInfo, Bytecode, PrecompileStorageProvider},
};

const FROM: Address = Address::new([0x11; 20]);
const TO: Address = Address::new([0x22; 20]);
const SLOT: U256 = U256::from_limbs([7, 0, 0, 0]);

fn block_context() -> BlockContext {
    BlockContext {
        block_gas_limit: 0,
        user_gas_used: 0,
        block_number: 7,
        timestamp: 1_700_000_000,
        chain_id: 54_322_345,
        genesis_hash: alloy_primitives::B256::ZERO,
        genesis_timestamp: 1_700_000_000,
        proposer: FROM,
        validators: vec![FROM],
    }
}

fn account(balance: U256, nonce: u64, code: Bytecode) -> AccountInfo {
    AccountInfo::new(balance, nonce, code.hash_slow(), Some(code))
}

#[test]
fn set_code_flushes_code_hash_and_exposes_the_account_change() {
    let mut db = CacheDB::new(EmptyDB::default());
    let mut provider = DirectStorageProvider::new(&mut db, block_context());
    let code = Bytecode::new_raw(Bytes::from_static(&[0xef]));
    let expected_hash = code.hash_slow();

    provider.set_code(TO, code.clone()).unwrap();
    assert_eq!(provider.account_info(TO).unwrap().code, Some(code.clone()));
    provider.flush().unwrap();
    let changes = provider.take_committed_changes();
    let changed = changes
        .accounts()
        .find(|(addr, _)| *addr == TO)
        .and_then(|(_, delta)| delta.current.as_ref())
        .expect("code account change");
    assert_eq!(changed.code_hash, expected_hash);
    let emitted_code = changes
        .code()
        .find(|(hash, _)| **hash == expected_hash)
        .map(|(_, bytecode)| bytecode)
        .expect("emitted bytecode");
    assert_eq!(emitted_code, &code);
    drop(provider);

    let persisted = db
        .get_account(&TO)
        .unwrap()
        .expect("persisted code account");
    assert_eq!(persisted.code_hash, expected_hash);
    assert_eq!(
        db.get_code_by_hash(&expected_hash)
            .unwrap()
            .original_bytes()
            .as_ref(),
        &[0xef]
    );
}

#[test]
fn nested_checkpoint_revert_restores_code_storage_balance_and_events() {
    let original_code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x00]));
    let outer_code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x01]));
    let inner_code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x02]));
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&FROM, account(U256::from(100), 3, original_code.clone()));
    db.insert_account_info(&TO, AccountInfo::default());
    let mut provider = DirectStorageProvider::new(&mut db, block_context());

    let outer = provider.checkpoint();
    provider.set_code(FROM, outer_code.clone()).unwrap();
    provider.sstore(FROM, SLOT, U256::from(10)).unwrap();
    provider
        .emit_event(
            FROM,
            LogData::new_unchecked(vec![], Bytes::from_static(&[0x01])),
        )
        .unwrap();
    provider.transfer_balance(FROM, TO, U256::from(20)).unwrap();

    let inner = provider.checkpoint();
    provider.set_code(FROM, inner_code).unwrap();
    provider.sstore(FROM, SLOT, U256::from(99)).unwrap();
    provider
        .emit_event(
            TO,
            LogData::new_unchecked(vec![], Bytes::from_static(&[0x02])),
        )
        .unwrap();
    provider.transfer_balance(FROM, TO, U256::from(30)).unwrap();
    provider.checkpoint_revert(inner).unwrap();

    assert_eq!(provider.sload(FROM, SLOT).unwrap(), U256::from(10));
    assert_eq!(provider.account_info(FROM).unwrap().balance, U256::from(80));
    assert_eq!(provider.account_info(FROM).unwrap().code, Some(outer_code));
    assert_eq!(provider.account_info(TO).unwrap().balance, U256::from(20));
    let events = provider.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].address, FROM);
    assert_eq!(events[0].data.data, Bytes::from_static(&[0x01]));

    provider.checkpoint_revert(outer).unwrap();
    assert_eq!(provider.sload(FROM, SLOT).unwrap(), U256::ZERO);
    assert_eq!(
        provider.account_info(FROM).unwrap().balance,
        U256::from(100)
    );
    assert_eq!(
        provider.account_info(FROM).unwrap().code,
        Some(original_code)
    );
    assert_eq!(provider.account_info(TO).unwrap().balance, U256::ZERO);
    assert!(provider.take_events().is_empty());
    provider.flush().unwrap();
    assert!(provider.take_committed_changes().is_empty());
}

#[test]
fn flush_preserves_account_code_nonce_and_exposes_complete_change_set() {
    let code = Bytecode::new_raw(Bytes::from_static(&[0x60, 0x2a, 0x00]));
    let expected_hash = code.hash_slow();
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&FROM, account(U256::from(50), 9, code));

    let mut provider = DirectStorageProvider::new(&mut db, block_context());
    provider.sstore(FROM, SLOT, U256::from(42)).unwrap();
    provider.increase_balance(FROM, U256::from(7)).unwrap();
    provider.flush().unwrap();

    let changes = provider.take_committed_changes();
    let changed = changes
        .accounts()
        .find(|(addr, _)| *addr == FROM)
        .and_then(|(_, delta)| delta.current.as_ref())
        .expect("flushed account change");
    assert_eq!(changed.balance, U256::from(57));
    assert_eq!(changed.nonce, 9);
    assert_eq!(changed.code_hash, expected_hash);
    let slot_delta = changes
        .storage()
        .find(|(key, _)| *key == StorageKey::new(FROM, SLOT))
        .map(|(_, delta)| delta)
        .expect("flushed storage slot change");
    assert_eq!(slot_delta.current, U256::from(42));
    assert!(provider.take_committed_changes().is_empty());
    drop(provider);

    let persisted = db.get_account(&FROM).unwrap().expect("persisted account");
    assert_eq!(persisted.balance, U256::from(57));
    assert_eq!(persisted.nonce, 9);
    assert_eq!(persisted.code_hash, expected_hash);
    assert_eq!(db.get_storage(&FROM, &SLOT).unwrap(), U256::from(42));
}

#[test]
fn transfer_underflow_and_destination_overflow_revert_without_partial_mutation() {
    let mut underflow_db = CacheDB::new(EmptyDB::default());
    underflow_db.insert_account_info(&FROM, account(U256::from(5), 0, Bytecode::default()));
    underflow_db.insert_account_info(&TO, AccountInfo::default());
    let mut underflow = DirectStorageProvider::new(&mut underflow_db, block_context());
    let error = underflow
        .transfer_balance(FROM, TO, U256::from(6))
        .unwrap_err();
    assert!(matches!(error, PrecompileError::Revert(_)));
    assert_eq!(underflow.account_info(FROM).unwrap().balance, U256::from(5));
    assert_eq!(underflow.account_info(TO).unwrap().balance, U256::ZERO);

    let mut overflow_db = CacheDB::new(EmptyDB::default());
    overflow_db.insert_account_info(&FROM, account(U256::ONE, 0, Bytecode::default()));
    overflow_db.insert_account_info(&TO, account(U256::MAX, 0, Bytecode::default()));
    let mut overflow = DirectStorageProvider::new(&mut overflow_db, block_context());
    let error = overflow.transfer_balance(FROM, TO, U256::ONE).unwrap_err();
    assert!(matches!(error, PrecompileError::Revert(_)));
    assert_eq!(overflow.account_info(FROM).unwrap().balance, U256::ONE);
    assert_eq!(overflow.account_info(TO).unwrap().balance, U256::MAX);
}

#[test]
fn increase_balance_overflow_reverts_without_partial_mutation() {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&FROM, account(U256::MAX, 0, Bytecode::default()));
    let mut provider = DirectStorageProvider::new(&mut db, block_context());

    let error = provider.increase_balance(FROM, U256::ONE).unwrap_err();
    assert!(matches!(error, PrecompileError::Revert(_)));
    assert_eq!(provider.account_info(FROM).unwrap().balance, U256::MAX);
    provider.flush().unwrap();
    assert!(provider.take_committed_changes().is_empty());
}

#[test]
fn block_state_accumulator_delta_semantics_across_flushes() {
    const ABSENT: Address = Address::new([0x33; 20]);
    const SLOT_A: U256 = U256::from_limbs([10, 0, 0, 0]);
    const SLOT_B: U256 = U256::from_limbs([11, 0, 0, 0]);
    const SLOT_C: U256 = U256::from_limbs([12, 0, 0, 0]);

    let marker_code = Bytecode::new_raw(Bytes::from_static(&[0xef]));
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(&FROM, account(U256::from(50), 9, marker_code));
    db.insert_account_storage(&FROM, &SLOT_A, &U256::from(10));
    db.insert_account_storage(&FROM, &SLOT_B, &U256::from(100));
    db.insert_account_storage(&FROM, &SLOT_C, &U256::from(77));

    let mut provider = DirectStorageProvider::new(&mut db, block_context());

    // (a) Storage-only write on an existing account produces no account-info delta.
    provider.sstore(FROM, SLOT_A, U256::from(20)).unwrap();
    provider.flush().unwrap();
    let changes_a = provider.take_committed_changes();
    assert_eq!(changes_a.accounts().count(), 0);
    let slot_a = changes_a
        .storage()
        .find(|(key, _)| *key == StorageKey::new(FROM, SLOT_A))
        .map(|(_, delta)| *delta)
        .expect("storage-only delta recorded");
    assert_eq!(slot_a.original, U256::from(10));
    assert_eq!(slot_a.current, U256::from(20));

    // (b) Two consecutive flushes to one slot preserve the first original and latest current.
    provider.sstore(FROM, SLOT_B, U256::from(200)).unwrap();
    provider.flush().unwrap();
    provider.sstore(FROM, SLOT_B, U256::from(300)).unwrap();
    provider.flush().unwrap();
    let changes_b = provider.take_committed_changes();
    assert_eq!(changes_b.accounts().count(), 0);
    let slot_b = changes_b
        .storage()
        .find(|(key, _)| *key == StorageKey::new(FROM, SLOT_B))
        .map(|(_, delta)| *delta)
        .expect("multi-flush delta recorded");
    assert_eq!(slot_b.original, U256::from(100));
    assert_eq!(slot_b.current, U256::from(300));

    // (c) Writing a slot back to its original value across flushes cancels the delta.
    provider.sstore(FROM, SLOT_C, U256::from(88)).unwrap();
    provider.flush().unwrap();
    provider.sstore(FROM, SLOT_C, U256::from(77)).unwrap();
    provider.flush().unwrap();
    let changes_c = provider.take_committed_changes();
    assert!(changes_c.is_empty());

    // (d) Writing storage on a non-existent account creates a default account delta alongside
    // the storage delta (production module addresses are pre-seeded in genesis with 0xef).
    provider.sstore(ABSENT, SLOT_A, U256::from(5)).unwrap();
    provider.flush().unwrap();
    let changes_d = provider.take_committed_changes();
    let absent_account = changes_d
        .accounts()
        .find(|(addr, _)| *addr == ABSENT)
        .map(|(_, delta)| delta)
        .expect("absent account created by storage flush");
    assert!(absent_account.original.is_none());
    assert_eq!(
        absent_account.current.as_ref(),
        Some(&AccountInfo::default())
    );
    let absent_slot = changes_d
        .storage()
        .find(|(key, _)| *key == StorageKey::new(ABSENT, SLOT_A))
        .map(|(_, delta)| *delta)
        .expect("absent account storage delta");
    assert_eq!(absent_slot.original, U256::ZERO);
    assert_eq!(absent_slot.current, U256::from(5));
}
