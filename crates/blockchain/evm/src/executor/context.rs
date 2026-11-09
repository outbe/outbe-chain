use super::*;

impl<E: Evm> OutbeBlockExecutor<'_, E> {
    pub(crate) fn block_context(
        &self,
        block_number: u64,
        timestamp: u64,
        proposer: Address,
    ) -> BlockContext {
        BlockContext {
            block_number,
            timestamp,
            chain_id: self.chain_id,
            genesis_hash: self.genesis_hash,
            genesis_timestamp: self.genesis_timestamp,
            proposer,
            validators: Vec::new(),
            block_gas_limit: self.inner.evm.block().gas_limit(),
            user_gas_used: self.accounted_block_gas(),
        }
    }
}

pub(crate) fn read_committee_snapshot<DB>(
    db: &mut DB,
    context: BlockContext,
    snapshot_key: B256,
    validation_error: impl FnOnce(PrecompileError) -> crate::errors::ExecutorValidationError,
) -> Result<Option<outbe_primitives::committee_snapshot::CommitteeSnapshot>, BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
{
    let mut provider = DirectStorageProvider::new(db, context);
    let storage = StorageHandle::new(&mut provider);
    crate::active_evm_runtime_host()
        .read_committee_snapshot(storage, snapshot_key)
        .map_err(|error| {
            if error.is_node_local() || matches!(error, PrecompileError::Fatal(_)) {
                BlockExecutionError::other(error)
            } else {
                validation_error(error).into()
            }
        })
}

pub(in crate::executor) fn build_block_context<DB>(
    db: &mut DB,
    mut context: BlockContext,
) -> Result<BlockContext, BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
{
    let mut provider = DirectStorageProvider::new(db, context.clone());
    let storage = StorageHandle::new(&mut provider);
    let validators = crate::active_evm_runtime_host()
        .consensus_validators(storage)
        .map_err(|e| {
            if e.is_node_local() || matches!(e, PrecompileError::Fatal(_)) {
                BlockExecutionError::other(e)
            } else {
                crate::errors::ExecutorValidationError::block_context(e).into()
            }
        })?;

    context.validators = validators;
    Ok(context)
}
