mod rebuild;

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
    CeDomain, CollectionKey, WwdEntityId,
};

fn invalid(message: impl Into<String>) -> CeAuditError {
    CeAuditError::Invalid(message.into())
}

pub(in crate::persistence) fn audit(
    source: &impl DbTx,
    source_path: &Path,
    identity: (&EnvironmentIdentity, ExactParentIdentity),
    work: &CeAuditWork,
    visitor: &mut impl CeAuditVisitor,
) -> Result<CeAuditReport, CeAuditError> {
    let (identity, required) = identity;
    let source_path = source_path.canonicalize()?;
    let scratch_path = work.root.canonicalize()?;
    if scratch_path.starts_with(&source_path) || source_path.starts_with(&scratch_path) {
        return Err(invalid("audit scratch overlaps the source environment"));
    }
    verify_source(source, &source_path, identity, required)?;
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
    // Shard namespaces are contiguous by collection in this immutable snapshot.
    let mut verified_collection = None;
    for row in source.cursor_read::<tables::CeTreeRoots>()?.walk(None)? {
        let (namespace_bytes, root_bytes) = row?;
        let namespace = TreeNamespace::decode(&namespace_bytes)?;
        let expected_root = decode_b256(&root_bytes, "tree root")?;
        validate_root(expected_root)?;
        let shard_count = if let TreeNamespace::CollectionShard(collection, shard) = namespace {
            let domain = match verified_collection {
                Some((key, domain)) if key == collection => domain,
                _ => {
                    let domain = domain_for_collection(collection)?;
                    verify_collection(source, &source_path, collection)?;
                    verified_collection = Some((collection, domain));
                    domain
                }
            };
            if shard >= domain.shard_count() {
                return Err(invalid("shard is outside its collection topology"));
            }
            domain.shard_count()
        } else {
            1
        };
        let config = rebuild::RebuildContext {
            namespace_bytes,
            source_path: &source_path,
            work,
            namespace,
            shard_count,
            expected_root,
            report: &mut report,
        };
        rebuild::rebuild(source, &rebuilt, config, visitor)?;
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
    let domain = domain_for_collection(collection)?;
    let roots = read_collection_roots(source, path, collection, domain.shard_count())?
        .ok_or_else(|| invalid("catalog entry has no materialized collection roots"))?;
    let top = aggregate_b256_shard_roots(&roots).map_err(|error| invalid(error.to_string()))?;
    let expected =
        collection_root(domain, collection, top).map_err(|error| invalid(error.to_string()))?;
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

fn domain_for_collection(key: CollectionKey) -> Result<CeDomain, CeAuditError> {
    // Singleton keys are fixed; all other keys belong to Tribute WWD collections.
    for domain in [
        CeDomain::NodItem,
        CeDomain::NodBucket,
        CeDomain::Gem,
        CeDomain::GemBucket,
    ] {
        if crate::collection_key(domain, WwdEntityId::from([0; 32]))
            .map_err(|error| invalid(error.to_string()))?
            == key
        {
            return Ok(domain);
        }
    }
    Ok(CeDomain::Tribute)
}

fn verify_source(
    source: &impl DbTx,
    source_path: &Path,
    identity: &EnvironmentIdentity,
    required: ExactParentIdentity,
) -> Result<(), CeAuditError> {
    let marker = read_marker(source, source_path)?;
    if marker.height != required.block_number
        || marker.block_hash != required.block_hash
        || marker.new_root != required.root
        || marker.commitment_scheme_version != required.commitment_scheme_version
    {
        return Err(invalid("CE marker differs from required block identity"));
    }
    verify_metadata(source, identity)?;
    let catalog = read_required_tree_root(source, source_path, TreeNamespace::Catalog)?;
    if marker.height == 0
        && (
            marker.block_hash,
            marker.parent_block_hash,
            marker.parent_root,
            catalog,
        ) != (identity.genesis_hash, B256::ZERO, B256::ZERO, B256::ZERO)
    {
        return Err(invalid(
            "genesis must use its native empty catalog and configured block identity",
        ));
    }
    if sealed_root(catalog).map_err(|error| invalid(error.to_string()))? != required.root {
        return Err(invalid("sealed catalog root differs from required root"));
    }
    Ok(())
}

fn verify_metadata(source: &impl DbTx, identity: &EnvironmentIdentity) -> Result<(), CeAuditError> {
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
    Ok(())
}
