use super::*;

#[derive(Debug)]
pub(super) struct FailOncePartitionLookup {
    pub(super) parent_root: B256,
    pub(super) calls: AtomicUsize,
}

impl outbe_compressed_entities::AuthenticatedParentTree for FailOncePartitionLookup {
    fn parent_block_hash(&self) -> B256 {
        B256::ZERO
    }

    fn parent_root(&self) -> B256 {
        self.parent_root
    }

    fn read_leaf_verified(
        &self,
        _entity: outbe_compressed_entities::EntityRef,
        expected_parent_root: B256,
    ) -> outbe_primitives::error::Result<Option<outbe_compressed_entities::Commitment>> {
        if expected_parent_root != self.parent_root {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "injected parent root mismatch".into(),
            ));
        }
        Ok(None)
    }

    fn partition_present_verified(
        &self,
        _partition: outbe_compressed_entities::PartitionRef,
        expected_parent_root: B256,
    ) -> outbe_primitives::error::Result<bool> {
        if expected_parent_root != self.parent_root {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "injected parent root mismatch".into(),
            ));
        }
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(outbe_primitives::error::PrecompileError::TreeUnavailable(
                "injected partition lookup failure after Promis credit".into(),
            ));
        }
        Ok(false)
    }

    fn partition_root_verified(
        &self,
        _partition: outbe_compressed_entities::PartitionRef,
        expected_parent_root: B256,
    ) -> outbe_primitives::error::Result<Option<B256>> {
        if expected_parent_root != self.parent_root {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "injected parent root mismatch".into(),
            ));
        }
        Ok(None)
    }

    fn prepare_seal(
        &self,
        block_number: u64,
        mutations: &[outbe_compressed_entities::FinalLeafMutation],
        retirements: &[outbe_compressed_entities::PartitionRef],
    ) -> outbe_primitives::error::Result<outbe_compressed_entities::ProvisionalTreeBatch> {
        if !mutations.is_empty() || !retirements.is_empty() {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "empty injected parent received unexpected CE changes".into(),
            ));
        }
        outbe_compressed_entities::ProvisionalTreeBatch::new_identity(
            block_number,
            B256::ZERO,
            B256::ZERO,
        )
        .map_err(|error| {
            outbe_primitives::error::PrecompileError::Fatal(format!(
                "build injected identity batch: {error}"
            ))
        })
    }
}

#[derive(Debug)]
pub(super) struct FailSecondPartitionLookup {
    pub(super) parent_root: B256,
    pub(super) partition_root: B256,
    pub(super) calls: AtomicUsize,
}

impl outbe_compressed_entities::AuthenticatedParentTree for FailSecondPartitionLookup {
    fn parent_block_hash(&self) -> B256 {
        B256::ZERO
    }

    fn parent_root(&self) -> B256 {
        self.parent_root
    }

    fn read_leaf_verified(
        &self,
        _entity: outbe_compressed_entities::EntityRef,
        expected_parent_root: B256,
    ) -> outbe_primitives::error::Result<Option<outbe_compressed_entities::Commitment>> {
        if expected_parent_root != self.parent_root {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "injected parent root mismatch".into(),
            ));
        }
        Ok(None)
    }

    fn partition_present_verified(
        &self,
        partition: outbe_compressed_entities::PartitionRef,
        expected_parent_root: B256,
    ) -> outbe_primitives::error::Result<bool> {
        Ok(self
            .partition_root_verified(partition, expected_parent_root)?
            .is_some())
    }

    fn partition_root_verified(
        &self,
        partition: outbe_compressed_entities::PartitionRef,
        expected_parent_root: B256,
    ) -> outbe_primitives::error::Result<Option<B256>> {
        if expected_parent_root != self.parent_root
            || !matches!(
                partition,
                outbe_compressed_entities::PartitionRef::TributeWwd(_)
            )
        {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "injected partition authentication mismatch".into(),
            ));
        }
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            return Err(outbe_primitives::error::PrecompileError::TreeUnavailable(
                "injected second partition lookup failure".into(),
            ));
        }
        Ok(Some(self.partition_root))
    }

    fn prepare_seal(
        &self,
        _block_number: u64,
        _mutations: &[outbe_compressed_entities::FinalLeafMutation],
        _retirements: &[outbe_compressed_entities::PartitionRef],
    ) -> outbe_primitives::error::Result<outbe_compressed_entities::ProvisionalTreeBatch> {
        Err(outbe_primitives::error::PrecompileError::Fatal(
            "multi-WWD rollback test does not seal its synthetic parent tree".into(),
        ))
    }
}

pub(super) fn create_waiting_day(
    storage: &StorageHandle,
    wwd: outbe_primitives::time::WorldwideDay,
    dtype: u8,
    day_limit: U256,
) -> u64 {
    let mut metadosis = MetadosisContract::new(storage.clone());
    metadosis
        .create_worldwide_day(
            wwd,
            wwd.start_timestamp(),
            LOOKBACK_DELAY_HOURS,
            OFFERING_PERIOD_HOURS,
        )
        .unwrap();
    metadosis.add_active_wwd(wwd).unwrap();
    metadosis
        .set_wwd_day_type(wwd, WwdDayType::try_from(dtype).unwrap())
        .unwrap();
    metadosis
        .fixture_set_wwd_status(wwd, WwdStatus::Waiting)
        .unwrap();
    metadosis.set_metadosis_limit(wwd, day_limit).unwrap();
    metadosis
        .worldwide_days
        .entry(wwd)
        .scheduled_process_time()
        .read()
        .unwrap()
}

pub(super) fn issue_one_tribute_in_scope(
    storage: &StorageHandle,
    scope: &ExecutionScope,
    parent: &TestParent,
    owner: Address,
    wwd: outbe_primitives::time::WorldwideDay,
    nominal: U256,
) {
    let mut tribute = TributeContract::new(storage.clone());
    tribute.initialize_fresh_ocomp_profile().unwrap();
    tribute.unseal_day(wwd).unwrap();
    tribute
        .issue(
            scope,
            parent,
            &TributeData {
                tribute_id: NodContract::generate_nod_id(owner, wwd).unwrap(),
                owner,
                worldwide_day: wwd,
                issuance_amount_minor: nominal,
                issuance_currency: 840,
                nominal_amount_minor: nominal,
                reference_currency: 840,
                exclude_from_intex_issuance: false,
                tribute_price_minor: U256::from(2),
            },
        )
        .unwrap();
    tribute.seal_day(wwd).unwrap();
}

pub(super) fn assert_no_ocomp_job(
    storage: &StorageHandle,
    wwd: outbe_primitives::time::WorldwideDay,
) {
    let metadosis = MetadosisContract::new(storage.clone());
    let limits = crate::ocomp::schema::poc_schema_limits();
    assert!(metadosis.ocomp_scheduler.is_empty().unwrap());
    assert!(metadosis.ocomp_ready_index.is_empty().unwrap());
    assert!(metadosis
        .ocomp_fsm_states
        .get_bytes(&wwd)
        .is_empty()
        .unwrap());
    assert_eq!(metadosis.terminal_intent_count(wwd).unwrap(), 0);
    assert!(metadosis
        .request_limit_receipt(wwd, &limits)
        .unwrap()
        .is_none());
    assert!(metadosis
        .read_pre_admission_envelope(wwd, &limits)
        .unwrap()
        .is_none());
}

pub(super) fn seed_missed_offering_day(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
    base_limit: U256,
    formation_carry_over: U256,
    later_carry_over: U256,
) -> u64 {
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    StorageHandle::enter(provider, |storage| {
        arm_genesis_ocomp(&storage, CHAIN_ID);
        PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(formation_carry_over)
            .unwrap();
        let formation_ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                1,
                wwd.start_timestamp() + 2 * SECONDS_PER_HOUR,
                CHAIN_ID,
            ),
            storage.clone(),
        );
        crate::emission_sink::apply(&formation_ctx, base_limit).unwrap();
        PromisLimitContract::new(storage.clone())
            .checked_add_carry_over(later_carry_over)
            .unwrap();
        MetadosisContract::new(storage)
            .worldwide_days
            .entry(wwd)
            .offering_end()
            .read()
            .unwrap()
    })
}

pub(super) fn begin_persistent_active_scope(
    provider: &mut HashMapStorageProvider,
) -> (ExecutionScope, TestParent) {
    let scope = ExecutionScope::new();
    let parent = TestParent::empty();
    StorageHandle::enter(provider, |storage| {
        storage
            .sstore(
                outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                U256::ZERO,
                U256::from(4),
            )
            .unwrap();
        storage
            .sstore(
                outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(
                    outbe_compressed_entities::sealed_root(B256::ZERO)
                        .unwrap()
                        .as_slice(),
                ),
            )
            .unwrap();
        begin_block(storage, &scope).unwrap();
    });
    (scope, parent)
}

pub(super) fn end_persistent_active_scope(
    provider: &mut HashMapStorageProvider,
    scope: &ExecutionScope,
) {
    StorageHandle::enter(provider, |storage| end_block(storage, scope).unwrap());
}

pub(super) fn run_start_command(
    provider: &mut HashMapStorageProvider,
    scope: &ExecutionScope,
    parent: &TestParent,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::error::Result<()> {
    provider.set_block_number(block_number);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    StorageHandle::enter(provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(block_number, timestamp, CHAIN_ID),
            storage,
        );
        crate::commands::start_metadosis(&ctx, scope, parent)
    })
}
