use super::*;

pub(super) fn catalog_entry(
    view: &AuthenticatedCatalogView,
    collection: crate::CollectionKey,
) -> Result<(TreeLeaf, CkbCompiledProofV1), PointReadServiceError> {
    let catalog_key = TreeKey::from_be_bytes(*collection.as_bytes())
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let catalog_root = TreeRoot::from_be_bytes(view.catalog_root().0)
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let catalog = PoseidonSmt::open_with_store(
        catalog_root,
        StagingCkbStore::new(view.clone(), TreeNamespace::Catalog, view.catalog_root()),
    );
    let catalog_leaf = catalog
        .get(catalog_key)
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let catalog_proof = catalog
        .prove(vec![catalog_key])
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    catalog
        .verify(
            catalog_root,
            &catalog_proof,
            vec![(catalog_key, catalog_leaf)],
        )
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let root_catalog_proof = CkbCompiledProofV1::from_tree(&catalog_proof)?;
    Ok((catalog_leaf, root_catalog_proof))
}

pub(super) fn collection_roots(
    view: &AuthenticatedCatalogView,
    collection: crate::CollectionKey,
    domain: CeDomain,
    catalog_leaf: TreeLeaf,
) -> Result<Vec<B256>, PointReadServiceError> {
    let count = view
        .collection_root_count(collection)
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    if count != domain.shard_count() as usize {
        return Err(PointReadServiceError::Materialization(
            "incomplete collection shard-root vector".into(),
        ));
    }
    let mut roots = Vec::with_capacity(count);
    for shard in 0..domain.shard_count() {
        roots.push(
            view.tree_root(TreeNamespace::CollectionShard(collection, shard))
                .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?
                .ok_or_else(|| {
                    PointReadServiceError::Materialization("missing shard root".into())
                })?,
        );
    }
    let top = aggregate_b256_shard_roots(&roots)
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let expected_collection = collection_root(domain, collection, top)
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    if expected_collection != B256::from(catalog_leaf.as_bytes()) {
        return Err(PointReadServiceError::Materialization(
            "catalog leaf does not match shard roots".into(),
        ));
    }
    Ok(roots)
}
