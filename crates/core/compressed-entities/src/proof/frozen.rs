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

pub(super) fn point_evidence(
    view: &AuthenticatedCatalogView,
    catalog: (crate::CollectionKey, TreeLeaf),
    domain: CeDomain,
    raw_id: WwdEntityId,
    root_catalog_proof: CkbCompiledProofV1,
) -> Result<(TreeLeaf, PresentEvidenceV1), PointReadServiceError> {
    let (collection, catalog_leaf) = catalog;
    let roots = collection_roots(view, collection, domain, catalog_leaf)?;
    let tree_key = derive_tree_key(collection_for_domain(domain), raw_id)
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let selected = shard_index(tree_key, domain.shard_count())
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let namespace = TreeNamespace::CollectionShard(collection, selected);
    let shard_root = TreeRoot::from_be_bytes(roots[selected as usize].0)
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let shard = PoseidonSmt::open_with_store(
        shard_root,
        StagingCkbStore::new(view.clone(), namespace, roots[selected as usize]),
    );
    let leaf = shard
        .get(tree_key)
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let proof = shard
        .prove(vec![tree_key])
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    shard
        .verify(shard_root, &proof, vec![(tree_key, leaf)])
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let evidence = PresentEvidenceV1 {
        shard_smt_proof: CkbCompiledProofV1::from_tree(&proof)?,
        shard_top_siblings: top_siblings(&roots, selected)?,
        root_catalog_proof,
    };
    Ok((leaf, evidence))
}

pub(super) fn finalized_view(
    service: &CompressedTreeService,
) -> Result<(FinalizedMarker, AuthenticatedCatalogView), PointReadServiceError> {
    let snapshot = service
        .open_finalized_snapshot()
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    let marker = snapshot
        .marker()
        .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    if marker.height == 0 {
        return Err(PointReadServiceError::GenesisUnavailable);
    }
    let view = AuthenticatedCatalogView::open(
        snapshot,
        ExactParentIdentity {
            commitment_scheme_version: marker.commitment_scheme_version,
            block_number: marker.height,
            block_hash: marker.block_hash,
            root: marker.new_root,
        },
    )
    .map_err(|e| PointReadServiceError::Materialization(e.to_string()))?;
    Ok((marker, view))
}

pub(super) fn point_result(
    common: PointProofCommonV1,
    leaf: TreeLeaf,
    evidence: PresentEvidenceV1,
) -> FrozenResultV1 {
    if leaf == TreeLeaf::ZERO {
        FrozenResultV1::Absent {
            common,
            evidence: AbsentEvidenceV1::EntityAbsentInCollection {
                shard_smt_proof: evidence.shard_smt_proof,
                shard_top_siblings: evidence.shard_top_siblings,
                root_catalog_proof: evidence.root_catalog_proof,
            },
        }
    } else {
        FrozenResultV1::Present {
            common,
            expected_leaf: B256::from(leaf.as_bytes()),
            evidence,
        }
    }
}
