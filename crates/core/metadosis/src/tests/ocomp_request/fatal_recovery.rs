use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_compressed_entities::{
    begin_block, end_block, preview_end_block, AuthenticatedParentTree,
    AuthenticatedParentTreeFactory, Commitment, EntityRef, ExactParentIdentity, ExecutionScope,
    FinalLeafMutation, PartitionRef, ProvisionalTreeBatch, ACTIVE_COMMITMENT_SCHEME,
};
use outbe_ocomp_protocol::state::{OcompJobRecordV1, OcompJobStatus, OcompTerminalOutcome};
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, PROMIS_LIMIT_ADDRESS},
    block::{BlockContext, BlockRuntimeContext},
    chain,
    error::{PrecompileError, Result},
    storage::{hashmap::HashMapStorageProvider, MetadosisMutationPurposeTag, StorageHandle},
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

use super::{poc_schema_limits, prepare_request_fixture};
use crate::{api, precompile::IMetadosis, schema::MetadosisContract, WwdMembership, WwdStatus};

mod commands {
    pub(crate) use crate::commands::{
        fail_worldwide_day_for_test, record_certified_parent_finality,
        run_ocomp_lifecycle_begin_with_scope,
        run_ocomp_terminal_request_with_completed_fixture as run_ocomp_terminal_request,
    };
}

#[derive(Debug)]
struct OneTributePartitionTree {
    parent_root: B256,
    parent_catalog_root: B256,
    parent_block_hash: B256,
    partition_roots: Vec<(outbe_primitives::time::WorldwideDay, B256)>,
}

impl AuthenticatedParentTree for OneTributePartitionTree {
    fn parent_block_hash(&self) -> B256 {
        self.parent_block_hash
    }

    fn parent_root(&self) -> B256 {
        self.parent_root
    }

    fn read_leaf_verified(
        &self,
        _entity: EntityRef,
        expected_parent_root: B256,
    ) -> Result<Option<Commitment>> {
        assert_eq!(expected_parent_root, self.parent_root);
        Ok(None)
    }

    fn partition_present_verified(
        &self,
        partition: PartitionRef,
        expected_parent_root: B256,
    ) -> Result<bool> {
        Ok(self
            .partition_root_verified(partition, expected_parent_root)?
            .is_some())
    }

    fn partition_root_verified(
        &self,
        partition: PartitionRef,
        expected_parent_root: B256,
    ) -> Result<Option<B256>> {
        assert_eq!(expected_parent_root, self.parent_root);
        let PartitionRef::TributeWwd(worldwide_day) = partition;
        Ok(self
            .partition_roots
            .iter()
            .find_map(|(candidate, root)| (*candidate == worldwide_day).then_some(*root)))
    }

    fn prepare_seal(
        &self,
        block_number: u64,
        _mutations: &[FinalLeafMutation],
        _retirements: &[PartitionRef],
    ) -> Result<ProvisionalTreeBatch> {
        ProvisionalTreeBatch::new_identity(
            block_number,
            self.parent_block_hash,
            self.parent_catalog_root,
        )
        .map_err(|error| PrecompileError::Fatal(error.to_string()))
    }
}

#[derive(Debug)]
struct OneTributePartitionFactory {
    parent_root: B256,
    parent_catalog_root: B256,
    parent_block_hash: B256,
    partition_roots: Vec<(outbe_primitives::time::WorldwideDay, B256)>,
}

impl AuthenticatedParentTreeFactory for OneTributePartitionFactory {
    fn open_parent(&self, parent: ExactParentIdentity) -> Result<Arc<dyn AuthenticatedParentTree>> {
        assert_eq!(parent.root, self.parent_root);
        assert_eq!(parent.block_hash, self.parent_block_hash);
        Ok(Arc::new(OneTributePartitionTree {
            parent_root: self.parent_root,
            parent_catalog_root: self.parent_catalog_root,
            parent_block_hash: self.parent_block_hash,
            partition_roots: self.partition_roots.clone(),
        }))
    }
}

fn run_terminal_request(
    provider: &mut HashMapStorageProvider,
    fixture: &super::PreparedRequestFixture,
) {
    try_terminal_request(provider, fixture).expect("production terminal request");
}

fn try_terminal_request(
    provider: &mut HashMapStorageProvider,
    fixture: &super::PreparedRequestFixture,
) -> Result<()> {
    provider.set_block_number(fixture.block_number);
    provider.set_timestamp(U256::from(fixture.block_time));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
    StorageHandle::enter(provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(
                fixture.block_number,
                fixture.block_time,
                chain::CHAIN_ID,
            ),
            storage,
        );
        commands::run_ocomp_terminal_request(&ctx, &fixture.scope)
    })
}

fn live_intent(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
) -> (B256, OcompJobRecordV1) {
    StorageHandle::enter(provider, |storage| {
        let limits = poc_schema_limits();
        let metadosis = MetadosisContract::new(storage.clone());
        let state = metadosis
            .live_ocomp_fsm_states(&limits)
            .unwrap()
            .into_iter()
            .find(|state| state.projection().worldwide_day == wwd)
            .expect("live WWD job");
        let intent_id = state.projection().live_intent_id.expect("live intent");
        let record = OcompJobRecordV1::decode_canonical(
            &api::get_offchain_job(storage, intent_id).expect("public live job"),
            &limits,
        )
        .expect("canonical live job");
        (intent_id, record)
    })
}

pub(super) fn begin_recovery_scope(
    provider: &mut HashMapStorageProvider,
    fixture: &super::PreparedRequestFixture,
    block_number: u64,
) -> ExecutionScope {
    begin_recovery_scope_for_wwd(provider, &fixture.scope, fixture.wwd, block_number)
}

pub(super) fn begin_recovery_scope_for_wwd(
    provider: &mut HashMapStorageProvider,
    completed_scope: &ExecutionScope,
    wwd: outbe_primitives::time::WorldwideDay,
    block_number: u64,
) -> ExecutionScope {
    provider.set_block_number(block_number);
    StorageHandle::enter(provider, |storage| {
        begin_recovery_scope_from_storage(storage, completed_scope, wwd, block_number)
    })
}

pub(super) fn begin_recovery_scope_from_storage(
    storage: StorageHandle<'_>,
    completed_scope: &ExecutionScope,
    wwd: outbe_primitives::time::WorldwideDay,
    block_number: u64,
) -> ExecutionScope {
    begin_recovery_scope_for_wwds_from_storage(storage, completed_scope, &[wwd], block_number)
}

pub(super) fn begin_recovery_scope_for_wwds_from_storage(
    storage: StorageHandle<'_>,
    completed_scope: &ExecutionScope,
    worldwide_days: &[outbe_primitives::time::WorldwideDay],
    block_number: u64,
) -> ExecutionScope {
    let parent_root = completed_scope.completed_sealed_root().unwrap();
    let parent_catalog_root = completed_scope.completed_catalog_root().unwrap();
    let partition_roots = worldwide_days
        .iter()
        .map(|wwd| {
            (
                *wwd,
                completed_scope
                    .completed_partition_root(PartitionRef::TributeWwd(*wwd))
                    .unwrap()
                    .root(),
            )
        })
        .collect();
    let parent_block_hash = B256::repeat_byte(0x91);
    storage
        .sstore(
            COMPRESSED_ENTITIES_ADDRESS,
            U256::from(1),
            U256::from_be_slice(parent_root.as_slice()),
        )
        .unwrap();
    let scope = ExecutionScope::new();
    scope
        .configure_parent_tree_factory(
            Arc::new(OneTributePartitionFactory {
                parent_root,
                parent_catalog_root,
                parent_block_hash,
                partition_roots,
            }),
            ACTIVE_COMMITMENT_SCHEME,
            block_number - 1,
            parent_block_hash,
        )
        .unwrap();
    begin_block(storage, &scope).unwrap();
    scope
}

fn run_lifecycle_begin(
    provider: &mut HashMapStorageProvider,
    scope: &ExecutionScope,
    block_number: u64,
    timestamp: u64,
) -> Result<()> {
    provider.set_block_number(block_number);
    provider.set_timestamp(U256::from(timestamp));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
    StorageHandle::enter(provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(block_number, timestamp, chain::CHAIN_ID),
            storage,
        );
        commands::run_ocomp_lifecycle_begin_with_scope(&ctx, scope)
    })
}

fn run_direct_failed_day_recovery(
    provider: &mut HashMapStorageProvider,
    scope: &ExecutionScope,
    wwd: outbe_primitives::time::WorldwideDay,
    block_number: u64,
    timestamp: u64,
) -> Result<()> {
    provider.set_block_number(block_number);
    provider.set_timestamp(U256::from(timestamp));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
    StorageHandle::enter(provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(block_number, timestamp, chain::CHAIN_ID),
            storage,
        );
        commands::fail_worldwide_day_for_test(&ctx, scope, wwd)
    })
}

fn assert_failed_day_recovery(
    provider: &mut HashMapStorageProvider,
    fixture: &super::PreparedRequestFixture,
    expected_promis_total: U256,
) {
    StorageHandle::enter(provider, |storage| {
        let projection = api::worldwide_day(storage.clone(), fixture.wwd)
            .unwrap()
            .expect("failed WWD remains queryable");
        assert_eq!(projection.status, WwdStatus::Failed);
        assert_eq!(projection.membership, WwdMembership::Closed);

        let metadosis = MetadosisContract::new(storage.clone());
        assert!(metadosis.ocomp_scheduler.is_empty().unwrap());
        assert!(metadosis.ocomp_ready_index.is_empty().unwrap());
        assert!(metadosis.ocomp_response_deadline_index.is_empty().unwrap());
        assert!(metadosis
            .ocomp_fsm_states
            .get_bytes(&fixture.wwd)
            .is_empty()
            .unwrap());

        let tribute = TributeContract::new(storage.clone());
        let totals = tribute.get_day_totals(fixture.wwd).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_total_minor, U256::ZERO);
        assert_eq!(tribute.total_supply().unwrap(), 0);
        assert_eq!(
            tribute
                .pre_admission_projection(fixture.wwd)
                .unwrap()
                .source_generation,
            1
        );
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            expected_promis_total
        );
    });
}

fn assert_expired_job(provider: &mut HashMapStorageProvider, intent_id: B256) -> OcompJobRecordV1 {
    StorageHandle::enter(provider, |storage| {
        let record = OcompJobRecordV1::decode_canonical(
            &api::get_offchain_job(storage, intent_id).expect("terminal job evidence is retained"),
            &poc_schema_limits(),
        )
        .expect("canonical terminal job");
        assert_eq!(record.status, OcompJobStatus::Expired);
        let terminal = record.terminal.as_ref().expect("expired terminal evidence");
        assert_eq!(terminal.outcome, OcompTerminalOutcome::Expired);
        assert_eq!(terminal.completed_binding, None);
        record
    })
}

fn prepare_skipped_finality_recovery() -> (
    HashMapStorageProvider,
    super::PreparedRequestFixture,
    ExecutionScope,
    u64,
    B256,
) {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);
    run_terminal_request(&mut provider, &fixture);
    let intent_id = live_intent(&mut provider, fixture.wwd).0;
    let recovery_height = fixture.block_number + 65;
    let scope = begin_recovery_scope(&mut provider, &fixture, recovery_height);
    (provider, fixture, scope, recovery_height, intent_id)
}

#[test]
fn failure_before_request_split_returns_the_full_formed_day_limit() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);
    let recovery_height = fixture.block_number + 1;
    let scope = begin_recovery_scope(&mut provider, &fixture, recovery_height);

    run_direct_failed_day_recovery(
        &mut provider,
        &scope,
        fixture.wwd,
        recovery_height,
        fixture.block_time + 1,
    )
    .expect("pre-request Metadosis failure closes only the WWD");

    assert_failed_day_recovery(&mut provider, &fixture, U256::from(100));
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage);
        assert!(metadosis
            .request_limit_receipt(fixture.wwd, &poc_schema_limits())
            .unwrap()
            .is_none());
        assert_eq!(metadosis.terminal_intent_count(fixture.wwd).unwrap(), 0);
    });
}

#[test]
fn terminal_business_failure_fails_day_before_the_single_final_seal() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = super::prepare_request_fixture_with_day_type(
        &mut provider,
        true,
        crate::WwdDayType::Unknown,
    );
    let block_number = fixture.block_number + 1;
    let scope = begin_recovery_scope(&mut provider, &fixture, block_number);
    StorageHandle::enter(&mut provider, |storage| {
        preview_end_block(storage, &scope).unwrap()
    });

    provider.set_block_number(block_number);
    provider.set_timestamp(U256::from(fixture.block_time + 1));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(block_number, fixture.block_time + 1, chain::CHAIN_ID),
            storage.clone(),
        );
        crate::commands::run_ocomp_terminal_request(&ctx, &scope).unwrap();
        end_block(storage, &scope).unwrap();
    });

    assert_failed_day_recovery(&mut provider, &fixture, U256::from(100));
    assert!(scope.completed_sealed_root().is_ok());
}

#[test]
fn delayed_voting_open_preserves_the_single_job_until_its_deadline() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);
    run_terminal_request(&mut provider, &fixture);
    let (intent_id, record) = live_intent(&mut provider, fixture.wwd);
    let retained_lysis_limit_minor = record.intent.frozen_metadosis_values.lysis_limit_minor;

    let finality_height = fixture.block_number + 2;
    let certified = outbe_primitives::storage::MetadosisCertifiedFinalityBinding::new(
        chain::CHAIN_ID,
        finality_height,
        fixture.block_number,
        B256::repeat_byte(0x46),
        B256::repeat_byte(0x98),
    );
    provider.set_block_number(finality_height);
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CertifiedFinality);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(finality_height, fixture.block_time + 1, chain::CHAIN_ID),
            storage,
        );
        assert!(commands::record_certified_parent_finality(&ctx, &certified).unwrap());
    });
    let finalized = live_intent(&mut provider, fixture.wwd)
        .1
        .finalized
        .expect("certified finality");
    let recovery_height = finalized.open_height + 1;
    let scope = begin_recovery_scope(&mut provider, &fixture, recovery_height);

    run_lifecycle_begin(
        &mut provider,
        &scope,
        recovery_height,
        fixture.block_time + 2,
    )
    .expect("a delayed lifecycle tick opens the existing job");

    let (_, opened) = live_intent(&mut provider, fixture.wwd);
    assert_eq!(opened.status, OcompJobStatus::VotingOpen);
    assert!(opened.terminal.is_none());

    let expiry_scope = begin_recovery_scope(&mut provider, &fixture, finalized.deadline_height);
    run_lifecycle_begin(
        &mut provider,
        &expiry_scope,
        finalized.deadline_height,
        fixture.block_time + 3,
    )
    .expect("the same job expires at its immutable deadline");

    assert_failed_day_recovery(&mut provider, &fixture, retained_lysis_limit_minor);
    assert_expired_job(&mut provider, intent_id);
}

#[test]
fn skipped_awaiting_finality_deadline_expires_the_single_job() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);
    run_terminal_request(&mut provider, &fixture);
    let (intent_id, record) = live_intent(&mut provider, fixture.wwd);
    let retained_lysis_limit_minor = record.intent.frozen_metadosis_values.lysis_limit_minor;
    let carry_over_before = U256::from(17);
    StorageHandle::enter(&mut provider, |storage| {
        PromisLimitContract::new(storage)
            .checked_add_carry_over(carry_over_before)
            .unwrap();
    });
    let recovery_height = fixture.block_number + 65;
    let scope = begin_recovery_scope(&mut provider, &fixture, recovery_height);

    run_lifecycle_begin(
        &mut provider,
        &scope,
        recovery_height,
        fixture.block_time + 65,
    )
    .expect("missed finality deadline expires the single job");

    assert_failed_day_recovery(
        &mut provider,
        &fixture,
        carry_over_before + retained_lysis_limit_minor,
    );
    assert_expired_job(&mut provider, intent_id);

    let promis_after = StorageHandle::enter(&mut provider, |storage| {
        PromisLimitContract::new(storage)
            .get_total_unallocated()
            .unwrap()
    });
    run_lifecycle_begin(
        &mut provider,
        &scope,
        recovery_height + 1,
        fixture.block_time + 66,
    )
    .expect("closed failed WWD replay is a no-op");
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            promis_after,
            "replay must not credit PromiseLimit twice"
        );
    });
}

#[test]
fn skipped_response_deadline_expires_job_and_retains_closed_vote_accountability() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);
    run_terminal_request(&mut provider, &fixture);
    let (intent_id, record) = live_intent(&mut provider, fixture.wwd);
    let retained_lysis_limit_minor = record.intent.frozen_metadosis_values.lysis_limit_minor;

    let finality_height = fixture.block_number + 2;
    let certified = outbe_primitives::storage::MetadosisCertifiedFinalityBinding::new(
        chain::CHAIN_ID,
        finality_height,
        fixture.block_number,
        B256::repeat_byte(0x47),
        B256::repeat_byte(0x99),
    );
    provider.set_block_number(finality_height);
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CertifiedFinality);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(finality_height, fixture.block_time + 1, chain::CHAIN_ID),
            storage,
        );
        assert!(commands::record_certified_parent_finality(&ctx, &certified).unwrap());
    });
    let finalized = live_intent(&mut provider, fixture.wwd)
        .1
        .finalized
        .expect("certified finality");
    let open_scope = begin_recovery_scope(&mut provider, &fixture, finalized.open_height);
    run_lifecycle_begin(
        &mut provider,
        &open_scope,
        finalized.open_height,
        fixture.block_time + 2,
    )
    .expect("voting opens at the exact height");

    let recovery_height = finalized.deadline_height + 1;
    let recovery_scope = begin_recovery_scope(&mut provider, &fixture, recovery_height);
    run_lifecycle_begin(
        &mut provider,
        &recovery_scope,
        recovery_height,
        fixture.block_time + 3,
    )
    .expect("missed response deadline expires the single job");

    assert_failed_day_recovery(&mut provider, &fixture, retained_lysis_limit_minor);
    let expired = assert_expired_job(&mut provider, intent_id);
    let job_id = expired.finalized.expect("finalized expired job").job_id;
    StorageHandle::enter(&mut provider, |storage| {
        let accountability = MetadosisContract::new(storage)
            .result_vote_accountability(job_id, &poc_schema_limits())
            .unwrap()
            .expect("vote accountability is retained");
        assert!(accountability.closed_summary.is_some());
    });
}

#[test]
fn every_expired_day_recovery_mutation_is_atomic_and_retryable() {
    let (mut probe, fixture, scope, recovery_height, _) = prepare_skipped_finality_recovery();
    probe.fail_after_mutation_at(usize::MAX);
    run_lifecycle_begin(&mut probe, &scope, recovery_height, fixture.block_time + 65)
        .expect("clean failed-day recovery");
    let mutation_count = probe.clear_mutation_failure();
    assert!(
        mutation_count >= 12,
        "recovery must cross every owner boundary"
    );
    let clean_storage = probe.storage.clone();
    let clean_events = probe.events.clone();
    let clean_ordered_events = probe.get_ordered_events().to_vec();
    let clean_ce_work = scope.ce_work_checkpoint().unwrap();

    for operation in 0..mutation_count {
        let (mut provider, fixture, scope, recovery_height, intent_id) =
            prepare_skipped_finality_recovery();
        let storage_before = provider.storage.clone();
        let events_before = provider.events.clone();
        let ordered_events_before = provider.get_ordered_events().to_vec();
        let ce_before = scope.ce_work_checkpoint().unwrap();
        provider.fail_after_mutation_at(operation);

        let error = run_lifecycle_begin(
            &mut provider,
            &scope,
            recovery_height,
            fixture.block_time + 65,
        )
        .expect_err("injected recovery mutation must propagate");
        assert!(matches!(error, PrecompileError::Storage(_)));
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(provider.storage, storage_before, "storage at {operation}");
        assert_eq!(provider.events, events_before, "events at {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            ordered_events_before.as_slice(),
            "ordered events at {operation}"
        );
        assert_eq!(
            scope.ce_work_checkpoint().unwrap(),
            ce_before,
            "CE work at {operation}"
        );

        run_lifecycle_begin(
            &mut provider,
            &scope,
            recovery_height,
            fixture.block_time + 65,
        )
        .expect("exact recovery retry");
        assert_eq!(
            provider.storage, clean_storage,
            "retry storage at {operation}"
        );
        assert_eq!(provider.events, clean_events, "retry events at {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            clean_ordered_events.as_slice(),
            "retry ordered events at {operation}"
        );
        assert_eq!(
            scope.ce_work_checkpoint().unwrap(),
            clean_ce_work,
            "retry CE work at {operation}"
        );
        assert_expired_job(&mut provider, intent_id);
    }
}

#[test]
fn expired_day_recovery_does_not_mutate_an_independent_live_worldwide_day() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = super::prepare_ready_days_fixture(&mut provider, true);
    for (block_number, block_time) in [
        (fixture.block_number, fixture.block_time),
        (fixture.block_number + 1, fixture.block_time + 1),
    ] {
        provider.set_block_number(block_number);
        provider.set_timestamp(U256::from(block_time));
        provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(block_number, block_time, chain::CHAIN_ID),
                storage,
            );
            commands::run_ocomp_terminal_request(&ctx, &fixture.scope)
        })
        .expect("independent live request");
    }

    let (failed_intent, survivor_intent, recovery_height) =
        StorageHandle::enter(&mut provider, |storage| {
            let metadosis = MetadosisContract::new(storage);
            let limits = poc_schema_limits();
            let failed = metadosis
                .ocomp_fsm_state(fixture.first_wwd, &limits)
                .unwrap()
                .projection()
                .live_intent_id
                .unwrap();
            let survivor = metadosis
                .ocomp_fsm_state(fixture.later_wwd, &limits)
                .unwrap()
                .projection()
                .live_intent_id
                .unwrap();
            (failed, survivor, fixture.block_number + 65)
        });
    let scope = begin_recovery_scope_for_wwd(
        &mut provider,
        &fixture.scope,
        fixture.first_wwd,
        recovery_height,
    );
    run_lifecycle_begin(
        &mut provider,
        &scope,
        recovery_height,
        fixture.block_time + 65,
    )
    .expect("only the first missed WWD fails");

    assert_expired_job(&mut provider, failed_intent);
    StorageHandle::enter(&mut provider, |storage| {
        let limits = poc_schema_limits();
        let metadosis = MetadosisContract::new(storage.clone());
        assert_eq!(
            api::worldwide_day(storage, fixture.later_wwd)
                .unwrap()
                .unwrap()
                .status,
            WwdStatus::OffchainPending
        );
        let survivor = metadosis
            .ocomp_job_record(survivor_intent, &limits)
            .unwrap()
            .expect("survivor job remains queryable");
        assert_eq!(survivor.status, OcompJobStatus::AwaitingFinality);
        assert!(survivor.terminal.is_none());
    });
}

#[test]
fn emergency_failure_cannot_construct_the_reserved_failed_job_state() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);
    run_terminal_request(&mut provider, &fixture);
    let (intent_id, before_record) = live_intent(&mut provider, fixture.wwd);
    let recovery_height = fixture.block_number + 1;
    let scope = begin_recovery_scope(&mut provider, &fixture, recovery_height);
    let storage_before = provider.storage.clone();
    let events_before = provider.events.clone();

    let error = run_direct_failed_day_recovery(
        &mut provider,
        &scope,
        fixture.wwd,
        recovery_height,
        fixture.block_time + 1,
    )
    .expect_err("a live canonical job must be left for its deadline");

    assert!(matches!(error, PrecompileError::Fatal(_)));
    assert_eq!(provider.storage, storage_before);
    assert_eq!(provider.events, events_before);
    let (_, after_record) = live_intent(&mut provider, fixture.wwd);
    assert_eq!(after_record, before_record);
    assert_eq!(after_record.status, OcompJobStatus::AwaitingFinality);
    assert_eq!(after_record.terminal, None);
    assert_ne!(intent_id, B256::ZERO);
}

#[test]
fn malformed_persisted_request_receipt_remains_fatal_and_atomic() {
    let mut fixture = crate::fixture_kernel::ActivationFixture::new(91, 5_000, true);
    fixture.corrupt_request_receipt_mismatch();
    let before = fixture.rollback_snapshot();

    let error = fixture.apply().unwrap_err();

    assert!(matches!(error, PrecompileError::Fatal(_)));
    assert_eq!(fixture.rollback_snapshot(), before);
}

#[test]
fn a_carry_over_that_misses_the_request_limit_is_a_business_failure() {
    use crate::ocomp::activation::conserved_lysis_limit;

    assert_eq!(
        conserved_lysis_limit(U256::from(30), U256::from(10), U256::from(40)).unwrap(),
        U256::from(40)
    );
    for (allocation, unused) in [
        (U256::from(30), U256::from(9)),
        (U256::from(30), U256::from(11)),
        (U256::MAX, U256::ONE),
    ] {
        let error = conserved_lysis_limit(allocation, unused, U256::from(40)).unwrap_err();
        assert!(crate::errors::is_business_failure(&error), "{error}");
    }
}

fn seed_carry_over(provider: &mut HashMapStorageProvider, amount: U256) {
    StorageHandle::enter(provider, |storage| {
        PromisLimitContract::new(storage)
            .checked_add_carry_over(amount)
            .unwrap();
    });
}

fn promis_total(provider: &mut HashMapStorageProvider) -> U256 {
    StorageHandle::enter(provider, |storage| {
        PromisLimitContract::new(storage)
            .get_total_unallocated()
            .unwrap()
    })
}

fn request_receipt(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
) -> outbe_ocomp_protocol::receipts::RequestLimitSplitReceiptV1 {
    StorageHandle::enter(provider, |storage| {
        MetadosisContract::new(storage)
            .request_limit_receipt(wwd, &poc_schema_limits())
            .unwrap()
            .expect("requested day keeps its receipt")
    })
}

/// The failure receipt as the public view reports it, read after the aggregate validates it.
fn failure_view(
    provider: &mut HashMapStorageProvider,
    wwd: outbe_primitives::time::WorldwideDay,
) -> (U256, U256, U256) {
    StorageHandle::enter(provider, |storage| {
        api::worldwide_days(storage.clone()).expect("the aggregate accepts the failure receipt");
        let call = IMetadosis::getWorldwideDayTerminalReceiptCall { wwd: wwd.value() };
        let output =
            crate::precompile::dispatch(storage, &call.abi_encode(), Address::ZERO, U256::ZERO)
                .unwrap();
        let receipt =
            IMetadosis::getWorldwideDayTerminalReceiptCall::abi_decode_returns(&output).unwrap();
        assert_eq!(
            receipt.outcome,
            crate::schema::terminal_outcome::METADOSIS_FAILURE
        );
        (
            receipt.promisLimitReturnedMinor,
            receipt.promisLimitBeforeMinor,
            receipt.promisLimitAfterMinor,
        )
    })
}

#[test]
fn an_expired_green_day_returns_its_lysis_and_desis_limits_once() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);
    let carried = U256::from(500);
    seed_carry_over(&mut provider, carried);
    run_terminal_request(&mut provider, &fixture);
    let receipt = request_receipt(&mut provider, fixture.wwd);
    assert!(!receipt.desis_limit_minor.is_zero());
    let reserved = carried + receipt.carry_over_credit - receipt.desis_limit_minor;
    assert_eq!(
        promis_total(&mut provider),
        reserved,
        "the request takes the Desis Limit out of the accumulator"
    );

    let recovery_height = fixture.block_number + 65;
    let scope = begin_recovery_scope(&mut provider, &fixture, recovery_height);
    run_lifecycle_begin(
        &mut provider,
        &scope,
        recovery_height,
        fixture.block_time + 65,
    )
    .expect("missed finality deadline expires the day");

    let retained = receipt.lysis_limit_minor + receipt.desis_limit_minor;
    assert_failed_day_recovery(&mut provider, &fixture, carried + U256::from(100));
    assert_eq!(
        failure_view(&mut provider, fixture.wwd),
        (retained, reserved, reserved + retained)
    );

    run_lifecycle_begin(
        &mut provider,
        &scope,
        recovery_height + 1,
        fixture.block_time + 66,
    )
    .expect("closed failed WWD replay is a no-op");
    run_direct_failed_day_recovery(
        &mut provider,
        &scope,
        fixture.wwd,
        recovery_height + 2,
        fixture.block_time + 67,
    )
    .expect("failing a FAILED day again is a no-op");
    let request_height = recovery_height + 3;
    provider.set_block_number(request_height);
    provider.set_timestamp(U256::from(fixture.block_time + 68));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(request_height, fixture.block_time + 68, chain::CHAIN_ID),
            storage,
        );
        commands::run_ocomp_terminal_request(&ctx, &scope)
    })
    .expect("a FAILED day is not requested again");

    assert_failed_day_recovery(&mut provider, &fixture, carried + U256::from(100));
    assert_eq!(request_receipt(&mut provider, fixture.wwd), receipt);
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            MetadosisContract::new(storage)
                .terminal_intent_count(fixture.wwd)
                .unwrap(),
            1
        );
    });
}

#[test]
fn a_red_day_reserves_nothing_and_its_expiry_returns_only_lysis() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture =
        super::prepare_request_fixture_with_day_type(&mut provider, true, crate::WwdDayType::Red);
    let carried = U256::from(500);
    seed_carry_over(&mut provider, carried);
    run_terminal_request(&mut provider, &fixture);
    let receipt = request_receipt(&mut provider, fixture.wwd);
    assert_eq!(receipt.desis_limit_minor, U256::ZERO);
    let credited = carried + receipt.carry_over_credit;
    assert_eq!(promis_total(&mut provider), credited);

    let recovery_height = fixture.block_number + 65;
    let scope = begin_recovery_scope(&mut provider, &fixture, recovery_height);
    run_lifecycle_begin(
        &mut provider,
        &scope,
        recovery_height,
        fixture.block_time + 65,
    )
    .expect("missed finality deadline expires the day");

    assert_failed_day_recovery(&mut provider, &fixture, carried + U256::from(100));
    assert_eq!(
        failure_view(&mut provider, fixture.wwd),
        (
            receipt.lysis_limit_minor,
            credited,
            credited + receipt.lysis_limit_minor
        )
    );
}

#[test]
fn overlapping_requests_size_desis_from_what_earlier_requests_reserved() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = super::prepare_ready_days_fixture(&mut provider, true);
    let carried = U256::from(500);
    seed_carry_over(&mut provider, carried);
    for (block_number, block_time) in [
        (fixture.block_number, fixture.block_time),
        (fixture.block_number + 1, fixture.block_time + 1),
    ] {
        provider.set_block_number(block_number);
        provider.set_timestamp(U256::from(block_time));
        provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(block_number, block_time, chain::CHAIN_ID),
                storage,
            );
            commands::run_ocomp_terminal_request(&ctx, &fixture.scope)
        })
        .expect("overlapping request");
    }

    let first = request_receipt(&mut provider, fixture.first_wwd);
    let second = request_receipt(&mut provider, fixture.later_wwd);
    assert_eq!(
        first.desis_limit_minor,
        carried + first.carry_over_credit,
        "the first day reserves everything the accumulator offers"
    );
    assert_eq!(
        second.desis_limit_minor, second.carry_over_credit,
        "the second day finds only its own remainder"
    );
    assert_eq!(promis_total(&mut provider), U256::ZERO);

    let recovery_height = fixture.block_number + 65;
    let scope = begin_recovery_scope_for_wwd(
        &mut provider,
        &fixture.scope,
        fixture.first_wwd,
        recovery_height,
    );
    run_lifecycle_begin(
        &mut provider,
        &scope,
        recovery_height,
        fixture.block_time + 65,
    )
    .expect("only the first missed WWD fails");

    let returned = first.lysis_limit_minor + first.desis_limit_minor;
    assert_eq!(
        promis_total(&mut provider),
        returned,
        "the expired day returns its reservation while the live one keeps its own"
    );
    assert_eq!(
        failure_view(&mut provider, fixture.first_wwd),
        (returned, U256::ZERO, returned)
    );
    StorageHandle::enter(&mut provider, |storage| {
        assert_eq!(
            api::worldwide_day(storage, fixture.later_wwd)
                .unwrap()
                .unwrap()
                .status,
            WwdStatus::OffchainPending
        );
    });
}

/// A day whose limit exceeds its Lysis share, so its request both credits and reserves.
fn prepare_carried_request(
    carried: U256,
) -> (HashMapStorageProvider, super::PreparedRequestFixture) {
    use crate::fixture_kernel::FixtureKernelExt;

    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = prepare_request_fixture(&mut provider, true);
    StorageHandle::enter(&mut provider, |storage| {
        MetadosisContract::new(storage)
            .set_metadosis_limit(fixture.wwd, U256::from(2_000))
            .unwrap();
    });
    seed_carry_over(&mut provider, carried);
    (provider, fixture)
}

#[test]
fn a_failure_after_the_carry_over_credit_rolls_back_the_credit_and_the_desis_reservation() {
    let carried = U256::from(500);
    let (mut probe, fixture) = prepare_carried_request(carried);
    probe.fail_mutation_at_address(PROMIS_LIMIT_ADDRESS);
    assert!(matches!(
        try_terminal_request(&mut probe, &fixture),
        Err(PrecompileError::Storage(_))
    ));
    let credit_write = probe.clear_mutation_failure();

    let (mut clean, fixture) = prepare_carried_request(carried);
    run_terminal_request(&mut clean, &fixture);
    let receipt = request_receipt(&mut clean, fixture.wwd);
    assert!(!receipt.carry_over_credit.is_zero());
    assert!(!receipt.desis_limit_minor.is_zero());
    assert_eq!(
        promis_total(&mut clean),
        carried + receipt.carry_over_credit - receipt.desis_limit_minor
    );

    // The credit is the first Promis Limit write; the reservation is the very next mutation.
    for operation in [credit_write, credit_write + 1] {
        let (mut provider, fixture) = prepare_carried_request(carried);
        let storage_before = provider.storage.clone();
        let events_before = provider.get_ordered_events().to_vec();
        let ce_before = fixture.scope.ce_work_checkpoint().unwrap();
        provider.fail_after_mutation_at(operation);

        let error = try_terminal_request(&mut provider, &fixture)
            .expect_err("the injected failure fails the request");
        assert!(matches!(error, PrecompileError::Storage(_)), "{error}");
        assert_eq!(provider.clear_mutation_failure(), operation + 1);
        assert_eq!(provider.storage, storage_before, "storage at {operation}");
        assert_eq!(
            provider.get_ordered_events(),
            events_before.as_slice(),
            "events at {operation}"
        );
        assert_eq!(fixture.scope.ce_work_checkpoint().unwrap(), ce_before);
        assert_eq!(promis_total(&mut provider), carried);

        run_terminal_request(&mut provider, &fixture);
        assert_eq!(provider.storage, clean.storage, "retry at {operation}");
        assert_eq!(request_receipt(&mut provider, fixture.wwd), receipt);
    }
}

fn replay_closed_day(
    provider: &mut HashMapStorageProvider,
    fixture: &super::PreparedRequestFixture,
    scope: &ExecutionScope,
    height: u64,
    time: u64,
) {
    run_lifecycle_begin(provider, scope, height, time).expect("a closed day replays as a no-op");
    run_direct_failed_day_recovery(provider, scope, fixture.wwd, height + 1, time + 1)
        .expect("failing a FAILED day again is a no-op");
    provider.set_block_number(height + 2);
    provider.set_timestamp(U256::from(time + 2));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
    StorageHandle::enter(provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(height + 2, time + 2, chain::CHAIN_ID),
            storage,
        );
        commands::run_ocomp_terminal_request(&ctx, scope)
    })
    .expect("a FAILED day is not requested again");
}

#[test]
fn an_unresolved_day_fails_before_its_request_and_returns_its_whole_limit_once() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture = super::prepare_request_fixture_with_day_type(
        &mut provider,
        true,
        crate::WwdDayType::Unknown,
    );
    let carried = U256::from(500);
    let day_limit = U256::from(100);
    seed_carry_over(&mut provider, carried);
    let height = fixture.block_number + 1;
    let scope = begin_recovery_scope(&mut provider, &fixture, height);
    StorageHandle::enter(&mut provider, |storage| {
        preview_end_block(storage, &scope).unwrap()
    });
    provider.set_block_number(height);
    provider.set_timestamp(U256::from(fixture.block_time + 1));
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::OcompLifecycle);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(height, fixture.block_time + 1, chain::CHAIN_ID),
            storage,
        );
        crate::commands::run_ocomp_terminal_request(&ctx, &scope)
    })
    .expect("the business failure fails the day");

    let routed = (day_limit, carried, carried + day_limit);
    assert_failed_day_recovery(&mut provider, &fixture, carried + day_limit);
    assert_eq!(failure_view(&mut provider, fixture.wwd), routed);

    replay_closed_day(
        &mut provider,
        &fixture,
        &scope,
        height + 1,
        fixture.block_time + 2,
    );

    assert_failed_day_recovery(&mut provider, &fixture, carried + day_limit);
    assert_eq!(failure_view(&mut provider, fixture.wwd), routed);
    StorageHandle::enter(&mut provider, |storage| {
        let metadosis = MetadosisContract::new(storage);
        assert!(metadosis
            .request_limit_receipt(fixture.wwd, &poc_schema_limits())
            .unwrap()
            .is_none());
        assert_eq!(metadosis.terminal_intent_count(fixture.wwd).unwrap(), 0);
    });
}

#[test]
fn an_overflowed_day_is_requested_red_and_returns_its_whole_limit_once() {
    let mut provider = HashMapStorageProvider::new(chain::CHAIN_ID);
    let fixture =
        super::prepare_request_fixture_with_day_type(&mut provider, true, crate::WwdDayType::Red);
    // An overflowed day VWAP is never stored, so the day resolves red at a zero VWAP.
    StorageHandle::enter(&mut provider, |storage| {
        use crate::schema::WorldwideDayEntryExt;
        MetadosisContract::new(storage)
            .worldwide_days
            .entry(fixture.wwd)
            .current_vwap()
            .write(U256::ZERO)
            .unwrap();
    });
    let carried = U256::from(500);
    let day_limit = U256::from(100);
    seed_carry_over(&mut provider, carried);
    run_terminal_request(&mut provider, &fixture);
    let receipt = request_receipt(&mut provider, fixture.wwd);
    assert_eq!(receipt.desis_limit_minor, U256::ZERO);
    let credited = carried + receipt.carry_over_credit;
    assert_eq!(promis_total(&mut provider), credited);

    let recovery_height = fixture.block_number + 65;
    let scope = begin_recovery_scope(&mut provider, &fixture, recovery_height);
    run_lifecycle_begin(
        &mut provider,
        &scope,
        recovery_height,
        fixture.block_time + 65,
    )
    .expect("missed finality deadline expires the day");

    let routed = (
        receipt.lysis_limit_minor,
        credited,
        credited + receipt.lysis_limit_minor,
    );
    assert_failed_day_recovery(&mut provider, &fixture, carried + day_limit);
    assert_eq!(failure_view(&mut provider, fixture.wwd), routed);

    replay_closed_day(
        &mut provider,
        &fixture,
        &scope,
        recovery_height + 1,
        fixture.block_time + 66,
    );

    assert_failed_day_recovery(&mut provider, &fixture, carried + day_limit);
    assert_eq!(failure_view(&mut provider, fixture.wwd), routed);
    assert_eq!(request_receipt(&mut provider, fixture.wwd), receipt);
}
