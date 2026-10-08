use super::*;

#[derive(Debug)]
struct TributePartitionTree {
    parent_root: B256,
}

impl AuthenticatedParentTree for TributePartitionTree {
    fn parent_block_hash(&self) -> B256 {
        hash(90)
    }

    fn parent_root(&self) -> B256 {
        self.parent_root
    }

    fn read_leaf_verified(
        &self,
        _entity: EntityRef,
        _expected_parent_root: B256,
    ) -> PrecompileResult<Option<outbe_compressed_entities::Commitment>> {
        Ok(None)
    }

    fn partition_present_verified(
        &self,
        partition: PartitionRef,
        expected_parent_root: B256,
    ) -> PrecompileResult<bool> {
        if expected_parent_root != self.parent_root
            || partition != PartitionRef::TributeWwd(TEST_WWD)
        {
            return Err(PrecompileError::Fatal(
                "activation fixture parent partition binding mismatch".into(),
            ));
        }
        Ok(true)
    }

    fn partition_root_verified(
        &self,
        partition: PartitionRef,
        expected_parent_root: B256,
    ) -> PrecompileResult<Option<B256>> {
        if expected_parent_root != self.parent_root
            || partition != PartitionRef::TributeWwd(TEST_WWD)
        {
            return Err(PrecompileError::Fatal(
                "activation fixture parent partition binding mismatch".into(),
            ));
        }
        Ok(Some(hash(31)))
    }

    fn prepare_seal(
        &self,
        _block_number: u64,
        _mutations: &[FinalLeafMutation],
        _retirements: &[PartitionRef],
    ) -> PrecompileResult<ProvisionalTreeBatch> {
        Err(PrecompileError::Fatal(
            "activation fixture does not seal its synthetic parent".into(),
        ))
    }
}

pub(super) fn begin_activation_scope(provider: &mut HashMapStorageProvider) -> ExecutionScope {
    let parent_root = hash(80);
    let scope = outbe_compressed_entities::execution_scope::with_parent_tree(
        Arc::new(TributePartitionTree { parent_root }),
        CeWorkConfig::new(0, 0, u64::MAX),
    );
    StorageHandle::enter(provider, |storage| {
        storage
            .sstore(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO, U256::from(4))
            .unwrap();
        storage
            .sstore(
                COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(parent_root.as_slice()),
            )
            .unwrap();
        begin_block(storage, &scope).unwrap();
    });
    scope
}
