use super::*;

pub(in crate::executor) fn validate_compressed_entities_root_scheme(
    artifact: Option<CompressedEntitiesRootArtifact>,
) -> Result<CompressedEntitiesRootArtifact, BlockExecutionError> {
    let artifact = artifact.ok_or_else(|| {
        BlockExecutionError::msg("missing compressed-entities root artifact in block extra_data")
    })?;
    if artifact.commitment_scheme_version != outbe_compressed_entities::ACTIVE_COMMITMENT_SCHEME {
        return Err(BlockExecutionError::msg(format!(
            "compressed-entities root artifact scheme mismatch: header={}, active={}",
            artifact.commitment_scheme_version,
            outbe_compressed_entities::ACTIVE_COMMITMENT_SCHEME
        )));
    }
    Ok(artifact)
}

pub(in crate::executor) fn validate_compressed_entities_root_after_seal(
    artifact: Option<CompressedEntitiesRootArtifact>,
    sealed_root: B256,
) -> Result<CompressedEntitiesRootArtifact, BlockExecutionError> {
    let artifact = validate_compressed_entities_root_scheme(artifact)?;
    if artifact.r_sealed != sealed_root {
        return Err(BlockExecutionError::msg(format!(
            "compressed-entities header/SealOutput root mismatch: header={}, seal={sealed_root}",
            artifact.r_sealed
        )));
    }
    Ok(artifact)
}

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    pub(crate) fn compressed_entities_seal_output(
        &self,
    ) -> Option<outbe_compressed_entities::SealOutput> {
        self.compressed_entities_seal_output.clone()
    }

    pub(crate) fn compressed_tree_service(
        &self,
    ) -> Option<Arc<outbe_compressed_entities::CompressedTreeService>> {
        self.compressed_tree_service.clone()
    }
}

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    /// Finalizes the block-scoped compressed-entity overlay while the caller's
    /// state hook is still installed.
    ///
    /// Payload building invokes this before asking the parallel trie task for
    /// its root. Validator/general execution may rely on [`BlockExecutor::finish`],
    /// which calls the same helper. A successful call clears `started`, making
    /// the helper idempotent without permitting a second lifecycle transition.
    pub fn finalize_compressed_entities(&mut self) -> Result<(), BlockExecutionError> {
        if !self.compressed_entities_started {
            return Ok(());
        }

        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
        let chain_id = self.inner.evm.chain_id();
        let proposer = self.inner.evm.block().beneficiary();
        let scope = self.compressed_entities_scope.clone();
        let (_changes, events, seal_output) = {
            let db = self.inner.evm.db_mut();
            let ctx = build_block_context(
                db,
                block_number,
                timestamp,
                chain_id,
                self.genesis_hash,
                proposer,
            )?;
            run_atomic_storage_hook_with_output(db, ctx, |hook_ctx| {
                let lifecycle = outbe_compressed_entities::CompressedEntitiesLifecycleContext::new(
                    hook_ctx.clone(),
                    scope.as_ref(),
                );
                let output = <outbe_compressed_entities::CompressedEntitiesLifecycle as BlockLifecycle>::end_block(
                    &lifecycle,
                )?;
                let evm_root = B256::from(
                    hook_ctx
                        .storage
                        .sload(
                            outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                            U256::from(1),
                        )?
                        .to_be_bytes::<32>(),
                );
                if evm_root != output.new_root {
                    return Err(PrecompileError::Fatal(format!(
                        "compressed-entities SealOutput/EVM root mismatch: seal={}, evm={}",
                        output.new_root, evm_root
                    )));
                }
                Ok(output)
            })?
        };
        if !events.is_empty() {
            return Err(BlockExecutionError::msg(
                "compressed-entity end_block emitted an unexpected event",
            ));
        }

        self.compressed_entities_started = false;
        self.compressed_entities_seal_output = Some(seal_output);
        Ok(())
    }

    /// Prepares the exact CE roots consumed by the terminal OCOMP phase while
    /// leaving the scope active for a possible failure retirement.
    pub(in crate::executor) fn preview_compressed_entities(
        &mut self,
    ) -> Result<outbe_compressed_entities::SealOutput, BlockExecutionError> {
        if !self.compressed_entities_started {
            return Err(BlockExecutionError::msg(
                "compressed-entity preview requested outside active lifecycle",
            ));
        }

        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
        let chain_id = self.inner.evm.chain_id();
        let proposer = self.inner.evm.block().beneficiary();
        let scope = self.compressed_entities_scope.clone();
        let (changes, events, output) = {
            let db = self.inner.evm.db_mut();
            let ctx = build_block_context(
                db,
                block_number,
                timestamp,
                chain_id,
                self.genesis_hash,
                proposer,
            )?;
            run_atomic_storage_hook_with_output(db, ctx, |hook_ctx| {
                let lifecycle = outbe_compressed_entities::CompressedEntitiesLifecycleContext::new(
                    hook_ctx.clone(),
                    scope.as_ref(),
                );
                outbe_compressed_entities::preview_lifecycle_end_block(&lifecycle)
            })?
        };
        if !changes.is_empty() || !events.is_empty() {
            return Err(BlockExecutionError::msg(
                "compressed-entity preview unexpectedly mutated EVM state",
            ));
        }
        Ok(output)
    }
}
