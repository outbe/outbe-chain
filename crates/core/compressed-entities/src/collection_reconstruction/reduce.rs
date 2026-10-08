use super::*;

pub(super) fn sorted_run(
    run: Option<&Path>,
    day: WorldwideDay,
    on_progress: &impl Fn(),
) -> Result<B256, TributePartitionReconstructionError> {
    let mut roots = vec![B256::ZERO; CeDomain::Tribute.shard_count() as usize];
    if let Some(path) = run {
        let mut reader = RunReader::open(path.to_path_buf())?;
        let mut current_shard = None;
        let mut reducer = SortedPoseidonRootReducer::new();
        while let Some(record) = reader.next_record()? {
            let derived_shard = shard_index(record.key, CeDomain::Tribute.shard_count())
                .map_err(|error| TributePartitionReconstructionError::Tree(error.to_string()))?;
            if record.shard != derived_shard {
                return Err(TributePartitionReconstructionError::CorruptRun(
                    path.to_path_buf(),
                ));
            }
            if current_shard.is_some_and(|shard| shard != record.shard) {
                let shard = current_shard
                    .take()
                    .ok_or(TributePartitionReconstructionError::IntegerOverflow)?;
                roots[usize::try_from(shard)
                    .map_err(|_| TributePartitionReconstructionError::IntegerOverflow)?] =
                    B256::from(reducer.finish().map_err(map_tree)?.as_bytes());
                reducer = SortedPoseidonRootReducer::new();
            }
            current_shard = Some(record.shard);
            reducer.push(record.key, record.leaf).map_err(|error| {
                if error == crate::smt::TreeError::DuplicateKey {
                    TributePartitionReconstructionError::Collection(
                        CollectionError::DuplicateTributeKey,
                    )
                } else {
                    map_tree(error)
                }
            })?;
            on_progress();
        }
        if let Some(shard) = current_shard {
            roots[usize::try_from(shard)
                .map_err(|_| TributePartitionReconstructionError::IntegerOverflow)?] =
                B256::from(reducer.finish().map_err(map_tree)?.as_bytes());
        }
        reader.finish()?;
    }
    let shard_top_root = aggregate_b256_shard_roots(&roots)
        .map_err(|error| TributePartitionReconstructionError::Tree(error.to_string()))?;
    let (_, key) = partition_collection_key(PartitionRef::TributeWwd(day))?;
    collection_root(CeDomain::Tribute, key, shard_top_root).map_err(Into::into)
}
