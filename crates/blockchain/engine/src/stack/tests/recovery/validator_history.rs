//! Persist independent ValidatorSet snapshots using real native history tables.
use alloy_primitives::{Address, B256, U256};
use reth_ethereum::provider::db::models::ShardedKey;
use reth_ethereum::provider::db::{
    database::Database,
    init_db,
    mdbx::DatabaseArguments,
    table::Table,
    tables,
    transaction::{DbTx, DbTxMut},
};
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

type StorageWord = <tables::PlainStorageState as Table>::Value;
type HistoryKey = <tables::StoragesHistory as Table>::Key;
type HistoryBlocks = <tables::StoragesHistory as Table>::Value;

pub(super) struct Snapshots<'a> {
    pub before: &'a HashMap<(Address, U256), U256>,
    pub after: &'a HashMap<(Address, U256), U256>,
    pub change_height: u64,
}

pub(super) fn persist(
    root: &Path,
    snapshots: Snapshots<'_>,
    check_changes: impl FnOnce(&BTreeSet<Address>),
) -> eyre::Result<BTreeSet<Address>> {
    let keys: BTreeSet<_> = snapshots
        .before
        .keys()
        .chain(snapshots.after.keys())
        .copied()
        .collect();
    let addresses: BTreeSet<_> = keys.iter().map(|(address, _)| *address).collect();
    let mut changed_addresses = BTreeSet::new();
    let db = init_db(root.join("db"), DatabaseArguments::test())?;
    let tx = db.tx_mut()?;
    for address in addresses {
        tx.put::<tables::PlainAccountState>(address, Default::default())?;
    }
    for (address, slot) in keys {
        let before = snapshots
            .before
            .get(&(address, slot))
            .copied()
            .unwrap_or_default();
        let after = snapshots
            .after
            .get(&(address, slot))
            .copied()
            .unwrap_or_default();
        if persist_slot(
            &tx,
            (address, slot),
            (before, after),
            snapshots.change_height,
        )? {
            changed_addresses.insert(address);
        }
    }
    check_changes(&changed_addresses);
    tx.commit()?;
    Ok(changed_addresses)
}

fn persist_slot(
    tx: &impl DbTxMut,
    position: (Address, U256),
    values: (U256, U256),
    change_height: u64,
) -> eyre::Result<bool> {
    let (address, slot) = position;
    let (before, after) = values;
    let key = B256::from(slot.to_be_bytes::<32>());
    if !after.is_zero() {
        tx.put::<tables::PlainStorageState>(address, StorageWord { key, value: after })?;
    }
    let mut writes = Vec::new();
    if !before.is_zero() {
        writes.push(0);
        tx.put::<tables::StorageChangeSets>(
            (0, address).into(),
            StorageWord {
                key,
                value: U256::ZERO,
            },
        )?;
    }
    if before != after {
        writes.push(change_height);
        tx.put::<tables::StorageChangeSets>(
            (change_height, address).into(),
            StorageWord { key, value: before },
        )?;
    }
    if !writes.is_empty() {
        tx.put::<tables::StoragesHistory>(
            HistoryKey {
                address,
                sharded_key: ShardedKey {
                    key,
                    highest_block_number: u64::MAX,
                },
            },
            HistoryBlocks::new(writes)?,
        )?;
    }
    Ok(before != after)
}
