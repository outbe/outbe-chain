use super::*;

impl ExecutionScope {
    /// Binds the factory/identity to the scope already captured by this EVM's
    /// precompiles. Live wiring calls this after the block parent is known and
    /// before begin-block. Lifecycle and every nested precompile therefore keep
    /// using the same `Arc<ExecutionScope>`.
    pub fn configure_parent_tree_factory(
        &self,
        factory: Arc<dyn AuthenticatedParentTreeFactory>,
        commitment_scheme_version: u32,
        parent_block_number: u64,
        parent_block_hash: B256,
    ) -> Result<()> {
        if self.phase.load(Ordering::Acquire) != PHASE_BEFORE_BEGIN {
            return Err(fatal_scope(
                "exact-parent tree factory configured after begin-block",
            ));
        }
        self.parent_binding_configured
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| fatal_scope("exact-parent tree factory configured more than once"))?;
        self.rpc_read_only.store(false, Ordering::Release);

        let mut tree = self
            .parent_tree
            .lock()
            .map_err(|_| fatal_scope("authenticated parent tree lock poisoned"))?;
        let mut installed_factory = self
            .parent_tree_factory
            .lock()
            .map_err(|_| fatal_scope("authenticated parent tree factory lock poisoned"))?;
        let mut identity = self
            .parent_identity_without_root
            .lock()
            .map_err(|_| fatal_scope("authenticated parent identity lock poisoned"))?;
        *tree = None;
        *installed_factory = Some(factory);
        *identity = Some((
            commitment_scheme_version,
            parent_block_number,
            parent_block_hash,
        ));
        Ok(())
    }

    pub(crate) fn open_exact_parent(&self, evm_root: B256) -> Result<()> {
        let mut tree = self
            .parent_tree
            .lock()
            .map_err(|_| fatal_scope("authenticated parent tree lock poisoned"))?;
        if let Some(opened) = tree.as_ref() {
            if opened.parent_root() != evm_root {
                return Err(fatal_scope(
                    "opened parent tree root does not match EVM slot 1",
                ));
            }
            return Ok(());
        }
        let factory = self
            .parent_tree_factory
            .lock()
            .map_err(|_| fatal_scope("authenticated parent tree factory lock poisoned"))?
            .clone()
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::TreeUnavailable(
                    "no exact-parent tree factory was installed".into(),
                )
            })?;
        let (commitment_scheme_version, block_number, block_hash) = self
            .parent_identity_without_root
            .lock()
            .map_err(|_| fatal_scope("authenticated parent identity lock poisoned"))?
            .ok_or_else(|| fatal_scope("missing exact-parent identity metadata"))?;
        let opened = factory.open_parent(crate::ExactParentIdentity {
            commitment_scheme_version,
            block_number,
            block_hash,
            root: evm_root,
        })?;
        if opened.parent_block_hash() != block_hash || opened.parent_root() != evm_root {
            return Err(fatal_scope(
                "exact-parent factory returned a tree with the wrong block hash or root",
            ));
        }
        *tree = Some(opened);
        Ok(())
    }

    fn opened_parent_tree(&self) -> Result<Arc<dyn AuthenticatedParentTree>> {
        self.parent_tree
            .lock()
            .map_err(|_| fatal_scope("authenticated parent tree lock poisoned"))?
            .clone()
            .ok_or_else(|| {
                outbe_primitives::error::PrecompileError::TreeUnavailable(
                    "exact-parent tree has not been opened by begin-block".into(),
                )
            })
    }

    pub fn read_parent_leaf_verified(
        &self,
        entity: EntityRef,
        expected_parent_root: B256,
    ) -> Result<Option<crate::Commitment>> {
        self.require_readable()?;
        if self.phase.load(Ordering::Acquire) != PHASE_ACTIVE {
            self.open_exact_parent(expected_parent_root)?;
        }
        let parent_tree = self.opened_parent_tree()?;
        if parent_tree.parent_root() != expected_parent_root {
            return Err(fatal_scope(
                "authenticated parent tree root does not match EVM slot 1",
            ));
        }
        parent_tree.read_leaf_verified(entity, expected_parent_root)
    }

    pub fn parent_root(&self) -> Result<B256> {
        self.opened_parent_tree().map(|tree| tree.parent_root())
    }

    /// Best-effort context for a failed body check; never selects a different tree.
    pub(crate) fn diagnostic_parent_binding(&self) -> String {
        let binding = self
            .parent_identity_without_root
            .lock()
            .map(|binding| *binding);
        let rpc_read_only = self.rpc_read_only.load(Ordering::Acquire);
        format!("binding={binding:?} rpc_read_only={rpc_read_only}")
    }

    /// Returns the exact root prepared from all CE mutations currently staged
    /// in the active block without closing the execution scope.
    pub fn provisional_sealed_root(&self) -> Result<B256> {
        Ok(self.provisional_seal()?.new_root)
    }

    /// Resolves one partition root from the active block's provisional seal.
    pub fn provisional_partition_root(
        &self,
        partition: PartitionRef,
    ) -> Result<SealedCollectionRoot> {
        let output = self.provisional_seal()?;
        self.collection_root_from_output(&output, partition)
    }

    fn provisional_seal(&self) -> Result<crate::SealOutput> {
        self.require_active()?;
        let output = self
            .provisional_seal
            .lock()
            .map_err(|_| fatal_scope("provisional compressed-entity seal lock poisoned"))?
            .clone()
            .ok_or_else(|| fatal_scope("provisional compressed-entity seal is absent"))?;
        self.validate_seal_output(&output)?;
        Ok(output)
    }

    /// Returns the exact global CE root recorded by this scope's completed
    /// end-block seal. It is unavailable before the executor closes the CE
    /// lifecycle and does not expose the mutable staging envelope.
    pub fn completed_sealed_root(&self) -> Result<B256> {
        Ok(self.completed_seal()?.new_root)
    }

    /// Returns the catalog root underlying this scope's completed seal.
    pub fn completed_catalog_root(&self) -> Result<B256> {
        Ok(self.completed_seal()?.staged_tree_batch.new_catalog_root)
    }

    /// Resolves one partition root from this scope's exact completed seal.
    ///
    /// Terminal system phases use this instead of accepting a caller-supplied
    /// `SealOutput`, keeping the executor-owned seal as the only authority.
    pub fn completed_partition_root(
        &self,
        partition: PartitionRef,
    ) -> Result<SealedCollectionRoot> {
        let output = self.completed_seal()?;
        self.sealed_collection_root(&output, partition)
    }

    fn completed_seal(&self) -> Result<crate::SealOutput> {
        if self.phase.load(Ordering::Acquire) != PHASE_ENDED {
            return Err(fatal_scope(
                "compressed-entity seal requested before end-block completed",
            ));
        }
        let output = self
            .completed_seal
            .lock()
            .map_err(|_| fatal_scope("completed compressed-entity seal lock poisoned"))?
            .clone()
            .ok_or_else(|| fatal_scope("completed compressed-entity seal is absent"))?;
        self.validate_seal_output(&output)?;
        Ok(output)
    }

    /// Resolves a collection root from the exact completed CE seal. A changed
    /// collection comes from the validated provisional batch; an unchanged
    /// collection comes from the authenticated parent catalog.
    pub fn sealed_collection_root(
        &self,
        output: &crate::SealOutput,
        partition: PartitionRef,
    ) -> Result<SealedCollectionRoot> {
        if self.phase.load(Ordering::Acquire) != PHASE_ENDED {
            return Err(fatal_scope(
                "collection root requested before compressed-entity seal completed",
            ));
        }
        if self
            .completed_seal
            .lock()
            .map_err(|_| fatal_scope("completed compressed-entity seal lock poisoned"))?
            .as_ref()
            != Some(output)
        {
            return Err(fatal_scope(
                "SealOutput was not completed by this execution scope",
            ));
        }
        self.collection_root_from_output(output, partition)
    }

    fn validate_seal_output(&self, output: &crate::SealOutput) -> Result<()> {
        if output.parent_root != output.staged_tree_batch.parent_root()
            || output.new_root != output.staged_tree_batch.new_root()
        {
            return Err(fatal_scope(
                "compressed-entity SealOutput does not match its staged tree batch",
            ));
        }
        if self.opened_parent_tree()?.parent_root() != output.parent_root {
            return Err(fatal_scope(
                "compressed-entity SealOutput does not match the authenticated parent",
            ));
        }
        Ok(())
    }

    fn collection_root_from_output(
        &self,
        output: &crate::SealOutput,
        partition: PartitionRef,
    ) -> Result<SealedCollectionRoot> {
        self.validate_seal_output(output)?;
        let parent = self.opened_parent_tree()?;
        let (_, key) = crate::partition_collection_key(partition).map_err(fatal_scope)?;
        let root = match output.staged_tree_batch.changed_collections.get(&key) {
            Some(crate::CollectionOperation::Mutate(batch)) => Some(batch.new_collection_root),
            Some(crate::CollectionOperation::Retire(_)) => None,
            None => parent.partition_root_verified(partition, output.parent_root)?,
        }
        .filter(|root| !root.is_zero())
        .ok_or_else(|| fatal_scope("sealed compressed-entity collection is absent"))?;

        Ok(SealedCollectionRoot { partition, root })
    }

    pub(crate) fn record_provisional_seal(&self, output: &crate::SealOutput) -> Result<()> {
        self.require_active()?;
        self.validate_seal_output(output)?;
        *self
            .provisional_seal
            .lock()
            .map_err(|_| fatal_scope("provisional compressed-entity seal lock poisoned"))? =
            Some(output.clone());
        Ok(())
    }

    pub(crate) fn prepare_tree_seal(
        &self,
        block_number: u64,
        mutations: &[FinalLeafMutation],
        retirements: &[PartitionRef],
    ) -> Result<crate::ProvisionalTreeBatch> {
        self.require_active()?;
        self.opened_parent_tree()?
            .prepare_seal(block_number, mutations, retirements)
    }

    pub(crate) fn partition_present_verified(&self, partition: PartitionRef) -> Result<bool> {
        self.require_active()?;
        let parent = self.opened_parent_tree()?;
        parent.partition_present_verified(partition, parent.parent_root())
    }

    /// Returns the authenticated parent root for one partition during the
    /// active block lifecycle. Domain owners use this constant-size proof when
    /// a terminal aggregate effect must bind and retire a complete partition
    /// without enumerating its bodies.
    pub fn authenticated_partition_root(&self, partition: PartitionRef) -> Result<Option<B256>> {
        self.require_active()?;
        let parent = self.opened_parent_tree()?;
        parent.partition_root_verified(partition, parent.parent_root())
    }

    pub fn ce_work_checkpoint(&self) -> Result<CeWorkCheckpoint> {
        self.ce_work
            .lock()
            .map(|state| CeWorkCheckpoint(state.clone()))
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))
    }

    /// Opens one executor transaction window so overflow can distinguish a
    /// transaction that cannot fit an empty block from ordinary remaining
    /// block-capacity exhaustion.
    pub fn begin_ce_work_transaction(&self) -> Result<()> {
        self.ce_work_failure
            .store(CE_WORK_FAILURE_NONE, Ordering::Release);
        let mut state = self
            .ce_work
            .lock()
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))?;
        if state.transaction_start.is_some() {
            return Err(fatal_scope(
                "compressed-entity work transaction window is already active",
            ));
        }
        state.transaction_start = Some(CeWorkTransactionStart {
            transaction_keys: BTreeSet::new(),
        });
        Ok(())
    }

    /// Closes the current executor transaction window. Work remains reserved.
    /// Excluded transactions are restored separately from their checkpoint.
    pub fn end_ce_work_transaction(&self) -> Result<()> {
        let mut state = self
            .ce_work
            .lock()
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))?;
        if state.transaction_start.take().is_none() {
            return Err(fatal_scope(
                "compressed-entity work transaction window is not active",
            ));
        }
        Ok(())
    }

    /// Returns and clears the typed CE admission failure recorded through the
    /// infallible revm precompile boundary.
    pub fn take_ce_work_failure(&self) -> Option<outbe_primitives::error::PrecompileError> {
        match self
            .ce_work_failure
            .swap(CE_WORK_FAILURE_NONE, Ordering::AcqRel)
        {
            CE_WORK_FAILURE_TRANSACTION => {
                Some(outbe_primitives::error::PrecompileError::TransactionCeWorkLimitExceeded)
            }
            CE_WORK_FAILURE_BLOCK => {
                Some(outbe_primitives::error::PrecompileError::BlockCeWorkCapacityExhausted)
            }
            _ => None,
        }
    }

    pub fn restore_ce_work_checkpoint(&self, checkpoint: CeWorkCheckpoint) -> Result<()> {
        let mut state = self
            .ce_work
            .lock()
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))?;
        if checkpoint.0.used > state.used || !checkpoint.0.seen_keys.is_subset(&state.seen_keys) {
            return Err(fatal_scope(
                "invalid compressed-entity work checkpoint restore",
            ));
        }
        *state = checkpoint.0;
        Ok(())
    }

    pub fn ce_work_used(&self) -> Result<u64> {
        self.ce_work
            .lock()
            .map(|state| state.used)
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))
    }

    pub(crate) fn reserve_unique_key_work(&self, entity: EntityRef) -> Result<()> {
        self.require_active()?;
        let mut state = self
            .ce_work
            .lock()
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))?;
        let transaction_unique_keys = state.transaction_start.as_ref().map_or(1_usize, |start| {
            if start.transaction_keys.contains(&entity) {
                0
            } else {
                start.transaction_keys.len().saturating_add(1)
            }
        });
        if transaction_unique_keys == 0 {
            return Ok(());
        }
        let transaction_units = u64::try_from(transaction_unique_keys)
            .ok()
            .and_then(|count| self.ce_work_config.unique_key_units.checked_mul(count))
            .and_then(|units| self.ce_work_config.seal_base_units.checked_add(units));
        if transaction_units.is_none_or(|units| units > self.ce_work_config.work_limit) {
            self.ce_work_failure
                .store(CE_WORK_FAILURE_TRANSACTION, Ordering::Release);
            return Err(outbe_primitives::error::PrecompileError::TransactionCeWorkLimitExceeded);
        }
        if state.seen_keys.contains(&entity) {
            if let Some(start) = state.transaction_start.as_mut() {
                start.transaction_keys.insert(entity);
            }
            return Ok(());
        }
        let next = state
            .used
            .checked_add(self.ce_work_config.unique_key_units)
            .ok_or(outbe_primitives::error::PrecompileError::BlockCeWorkCapacityExhausted)?;
        if next > self.ce_work_config.work_limit {
            self.ce_work_failure
                .store(CE_WORK_FAILURE_BLOCK, Ordering::Release);
            return Err(outbe_primitives::error::PrecompileError::BlockCeWorkCapacityExhausted);
        }
        state.seen_keys.insert(entity);
        if let Some(start) = state.transaction_start.as_mut() {
            start.transaction_keys.insert(entity);
        }
        state.used = next;
        Ok(())
    }

    /// Opens the receipt-visible CE gas budget for one system transaction.
    pub fn begin_explicit_gas_window(&self, gas_limit: u64) -> Result<ExplicitGasWindow<'_>> {
        self.require_active()?;
        self.explicit_gas_window_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| fatal_scope("compressed-entity explicit gas window is already active"))?;
        let start = self.explicit_gas_checkpoint();
        self.explicit_gas_window_start
            .store(start.0, Ordering::Release);
        self.explicit_gas_window_limit
            .store(gas_limit, Ordering::Release);
        Ok(ExplicitGasWindow { scope: self, start })
    }

    /// Snapshots the monotonic explicit CE charge counter.
    ///
    /// User transactions do not consume this snapshot: their CE charges are
    /// already part of normal EVM gas. The system lane snapshots immediately
    /// before execution and publishes exactly [`Self::explicit_gas_since`].
    #[must_use]
    pub fn explicit_gas_checkpoint(&self) -> ExplicitGasCheckpoint {
        ExplicitGasCheckpoint(self.explicit_gas_charged.load(Ordering::Acquire))
    }

    /// Returns explicit CE gas successfully deducted since `checkpoint`.
    pub fn explicit_gas_since(&self, checkpoint: ExplicitGasCheckpoint) -> Result<u64> {
        self.explicit_gas_charged
            .load(Ordering::Acquire)
            .checked_sub(checkpoint.0)
            .ok_or_else(|| fatal_scope("compressed-entity gas checkpoint exceeds current counter"))
    }

    pub(crate) fn deduct_explicit_gas(&self, storage: &StorageHandle<'_>, gas: u64) -> Result<()> {
        self.require_readable()?;
        if self.explicit_gas_window_active.load(Ordering::Acquire) {
            let charged = self.explicit_gas_charged.load(Ordering::Acquire);
            let start = self.explicit_gas_window_start.load(Ordering::Acquire);
            let limit = self.explicit_gas_window_limit.load(Ordering::Acquire);
            let used = charged.checked_sub(start).ok_or_else(|| {
                fatal_scope("compressed-entity explicit gas window starts after current counter")
            })?;
            if used.checked_add(gas).is_none_or(|next| next > limit) {
                return Err(outbe_primitives::error::PrecompileError::OutOfGas);
            }
        }
        storage.deduct_gas(gas)?;
        self.explicit_gas_charged
            .try_update(Ordering::AcqRel, Ordering::Acquire, |charged| {
                charged.checked_add(gas)
            })
            .map(|_| ())
            .map_err(|_| fatal_scope("compressed-entity explicit gas counter overflow"))
    }

    pub(crate) fn require_active(&self) -> Result<()> {
        if self.phase.load(Ordering::Acquire) == PHASE_ACTIVE {
            return Ok(());
        }
        Err(fatal_scope(
            "compressed-entity execution attempted outside active block lifecycle",
        ))
    }

    pub(super) fn require_readable(&self) -> Result<()> {
        if self.phase.load(Ordering::Acquire) == PHASE_ACTIVE
            || self.rpc_read_only.load(Ordering::Acquire)
        {
            return Ok(());
        }
        Err(fatal_scope(
            "compressed-entity execution attempted outside active block lifecycle",
        ))
    }

    pub(crate) fn activate(&self) -> Result<()> {
        if self.rpc_read_only.load(Ordering::Acquire) {
            return Err(fatal_scope(
                "finalized RPC read scope cannot enter block mutation lifecycle",
            ));
        }
        self.phase
            .compare_exchange(
                PHASE_BEFORE_BEGIN,
                PHASE_ACTIVE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| fatal_scope("compressed-entity begin_block called more than once"))?;
        let mut work = self
            .ce_work
            .lock()
            .map_err(|_| fatal_scope("compressed-entity work meter lock poisoned"))?;
        if self.ce_work_config.seal_base_units > self.ce_work_config.work_limit {
            return Err(outbe_primitives::error::PrecompileError::TransactionCeWorkLimitExceeded);
        }
        work.used = self.ce_work_config.seal_base_units;
        work.seen_keys.clear();
        work.transaction_start = None;
        self.ce_work_failure
            .store(CE_WORK_FAILURE_NONE, Ordering::Release);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn finish(&self) -> Result<()> {
        self.phase
            .compare_exchange(
                PHASE_ACTIVE,
                PHASE_ENDED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| fatal_scope("compressed-entity end_block called outside active lifecycle"))
    }

    pub(crate) fn finish_with_seal(&self, output: &crate::SealOutput) -> Result<()> {
        let mut completed = self
            .completed_seal
            .lock()
            .map_err(|_| fatal_scope("completed compressed-entity seal lock poisoned"))?;
        if completed.is_some() {
            return Err(fatal_scope(
                "compressed-entity seal identity recorded more than once",
            ));
        }
        if self.phase.load(Ordering::Acquire) != PHASE_ACTIVE {
            return Err(fatal_scope(
                "compressed-entity end_block called outside active lifecycle",
            ));
        }
        *completed = Some(output.clone());
        if self
            .phase
            .compare_exchange(
                PHASE_ACTIVE,
                PHASE_ENDED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            *completed = None;
            return Err(fatal_scope(
                "compressed-entity end_block called outside active lifecycle",
            ));
        }
        Ok(())
    }
}
