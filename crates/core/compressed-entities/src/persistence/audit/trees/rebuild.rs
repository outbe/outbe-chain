//! Rebuild native trees with bounded scratch and validate leaf placement.

use super::*;
use reth_db::transaction::DbTxMut;

pub(super) struct RebuildContext<'a> {
    pub(super) source_path: &'a Path,
    pub(super) namespace_bytes: Vec<u8>,
    pub(super) work: &'a CeAuditWork,
    pub(super) namespace: TreeNamespace,
    pub(super) shard_count: u32,
    pub(super) expected_root: B256,
    pub(super) report: &'a mut CeAuditReport,
}
pub(super) fn rebuild(
    source: &impl DbTx,
    rebuilt: &(impl DbTx + DbTxMut),
    config: RebuildContext<'_>,
    visitor: &mut impl CeAuditVisitor,
) -> Result<(), CeAuditError> {
    let RebuildContext {
        source_path,
        namespace_bytes,
        work,
        namespace,
        shard_count,
        expected_root,
        report,
    } = config;
    let store = ScratchTreeStore::new(rebuilt, namespace);
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
        verify_leaf_namespace(source, source_path, namespace, (smt_key, key), shard_count)?;
        if buffer.capacity() == 0 {
            buffer
                .try_reserve_exact(work.limits.records_per_run)
                .map_err(|_| invalid("cannot reserve tree leaf buffer"))?;
        }
        buffer.push((
            smt_key,
            TreeLeaf::from_be_bytes(value.encode()).map_err(|error| invalid(error.to_string()))?,
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
    Ok(())
}

fn verify_leaf_namespace(
    source: &impl DbTx,
    source_path: &Path,
    namespace: TreeNamespace,
    keys: (SmtKey, TreeKey),
    shard_count: u32,
) -> Result<(), CeAuditError> {
    let (smt_key, key) = keys;
    if let TreeNamespace::CollectionShard(_, shard) = namespace {
        if shard_index(smt_key, shard_count).map_err(|error| invalid(error.to_string()))? != shard {
            return Err(invalid("leaf is stored in the wrong native shard"));
        }
    }
    if namespace == TreeNamespace::Catalog {
        let collection = CollectionKey::try_from(key.into_inner())
            .map_err(|error| invalid(error.to_string()))?;
        verify_collection(source, source_path, collection)?;
    }
    Ok(())
}
