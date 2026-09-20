//! Independent current-state calculation in disposable scratch, never source repair.

use std::path::Path;

use alloy_consensus::Sealable;
use alloy_primitives::{keccak256, B256};
use eyre::ensure;
use outbe_primitives::OutbeHeader;
use reth_ethereum::{
    provider::db::{
        cursor::{DbCursorRO, DbDupCursorRO},
        database::Database,
        init_db,
        mdbx::DatabaseArguments,
        table::Value,
        tables::{self, RawDupSort, RawKey, RawTable, RawValue},
        transaction::{DbTx, DbTxMut},
        DatabaseEnv,
    },
    trie::{
        DatabaseHashedCursorFactory, DatabaseStateRoot, DatabaseTrieCursorFactory,
        LegacyKeyAdapter, StateRoot,
    },
};
use reth_primitives_traits::StorageEntry;

use super::super::native::RethReadOnlyView;
use super::Incomplete;

pub(crate) struct VerifiedState {
    pub db: DatabaseEnv,
    pub header: OutbeHeader,
    pub state_root: B256,
    _directory: tempfile::TempDir,
}

pub(crate) fn verify_current_evm(
    view: &RethReadOnlyView,
    scratch_parent: &Path,
) -> eyre::Result<VerifiedState> {
    let progress = &view.progress;
    if progress.execution_stage != Some(progress.execution.number)
        || progress.finish_stage != progress.execution_stage
        || progress.partial_state_trie.is_some()
        || progress.unwind.is_some()
    {
        return Err(Incomplete(
            "execution/Finish/partial/unwind observations do not describe complete current state"
                .into(),
        )
        .into());
    }
    let header = view.header(progress.execution.number)?.ok_or_else(|| {
        Incomplete(format!(
            "missing execution header {}",
            progress.execution.number
        ))
    })?;
    ensure!(
        header.inner.number == progress.execution.number
            && view.canonical_hash(progress.execution.number)? == Some(header.hash_slow())
            && hex::encode(header.hash_slow()) == progress.execution.hash,
        "execution header does not match the observed canonical identity"
    );
    outbe_snapshot::layout::validate_layout(&[], &view.protected, &[scratch_parent.to_path_buf()])?;
    let directory = tempfile::Builder::new()
        .prefix("outbe-evm-audit-")
        .tempdir_in(scratch_parent)?;
    let db = init_db(directory.path().join("db"), DatabaseArguments::default())?;
    let source = view.read_transaction()?;
    let scratch = db.tx_mut()?;
    copy_authoritative_state_to_scratch(&source, &scratch, progress.storage_version)?;
    verify_bytecodes(&source, &scratch)?;
    let state_root = recompute_state_root(&scratch)?;
    ensure!(
        state_root == header.inner.state_root,
        "current EVM state root differs at E={}: computed {state_root}, header {}",
        header.inner.number,
        header.inner.state_root
    );
    scratch.commit()?;
    drop(source);
    Ok(VerifiedState {
        db,
        header,
        state_root,
        _directory: directory,
    })
}

fn copy_authoritative_state_to_scratch(
    source: &impl DbTx,
    scratch: &(impl DbTx + DbTxMut),
    version: u32,
) -> eyre::Result<()> {
    match version {
        1 => {
            for row in source
                .cursor_read::<RawTable<tables::PlainAccountState>>()?
                .walk(None)?
            {
                let (address, account) = row?;
                scratch.put::<tables::HashedAccounts>(
                    keccak256(address.key()?),
                    read_canonical_native_value(account, "PlainAccountState")?,
                )?;
            }
            for row in source
                .cursor_dup_read::<RawDupSort<tables::PlainStorageState>>()?
                .walk(None)?
            {
                let (address, entry) = row?;
                let entry = read_canonical_native_value(entry, "PlainStorageState")?;
                copy_storage(
                    scratch,
                    keccak256(address.key()?),
                    StorageEntry {
                        key: keccak256(entry.key),
                        value: entry.value,
                    },
                )?;
            }
        }
        2 => {
            for row in source
                .cursor_read::<RawTable<tables::HashedAccounts>>()?
                .walk(None)?
            {
                let (address, account) = row?;
                scratch.put::<tables::HashedAccounts>(
                    address.key()?,
                    read_canonical_native_value(account, "HashedAccounts")?,
                )?;
            }
            for row in source
                .cursor_dup_read::<RawDupSort<tables::HashedStorages>>()?
                .walk(None)?
            {
                let (address, entry) = row?;
                copy_storage(
                    scratch,
                    address.key()?,
                    read_canonical_native_value(entry, "HashedStorages")?,
                )?;
            }
        }
        _ => eyre::bail!("unsupported state storage version {version}"),
    }
    Ok(())
}

// Reth's Compact decoders may panic on truncated native bytes. Isolate only the
// immutable value decode, so a malformed snapshot produces an audit error while
// the source stays untouched and the caller still drops its disposable scratch.
fn read_native_value<V: Value>(value: &RawValue<V>, table: &str) -> eyre::Result<V> {
    Ok(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| value.value()))
            .map_err(|_| eyre::eyre!("malformed native value in {table}"))??,
    )
}

// Account and storage entries have one native compact encoding. The upstream
// Decompress facade discards the decoder remainder, so check the entire row.
// Bytecode analysis encodings may normalize and are checked by code hash instead.
fn read_canonical_native_value<V: Value + Clone>(
    value: RawValue<V>,
    table: &str,
) -> eyre::Result<V> {
    let decoded = read_native_value(&value, table)?;
    ensure!(
        RawValue::new(decoded.clone()).raw_value() == value.raw_value(),
        "malformed native value in {table}"
    );
    Ok(decoded)
}

fn copy_storage(
    scratch: &(impl DbTx + DbTxMut),
    address: B256,
    entry: StorageEntry,
) -> eyre::Result<()> {
    ensure!(
        scratch.get::<tables::HashedAccounts>(address)?.is_some(),
        "orphan storage for account {address}"
    );
    ensure!(
        !entry.value.is_zero(),
        "noncanonical zero storage at {address}/{}",
        entry.key
    );
    let existing = scratch
        .cursor_dup_read::<tables::HashedStorages>()?
        .seek_by_key_subkey(address, entry.key)?;
    ensure!(
        existing.is_none_or(|value| value.key != entry.key),
        "duplicate storage at {address}/{}",
        entry.key
    );
    scratch.put::<tables::HashedStorages>(address, entry)?;
    Ok(())
}

fn verify_bytecodes(source: &impl DbTx, scratch: &impl DbTx) -> eyre::Result<()> {
    for row in source
        .cursor_read::<RawTable<tables::Bytecodes>>()?
        .walk(None)?
    {
        let (hash, bytecode) = row?;
        let hash = hash.key()?;
        let bytecode = read_native_value(&bytecode, "Bytecodes")?;
        ensure!(
            keccak256(bytecode.original_bytes()) == hash,
            "bytecode differs from its hash {hash}"
        );
    }
    for row in scratch
        .cursor_read::<tables::HashedAccounts>()?
        .walk(None)?
    {
        let (address, account) = row?;
        if let Some(hash) = account.bytecode_hash.filter(|hash| *hash != keccak256([])) {
            ensure!(
                source
                    .get::<RawTable<tables::Bytecodes>>(RawKey::new(hash))?
                    .is_some(),
                "missing bytecode {hash} for account {address}"
            );
        }
    }
    Ok(())
}

fn recompute_state_root(scratch: &impl DbTx) -> eyre::Result<B256> {
    Ok(StateRoot::<DatabaseTrieCursorFactory<_, LegacyKeyAdapter>, DatabaseHashedCursorFactory<_>>::from_tx(scratch).root()?)
}
