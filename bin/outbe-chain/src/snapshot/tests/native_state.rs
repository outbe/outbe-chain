//! Native current state and before-values for historical snapshot fixtures.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::{
    storage::hashmap::HashMapStorageProvider,
    tribute_encryption::{
        EncryptedTributeV2, TributeContextV2, TRIBUTE_AMOUNTS_BLOB_LEN, TRIBUTE_PUBLIC_KEY_BLOB_LEN,
    },
};
use reth_ethereum::provider::db::{
    cursor::{DbCursorRO, DbDupCursorRO},
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    models::{storage_sharded_key::StorageShardedKey, AccountBeforeTx, ShardedKey},
    table::Table,
    tables,
    transaction::{DbTx, DbTxMut},
};
use reth_primitives_traits::{Account, StorageEntry};
use reth_provider::{providers::RocksDBProvider, StorageSettings};

use crate::snapshot::config::NativeLayout;

pub(super) type State = BTreeMap<Address, (Account, BTreeMap<B256, U256>)>;
type AccountBlocks = <tables::AccountsHistory as Table>::Value;
type StorageBlocks = <tables::StoragesHistory as Table>::Value;

/// These carriers test native storage proofs. Crypto suites test decryption.
pub(super) fn encrypted(context: TributeContextV2) -> EncryptedTributeV2 {
    let blob = |length| {
        let mut bytes = vec![0x51; length];
        bytes[..8].copy_from_slice(&1u64.to_be_bytes());
        bytes
    };
    EncryptedTributeV2 {
        context,
        encrypted_creator_public_key: blob(TRIBUTE_PUBLIC_KEY_BLOB_LEN),
        encrypted_amounts: blob(TRIBUTE_AMOUNTS_BLOB_LEN),
    }
}

pub(super) fn from_owner(owner: &HashMapStorageProvider) -> State {
    let mut state = State::new();
    merge_owner(&mut state, owner);
    state
}

pub(super) fn merge_owner(state: &mut State, owner: &HashMapStorageProvider) {
    for (&(address, slot), &value) in &owner.storage {
        let words = &mut state.entry(address).or_default().1;
        let key = B256::from(slot.to_be_bytes::<32>());
        if value.is_zero() {
            words.remove(&key);
        } else {
            words.insert(key, value);
        }
    }
}

pub(super) fn root(state: &State) -> B256 {
    use reth_ethereum::trie::root::{state_root_unhashed, storage_root_unhashed};
    state_root_unhashed(state.iter().map(|(address, (account, words))| {
        (
            *address,
            account.into_trie_account(storage_root_unhashed(
                words.iter().map(|(key, value)| (*key, *value)),
            )),
        )
    }))
}

pub(super) fn read_plain(tx: &impl DbTx) -> State {
    let mut state = State::new();
    for row in tx
        .cursor_read::<tables::PlainAccountState>()
        .unwrap()
        .walk(None)
        .unwrap()
    {
        let (address, account) = row.unwrap();
        let mut words = BTreeMap::new();
        for row in tx
            .cursor_dup_read::<tables::PlainStorageState>()
            .unwrap()
            .walk_dup(Some(address), None)
            .unwrap()
        {
            let (_, entry) = row.unwrap();
            words.insert(entry.key, entry.value);
        }
        state.insert(address, (account, words));
    }
    state
}

/// Store the current population and actual before-values for its transition.
/// Version 2 uses RocksDB history indexes and MDBX changesets, as Reth does.
pub(super) fn persist(
    layout: &NativeLayout,
    version: u32,
    before: &State,
    after: &State,
    change_height: u64,
) {
    let rocks = RocksDBProvider::builder(&layout.execution_rocksdb_root)
        .with_default_tables()
        .build()
        .unwrap();
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let tx = db.tx_mut().unwrap();
    let settings = StorageSettings {
        storage_v2: version == 2,
        ..StorageSettings::v1()
    };
    tx.put::<tables::Metadata>(
        "storage_settings".into(),
        serde_json::to_vec(&settings).unwrap(),
    )
    .unwrap();
    tx.clear::<tables::PlainAccountState>().unwrap();
    tx.clear::<tables::PlainStorageState>().unwrap();
    tx.clear::<tables::HashedAccounts>().unwrap();
    tx.clear::<tables::HashedStorages>().unwrap();
    tx.clear::<tables::AccountsTrie>().unwrap();
    tx.clear::<tables::StoragesTrie>().unwrap();
    tx.clear::<tables::PackedAccountsTrie>().unwrap();
    tx.clear::<tables::PackedStoragesTrie>().unwrap();
    tx.clear::<tables::AccountsHistory>().unwrap();
    tx.clear::<tables::StoragesHistory>().unwrap();
    tx.clear::<tables::AccountChangeSets>().unwrap();
    tx.clear::<tables::StorageChangeSets>().unwrap();
    let addresses = before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for address in addresses {
        let previous = before.get(&address);
        let current = after.get(&address);
        if let Some((account, words)) = current {
            tx.put::<tables::PlainAccountState>(address, *account)
                .unwrap();
            tx.put::<tables::HashedAccounts>(keccak256(address), *account)
                .unwrap();
            for (&key, &value) in words {
                assert!(!value.is_zero());
                tx.put::<tables::PlainStorageState>(address, StorageEntry { key, value })
                    .unwrap();
                tx.put::<tables::HashedStorages>(
                    keccak256(address),
                    StorageEntry {
                        key: keccak256(key),
                        value,
                    },
                )
                .unwrap();
            }
        }
        let mut account_writes = Vec::new();
        if previous.is_some() {
            account_writes.push(0);
            tx.put::<tables::AccountChangeSets>(
                0,
                AccountBeforeTx {
                    address,
                    info: None,
                },
            )
            .unwrap();
        }
        if previous.map(|entry| entry.0) != current.map(|entry| entry.0) {
            account_writes.push(change_height);
            tx.put::<tables::AccountChangeSets>(
                change_height,
                AccountBeforeTx {
                    address,
                    info: previous.map(|entry| entry.0),
                },
            )
            .unwrap();
        }
        if !account_writes.is_empty() {
            let key = ShardedKey {
                key: address,
                highest_block_number: u64::MAX,
            };
            let value = AccountBlocks::new(account_writes).unwrap();
            if version == 1 {
                tx.put::<tables::AccountsHistory>(key, value).unwrap();
            } else {
                rocks.put::<tables::AccountsHistory>(key, &value).unwrap();
            }
        }
        let slots = previous
            .into_iter()
            .flat_map(|entry| entry.1.keys())
            .chain(current.into_iter().flat_map(|entry| entry.1.keys()))
            .copied()
            .collect::<BTreeSet<_>>();
        for key in slots {
            let old = previous
                .and_then(|entry| entry.1.get(&key))
                .copied()
                .unwrap_or_default();
            let new = current
                .and_then(|entry| entry.1.get(&key))
                .copied()
                .unwrap_or_default();
            let mut writes = Vec::new();
            if !old.is_zero() {
                writes.push(0);
                tx.put::<tables::StorageChangeSets>(
                    (0, address).into(),
                    StorageEntry {
                        key,
                        value: U256::ZERO,
                    },
                )
                .unwrap();
            }
            if old != new {
                writes.push(change_height);
                tx.put::<tables::StorageChangeSets>(
                    (change_height, address).into(),
                    StorageEntry { key, value: old },
                )
                .unwrap();
            }
            if !writes.is_empty() {
                let history_key = StorageShardedKey {
                    address,
                    sharded_key: ShardedKey {
                        key,
                        highest_block_number: u64::MAX,
                    },
                };
                let value = StorageBlocks::new(writes).unwrap();
                if version == 1 {
                    tx.put::<tables::StoragesHistory>(history_key, value)
                        .unwrap();
                } else {
                    rocks
                        .put::<tables::StoragesHistory>(history_key, &value)
                        .unwrap();
                }
            }
        }
    }
    tx.commit().unwrap();
    drop(db);
    materialize_trie(layout, rocks, version, root(after));
}

fn materialize_trie(layout: &NativeLayout, rocks: RocksDBProvider, version: u32, expected: B256) {
    use reth_ethereum::{
        node::api::NodeTypesWithDBAdapter,
        provider::db::DatabaseEnv,
        tasks::{RayonConfig, RuntimeBuilder, RuntimeConfig, TokioConfig},
        trie::{
            DatabaseHashedCursorFactory, DatabaseStateRoot, DatabaseTrieCursorFactory,
            LegacyKeyAdapter, PackedKeyAdapter, StateRoot,
        },
    };
    use reth_provider::{ProviderFactory, StaticFileProviderBuilder, TrieWriter};
    let runtime = RuntimeBuilder::new(RuntimeConfig {
        tokio: TokioConfig::with_worker_threads(1),
        rayon: RayonConfig {
            cpu_threads: Some(1),
            rpc_threads: Some(1),
            storage_threads: Some(1),
            proof_storage_worker_threads: Some(1),
            proof_account_worker_threads: Some(1),
            prewarming_threads: Some(1),
            bal_streaming_threads: Some(1),
            state_trie_overlay_worker_threads: Some(1),
            ..Default::default()
        },
    })
    .build()
    .unwrap();
    let db = init_db(layout.chain_root.join("db"), DatabaseArguments::test()).unwrap();
    let factory =
        ProviderFactory::<NodeTypesWithDBAdapter<outbe_node::OutbeNode, DatabaseEnv>>::new(
            db,
            layout.chain.clone(),
            StaticFileProviderBuilder::read_write(&layout.static_files_root)
                .build()
                .unwrap(),
            rocks,
            runtime,
        )
        .unwrap();
    let provider = factory.provider_rw().unwrap();
    let (actual, updates) = if version == 1 {
        StateRoot::<DatabaseTrieCursorFactory<_, LegacyKeyAdapter>, DatabaseHashedCursorFactory<_>>::from_tx(provider.tx_ref())
            .root_with_updates().unwrap()
    } else {
        StateRoot::<DatabaseTrieCursorFactory<_, PackedKeyAdapter>, DatabaseHashedCursorFactory<_>>::from_tx(provider.tx_ref())
            .root_with_updates().unwrap()
    };
    assert_eq!(actual, expected);
    provider.write_trie_updates(updates).unwrap();
    provider.commit().unwrap();
}
