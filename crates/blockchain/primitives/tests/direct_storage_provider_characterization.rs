use alloy_primitives::{Address, Bytes, LogData, U256};
use outbe_primitives::{
    block::BlockContext,
    error::PrecompileError,
    storage::{direct::DirectStorageProvider, PrecompileStorageProvider},
};
use revm::{
    database::{CacheDB, EmptyDB},
    state::{AccountInfo, Bytecode},
    Database,
};

const FROM: Address = Address::new([0x11; 20]);
const TO: Address = Address::new([0x22; 20]);
const SLOT: U256 = U256::from_limbs([7, 0, 0, 0]);

fn block_context() -> BlockContext {
    BlockContext::new(7, 1_700_000_000, 54_322_345, FROM, vec![FROM])
}

fn account(balance: U256, nonce: u64, code: Bytecode) -> AccountInfo {
    AccountInfo {
        balance,
        nonce,
        code_hash: code.hash_slow(),
        code: Some(code),
        ..Default::default()
    }
}

#[test]
fn set_code_is_unsupported_and_produces_no_change_set() {
    let mut db = CacheDB::new(EmptyDB::default());
    let mut provider = DirectStorageProvider::new(&mut db, block_context());
    let code = Bytecode::new_raw(Bytes::from_static(&[0x00]));

    let error = provider.set_code(TO, code).unwrap_err();
    assert!(matches!(error, PrecompileError::Unsupported));
    provider.flush().unwrap();
    assert!(provider.take_committed_changes().is_empty());
}

#[test]
fn nested_checkpoint_revert_restores_storage_balance_and_events() {
    let mut db = CacheDB::new(EmptyDB::default());
    db.insert_account_info(
        FROM,
        account(
            U256::from(100),
            3,
            Bytecode::new_raw(Bytes::from_static(&[0x60, 0x00])),
        ),
    );
    db.insert_account_info(TO, AccountInfo::default());
    let mut provider = DirectStorageProvider::new(&mut db, block_context());

    let outer = provider.checkpoint();
    provider.sstore(FROM, SLOT, U256::from(10)).unwrap();
    provider
        .emit_event(
            FROM,
            LogData::new_unchecked(vec![], Bytes::from_static(&[0x01])),
        )
        .unwrap();
    provider.transfer_balance(FROM, TO, U256::from(20)).unwrap();

    let inner = provider.checkpoint();
    provider.sstore(FROM, SLOT, U256::from(99)).unwrap();
    provider
        .emit_event(
            TO,
            LogData::new_unchecked(vec![], Bytes::from_static(&[0x02])),
        )
        .unwrap();
    provider.transfer_balance(FROM, TO, U256::from(30)).unwrap();
    provider.checkpoint_revert(inner);

    assert_eq!(provider.sload(FROM, SLOT).unwrap(), U256::from(10));
    assert_eq!(provider.account_info(FROM).unwrap().balance, U256::from(80));
    assert_eq!(provider.account_info(TO).unwrap().balance, U256::from(20));
    let events = provider.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].address, FROM);
    assert_eq!(events[0].data.data, Bytes::from_static(&[0x01]));

    provider.checkpoint_revert(outer);
    assert_eq!(provider.sload(FROM, SLOT).unwrap(), U256::ZERO);
    assert_eq!(
        provider.account_info(FROM).unwrap().balance,
        U256::from(100)
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
    db.insert_account_info(FROM, account(U256::from(50), 9, code));

    let mut provider = DirectStorageProvider::new(&mut db, block_context());
    provider.sstore(FROM, SLOT, U256::from(42)).unwrap();
    provider.increase_balance(FROM, U256::from(7)).unwrap();
    provider.flush().unwrap();

    let changes = provider.take_committed_changes();
    let changed = changes.get(&FROM).expect("flushed account change");
    assert_eq!(changed.info.balance, U256::from(57));
    assert_eq!(changed.info.nonce, 9);
    assert_eq!(changed.info.code_hash, expected_hash);
    assert_eq!(changed.storage[&SLOT].present_value(), U256::from(42));
    assert!(provider.take_committed_changes().is_empty());
    drop(provider);

    let persisted = db.basic(FROM).unwrap().expect("persisted account");
    assert_eq!(persisted.balance, U256::from(57));
    assert_eq!(persisted.nonce, 9);
    assert_eq!(persisted.code_hash, expected_hash);
    assert_eq!(db.storage(FROM, SLOT).unwrap(), U256::from(42));
}

#[test]
fn transfer_underflow_is_fatal_and_destination_overflow_wraps_today() {
    let mut underflow_db = CacheDB::new(EmptyDB::default());
    underflow_db.insert_account_info(FROM, account(U256::from(5), 0, Bytecode::default()));
    underflow_db.insert_account_info(TO, AccountInfo::default());
    let mut underflow = DirectStorageProvider::new(&mut underflow_db, block_context());
    let error = underflow
        .transfer_balance(FROM, TO, U256::from(6))
        .unwrap_err();
    assert!(matches!(error, PrecompileError::Fatal(_)));
    assert_eq!(underflow.account_info(FROM).unwrap().balance, U256::from(5));
    assert_eq!(underflow.account_info(TO).unwrap().balance, U256::ZERO);

    let mut overflow_db = CacheDB::new(EmptyDB::default());
    overflow_db.insert_account_info(FROM, account(U256::ONE, 0, Bytecode::default()));
    overflow_db.insert_account_info(TO, account(U256::MAX, 0, Bytecode::default()));
    let mut overflow = DirectStorageProvider::new(&mut overflow_db, block_context());
    overflow.transfer_balance(FROM, TO, U256::ONE).unwrap();
    assert_eq!(overflow.account_info(FROM).unwrap().balance, U256::ZERO);
    assert_eq!(
        overflow.account_info(TO).unwrap().balance,
        U256::ZERO,
        "current unchecked destination addition wraps U256::MAX + 1 to zero"
    );
}
