use super::{
    decode_b256, tables, validate_root, CeMdbx, LeafValue, PersistenceError, TreeKey, TreeNamespace,
};
use crate::CollectionKey;
use alloy_primitives::B256;
use reth_db::cursor::DbCursorRO;
use reth_db::table::Table;
use reth_db::transaction::DbTx;
use reth_db::transaction::DbTxMut;
use std::path::Path;

pub(super) fn prefixed_key(namespace: TreeNamespace, key: &[u8]) -> Vec<u8> {
    let namespace = namespace.encode();
    let mut output = Vec::with_capacity(namespace.len() + key.len());
    output.extend_from_slice(&namespace);
    output.extend_from_slice(key);
    output
}

pub(super) fn read_tree_leaf<T: DbTx>(
    tx: &T,
    path: &Path,
    namespace: TreeNamespace,
    key: TreeKey,
) -> Result<Option<LeafValue>, PersistenceError> {
    tx.get::<tables::CeLeaves>(prefixed_key(namespace, &key.encode()))
        .map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?
        .map(|bytes| LeafValue::decode(&bytes))
        .transpose()
}

pub(super) fn read_tree_root<T: DbTx>(
    tx: &T,
    path: &Path,
    namespace: TreeNamespace,
) -> Result<Option<B256>, PersistenceError> {
    tx.get::<tables::CeTreeRoots>(namespace.encode())
        .map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?
        .map(|bytes| {
            let root = decode_b256(&bytes, "tree root")?;
            validate_root(root)?;
            Ok(root)
        })
        .transpose()
}

pub(super) fn read_required_tree_root<T: DbTx>(
    tx: &T,
    path: &Path,
    namespace: TreeNamespace,
) -> Result<B256, PersistenceError> {
    read_tree_root(tx, path, namespace)?.ok_or(PersistenceError::MissingTreeRoot { namespace })
}

pub(super) fn read_collection_roots<T: DbTx>(
    tx: &T,
    path: &Path,
    collection: CollectionKey,
    shard_count: u32,
) -> Result<Option<Vec<B256>>, PersistenceError> {
    let actual = count_collection_root_records(tx, path, collection)?;
    if actual != 0 && actual != shard_count as usize {
        return Err(PersistenceError::CollectionRootCountMismatch {
            collection,
            expected: shard_count as usize,
            actual,
        });
    }
    let mut roots = Vec::with_capacity(shard_count as usize);
    let mut present = 0_usize;
    for shard in 0..shard_count {
        if let Some(root) =
            read_tree_root(tx, path, TreeNamespace::CollectionShard(collection, shard))?
        {
            present += 1;
            roots.push(root);
        } else {
            roots.push(B256::ZERO);
        }
    }
    if present == 0 && actual == 0 {
        Ok(None)
    } else if present == shard_count as usize && actual == shard_count as usize {
        Ok(Some(roots))
    } else {
        Err(PersistenceError::CollectionRootCountMismatch {
            collection,
            expected: shard_count as usize,
            actual: present,
        })
    }
}

pub(super) fn count_collection_root_records<T: DbTx>(
    tx: &T,
    path: &Path,
    collection: CollectionKey,
) -> Result<usize, PersistenceError> {
    let prefix = collection_prefix(collection);
    let mut cursor =
        tx.cursor_read::<tables::CeTreeRoots>()
            .map_err(|error| PersistenceError::Database {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
    let mut entry = cursor
        .seek(prefix.clone())
        .map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    let mut count = 0_usize;
    while let Some((key, _)) = entry {
        if !key.starts_with(&prefix) {
            break;
        }
        let namespace = TreeNamespace::decode(&key)?;
        if !matches!(namespace, TreeNamespace::CollectionShard(actual, _) if actual == collection) {
            return Err(PersistenceError::NonCanonicalTreeNamespace);
        }
        count = count.saturating_add(1);
        entry = cursor.next().map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    }
    Ok(count)
}

pub(super) fn count_collection_leaf_records<T: DbTx>(
    tx: &T,
    path: &Path,
    collection: CollectionKey,
) -> Result<usize, PersistenceError> {
    const NAMESPACE_BYTES: usize = 1 + 32 + 4;
    const LEAF_KEY_BYTES: usize = NAMESPACE_BYTES + 32;

    let prefix = collection_prefix(collection);
    let mut cursor =
        tx.cursor_read::<tables::CeLeaves>()
            .map_err(|error| PersistenceError::Database {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
    let mut entry = cursor
        .seek(prefix.clone())
        .map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    let mut count = 0_usize;
    while let Some((key, value)) = entry {
        if !key.starts_with(&prefix) {
            break;
        }
        if key.len() != LEAF_KEY_BYTES {
            return Err(PersistenceError::MalformedCollectionLeafKey);
        }
        let namespace = TreeNamespace::decode(&key[..NAMESPACE_BYTES])?;
        if !matches!(
            namespace,
            TreeNamespace::CollectionShard(actual, _) if actual == collection
        ) {
            return Err(PersistenceError::NonCanonicalTreeNamespace);
        }
        TreeKey::decode(&key[NAMESPACE_BYTES..])?;
        LeafValue::decode(&value)?;
        count = count
            .checked_add(1)
            .ok_or(PersistenceError::CollectionLeafCountOverflow)?;
        entry = cursor.next().map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    }
    Ok(count)
}

fn collection_prefix(collection: CollectionKey) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(33);
    prefix.push(1);
    prefix.extend_from_slice(collection.as_bytes());
    prefix
}

pub(super) fn collection_has_records<T: DbTx>(
    tx: &T,
    path: &Path,
    collection: CollectionKey,
) -> Result<bool, PersistenceError> {
    let prefix = collection_prefix(collection);
    let mut roots =
        tx.cursor_read::<tables::CeTreeRoots>()
            .map_err(|error| PersistenceError::Database {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
    if roots
        .seek(prefix.clone())
        .map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?
        .is_some_and(|(key, _)| key.starts_with(&prefix))
    {
        return Ok(true);
    }
    let mut branches =
        tx.cursor_read::<tables::CeBranches>()
            .map_err(|error| PersistenceError::Database {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
    if branches
        .seek(prefix.clone())
        .map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?
        .is_some_and(|(key, _)| key.starts_with(&prefix))
    {
        return Ok(true);
    }
    let mut leaves =
        tx.cursor_read::<tables::CeLeaves>()
            .map_err(|error| PersistenceError::Database {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?;
    Ok(leaves
        .seek(prefix.clone())
        .map_err(|error| PersistenceError::Database {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?
        .is_some_and(|(key, _)| key.starts_with(&prefix)))
}

pub(super) fn delete_collection_records(
    tx: &(impl DbTxMut + DbTx),
    store: &CeMdbx,
    collection: CollectionKey,
) -> Result<(), PersistenceError> {
    let prefix = collection_prefix(collection);

    let root_keys = {
        let mut cursor = tx
            .cursor_read::<tables::CeTreeRoots>()
            .map_err(|error| store.db_error(error))?;
        collect_prefixed_keys::<tables::CeTreeRoots, _>(&mut cursor, &prefix, store)?
    };
    let branch_keys = {
        let mut cursor = tx
            .cursor_read::<tables::CeBranches>()
            .map_err(|error| store.db_error(error))?;
        collect_prefixed_keys::<tables::CeBranches, _>(&mut cursor, &prefix, store)?
    };
    let leaf_keys = {
        let mut cursor = tx
            .cursor_read::<tables::CeLeaves>()
            .map_err(|error| store.db_error(error))?;
        collect_prefixed_keys::<tables::CeLeaves, _>(&mut cursor, &prefix, store)?
    };

    for key in root_keys {
        tx.delete::<tables::CeTreeRoots>(key, None)
            .map_err(|error| store.db_error(error))?;
    }
    for key in branch_keys {
        tx.delete::<tables::CeBranches>(key, None)
            .map_err(|error| store.db_error(error))?;
    }
    for key in leaf_keys {
        tx.delete::<tables::CeLeaves>(key, None)
            .map_err(|error| store.db_error(error))?;
    }
    Ok(())
}

fn collect_prefixed_keys<T, C>(
    cursor: &mut C,
    prefix: &[u8],
    store: &CeMdbx,
) -> Result<Vec<Vec<u8>>, PersistenceError>
where
    T: Table<Key = Vec<u8>, Value = Vec<u8>>,
    C: DbCursorRO<T>,
{
    let mut keys = Vec::new();
    let mut row = cursor
        .seek(prefix.to_vec())
        .map_err(|error| store.db_error(error))?;
    while let Some((key, _)) = row {
        if !key.starts_with(prefix) {
            break;
        }
        keys.push(key);
        row = cursor.next().map_err(|error| store.db_error(error))?;
    }
    Ok(keys)
}
