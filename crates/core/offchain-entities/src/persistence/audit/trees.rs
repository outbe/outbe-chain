use std::path::Path;

use alloy_primitives::B256;
use reth_db::{
    cursor::DbCursorRO,
    database::Database,
    mdbx::{create_db, DatabaseArguments},
    table::Table,
    transaction::DbTx,
};

use super::{store::ScratchTreeStore, CeAuditError, CeAuditReport, CeAuditVisitor, CeAuditWork};
use crate::{
    collection_root,
    persistence::{
        decode_b256, prefixed_key, read_collection_roots, read_marker, read_required_tree_root,
        tables, validate_root, EnvironmentIdentity, ExactParentIdentity, LeafValue, TreeKey,
        TreeNamespace, IDENTITY_KEY, LAST_APPLIED_KEY,
    },
    sealed_root,
    sharding::{aggregate_b256_shard_roots, shard_index},
    smt::{PoseidonSmt, TreeKey as SmtKey, TreeLeaf, TreeRoot},
    CeDomain, CollectionKey, K_PROVISIONAL,
};

fn invalid(message: impl Into<String>) -> CeAuditError {
    CeAuditError::Invalid(message.into())
}

pub(in crate::persistence) fn audit(
    source: &impl DbTx,
    source_path: &Path,
    identity: &EnvironmentIdentity,
    required: ExactParentIdentity,
    work: &CeAuditWork,
    visitor: &mut impl CeAuditVisitor,
) -> Result<CeAuditReport, CeAuditError> {
    let source_path = source_path.canonicalize()?;
    let scratch_path = work.root.canonicalize()?;
    if scratch_path.starts_with(&source_path) || source_path.starts_with(&scratch_path) {
        return Err(invalid("audit scratch overlaps the source environment"));
    }
    let marker = read_marker(source, &source_path)?;
    if marker.height != required.block_number
        || marker.block_hash != required.block_hash
        || marker.new_root != required.root
        || marker.commitment_scheme_version != required.commitment_scheme_version
    {
        return Err(invalid("CE marker differs from required block identity"));
    }
    for row in source.cursor_read::<tables::CeMetadata>()?.walk(None)? {
        let (key, bytes) = row?;
        if key == IDENTITY_KEY {
            if EnvironmentIdentity::decode(&bytes)? != *identity {
                return Err(invalid("CE environment identity changed"));
            }
        } else if key != LAST_APPLIED_KEY {
            return Err(invalid("unknown CE metadata record"));
        }
    }
    let catalog = read_required_tree_root(source, &source_path, TreeNamespace::Catalog)?;
    if marker.height == 0
        && (marker.block_hash != identity.genesis_hash
            || marker.parent_block_hash != B256::ZERO
            || marker.parent_root != B256::ZERO
            || catalog != B256::ZERO)
    {
        return Err(invalid(
            "genesis must use its native empty catalog and configured block identity",
        ));
    }
    if sealed_root(catalog).map_err(|error| invalid(error.to_string()))? != required.root {
        return Err(invalid("sealed catalog root differs from required root"));
    }
    let mut database = create_db(scratch_path.join("trees"), DatabaseArguments::default())
        .map_err(|error| invalid(format!("create audit scratch database: {error:#}")))?;
    database.create_and_track_tables_for::<tables::CeTables>()?;
    let rebuilt = database.tx_mut()?;
    let mut report = CeAuditReport {
        sealed_root: required.root,
        trees: 0,
        leaves: 0,
        peak_buffered_leaves: 0,
    };
    for row in source.cursor_read::<tables::CeTreeRoots>()?.walk(None)? {
        let (namespace_bytes, root_bytes) = row?;
        let namespace = TreeNamespace::decode(&namespace_bytes)?;
        let expected_root = decode_b256(&root_bytes, "tree root")?;
        validate_root(expected_root)?;
        if let TreeNamespace::CollectionShard(collection, _) = namespace {
            verify_collection(source, &source_path, collection)?;
        }
        let store = ScratchTreeStore::new(&rebuilt, namespace);
        let mut tree = PoseidonSmt::open_with_store(TreeRoot::EMPTY, store);
        let mut buffer = Vec::new();
        let mut cursor = source.cursor_read::<tables::CeLeaves>()?;
        let mut row = cursor.seek(namespace_bytes.clone())?;
        while let Some((key, value)) = row {
            if !key.starts_with(&namespace_bytes) {
                break;
            }
            let key = TreeKey::decode(&key[namespace_bytes.len()..])?;
            let value = LeafValue::decode(&value)?;
            let smt_key =
                SmtKey::from_be_bytes(key.encode()).map_err(|error| invalid(error.to_string()))?;
            if let TreeNamespace::CollectionShard(_, shard) = namespace {
                if shard_index(smt_key, K_PROVISIONAL)
                    .map_err(|error| invalid(error.to_string()))?
                    != shard
                {
                    return Err(invalid("leaf is stored in the wrong native shard"));
                }
            }
            if namespace == TreeNamespace::Catalog {
                verify_collection(
                    source,
                    &source_path,
                    CollectionKey::try_from(key.into_inner())
                        .map_err(|error| invalid(error.to_string()))?,
                )?;
            }
            if buffer.capacity() == 0 {
                buffer
                    .try_reserve_exact(work.limits.records_per_run)
                    .map_err(|_| invalid("cannot reserve tree leaf buffer"))?;
            }
            buffer.push((
                smt_key,
                TreeLeaf::from_be_bytes(value.encode())
                    .map_err(|error| invalid(error.to_string()))?,
            ));
            report.peak_buffered_leaves = report.peak_buffered_leaves.max(buffer.len());
            if buffer.len() == work.limits.records_per_run {
                tree.update_all(std::mem::take(&mut buffer))
                    .map_err(|error| invalid(error.to_string()))?;
            }
            visitor.visit_leaf(namespace, key, value)?;
            report.leaves = report
                .leaves
                .checked_add(1)
                .ok_or_else(|| invalid("leaf count overflow"))?;
            row = cursor.next()?;
        }
        tree.update_all(buffer)
            .map_err(|error| invalid(error.to_string()))?;
        if B256::from(
            tree.root()
                .map_err(|error| invalid(error.to_string()))?
                .as_bytes(),
        ) != expected_root
        {
            return Err(invalid(format!("rebuilt root differs for {namespace:?}")));
        }
        report.trees = report
            .trees
            .checked_add(1)
            .ok_or_else(|| invalid("tree count overflow"))?;
    }
    // Compare both directions, including records unreachable from any root.
    compare_table::<tables::CeLeaves>(source, &rebuilt)?;
    compare_table::<tables::CeBranches>(source, &rebuilt)?;
    Ok(report)
}

fn verify_collection(
    source: &impl DbTx,
    path: &Path,
    collection: CollectionKey,
) -> Result<(), CeAuditError> {
    let roots = read_collection_roots(source, path, collection, K_PROVISIONAL)?
        .ok_or_else(|| invalid("catalog entry has no materialized collection roots"))?;
    let top = aggregate_b256_shard_roots(&roots).map_err(|error| invalid(error.to_string()))?;
    // Every native v1 domain has the same K. This helper commits the collection
    // key and K, not the domain ID; the hashed key is never inverted into a domain.
    let expected = collection_root(CeDomain::Tribute, collection, top)
        .map_err(|error| invalid(error.to_string()))?;
    let key = prefixed_key(TreeNamespace::Catalog, collection.as_bytes());
    let actual = source
        .get::<tables::CeLeaves>(key)?
        .ok_or_else(|| invalid("collection roots lack a catalog leaf"))?;
    if LeafValue::decode(&actual)?.into_inner() != expected {
        return Err(invalid("collection root differs from catalog leaf"));
    }
    Ok(())
}

fn compare_table<T: Table<Key = Vec<u8>, Value = Vec<u8>>>(
    source: &impl DbTx,
    rebuilt: &impl DbTx,
) -> Result<(), CeAuditError> {
    let mut left = source.cursor_read::<T>()?;
    let mut right = rebuilt.cursor_read::<T>()?;
    let mut a = left.first()?;
    let mut b = right.first()?;
    loop {
        if a != b {
            return Err(invalid(format!(
                "persisted {} population differs from reconstruction",
                T::NAME
            )));
        }
        if a.is_none() {
            return Ok(());
        }
        a = left.next()?;
        b = right.next()?;
    }
}
