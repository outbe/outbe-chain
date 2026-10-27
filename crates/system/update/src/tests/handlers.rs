use super::tee_fixture::{initial_tee_policy, successor_tee_policy};
use super::TEST_CHAIN_ID;
use std::sync::atomic::{AtomicUsize, Ordering};

use alloy_primitives::{B256, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

use outbe_teeregistry::TeeRegistry;

use crate::api::get_active_version;
use crate::handlers::{UpgradeHandler, UpgradeHandlerRegistry};
use crate::schema::ScheduledUpdateStatus;
use crate::schema::Update;
use crate::state::ScheduledUpdateInfo;
use crate::ProtocolVersion;

use super::{
    block_ctx, schedule_update, scheduled_status, scheduled_update_provider, with_scheduled_update,
    PV,
};

static EMPTY_UPGRADE_HANDLER_REGISTRY: UpgradeHandlerRegistry = UpgradeHandlerRegistry::new(&[]);

static REGISTERED_HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);
static REPLAY_HANDLER_CALLS: AtomicUsize = AtomicUsize::new(0);

struct RegisteredCountingHandler;

impl UpgradeHandler for RegisteredCountingHandler {
    fn version(&self) -> ProtocolVersion {
        PV
    }

    fn label(&self) -> &'static str {
        "registered_counting_handler"
    }

    fn handle(&self, _ctx: &BlockRuntimeContext, _scheduled: &ScheduledUpdateInfo) -> Result<()> {
        REGISTERED_HANDLER_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct ReplayCountingHandler;

impl UpgradeHandler for ReplayCountingHandler {
    fn version(&self) -> ProtocolVersion {
        PV
    }

    fn label(&self) -> &'static str {
        "replay_counting_handler"
    }

    fn handle(&self, _ctx: &BlockRuntimeContext, _scheduled: &ScheduledUpdateInfo) -> Result<()> {
        REPLAY_HANDLER_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct FailingHandler;

impl UpgradeHandler for FailingHandler {
    fn version(&self) -> ProtocolVersion {
        PV
    }

    fn label(&self) -> &'static str {
        "failing_handler"
    }

    fn handle(&self, _ctx: &BlockRuntimeContext, _scheduled: &ScheduledUpdateInfo) -> Result<()> {
        Err(PrecompileError::Fatal("handler failed".into()))
    }
}

static REGISTERED_COUNTING_HANDLER: RegisteredCountingHandler = RegisteredCountingHandler;
static REPLAY_COUNTING_HANDLER: ReplayCountingHandler = ReplayCountingHandler;
static FAILING_HANDLER: FailingHandler = FailingHandler;

static REGISTERED_HANDLER_REGISTRY: UpgradeHandlerRegistry =
    UpgradeHandlerRegistry::new(&[&REGISTERED_COUNTING_HANDLER]);

static REPLAY_HANDLER_REGISTRY: UpgradeHandlerRegistry =
    UpgradeHandlerRegistry::new(&[&REPLAY_COUNTING_HANDLER]);

static FAILING_HANDLER_REGISTRY: UpgradeHandlerRegistry =
    UpgradeHandlerRegistry::new(&[&FAILING_HANDLER]);

fn stage_policy_release(
    provider: &mut HashMapStorageProvider,
    proposal_id: U256,
    current: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
    successor: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) {
    StorageHandle::enter(provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        registry.install_initial_policy_v1(current).unwrap();
        schedule_update(
            &mut Update::new(storage.clone()),
            proposal_id,
            crate::ScheduleUpdatePayload::new(PV, successor.activation_height, "TEE release"),
            1,
        )
        .unwrap();
        Update::new(storage)
            .live_plans
            .create(&crate::schema::UpdatePlanRecord {
                proposal_id,
                activation_height: successor.activation_height,
                replace_end: successor.activation_height - 100,
                mrenclave: successor.measurement_rules[0].mrenclave,
                prepared: true,
            })
            .unwrap();
        registry
            .stage_successor_policy_v1(proposal_id, successor)
            .unwrap();
    });
}

#[test]
fn activation_without_handler_succeeds() {
    with_scheduled_update(PV, |storage, update, activation| {
        let proposal_id = U256::from(1);

        let ctx = block_ctx(storage.clone(), activation);
        update
            .process_begin_block_with_handlers(&ctx, &EMPTY_UPGRADE_HANDLER_REGISTRY)
            .unwrap();

        assert_eq!(
            scheduled_status(update, proposal_id),
            ScheduledUpdateStatus::Activated
        );
        assert_eq!(get_active_version(storage).unwrap(), PV);
    });
}

#[test]
fn software_update_activation_promotes_its_staged_tee_policy() {
    let genesis_hash = B256::repeat_byte(0x60);
    let proposal_id = U256::from(11);
    let activation = 101;
    let current = initial_tee_policy(genesis_hash, B256::repeat_byte(0x65), 0x61);
    let successor = successor_tee_policy(&current, activation, B256::repeat_byte(0x66), 0x61);
    let mut provider = HashMapStorageProvider::new_with_chain_identity(TEST_CHAIN_ID, genesis_hash);
    provider.set_block_number(1);
    stage_policy_release(&mut provider, proposal_id, &current, &successor);

    provider.set_block_number(activation);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = block_ctx(storage.clone(), activation);
        Update::new(storage.clone())
            .process_begin_block_with_handlers(&ctx, &EMPTY_UPGRADE_HANDLER_REGISTRY)
            .unwrap();
        assert_eq!(
            TeeRegistry::new(storage).active_policy_v1().unwrap(),
            successor
        );
    });
}

#[test]
fn handler_failure_rolls_back_tee_policy_promotion_with_update_activation() {
    let genesis_hash = B256::repeat_byte(0x67);
    let proposal_id = U256::from(12);
    let activation = 101;
    let current = initial_tee_policy(genesis_hash, B256::repeat_byte(0x68), 0x61);
    let successor = successor_tee_policy(&current, activation, B256::repeat_byte(0x69), 0x61);
    let mut provider = HashMapStorageProvider::new_with_chain_identity(TEST_CHAIN_ID, genesis_hash);
    provider.set_block_number(1);
    stage_policy_release(&mut provider, proposal_id, &current, &successor);

    provider.set_block_number(activation);
    StorageHandle::enter(&mut provider, |storage| {
        let ctx = block_ctx(storage.clone(), activation);
        assert!(matches!(
            Update::new(storage.clone())
                .process_begin_block_with_handlers(&ctx, &FAILING_HANDLER_REGISTRY),
            Err(PrecompileError::Fatal(_))
        ));
        let registry = TeeRegistry::new(storage.clone());
        assert_eq!(registry.active_policy_v1().unwrap(), current);
        assert_eq!(
            registry.staged_successor_policy_v1().unwrap(),
            Some((proposal_id, successor))
        );
        assert_eq!(
            Update::new(storage)
                .read_scheduled_update(proposal_id)
                .unwrap()
                .unwrap()
                .status,
            ScheduledUpdateStatus::Scheduled
        );
    });
}

#[test]
fn registered_handler_is_called_before_activation() {
    REGISTERED_HANDLER_CALLS.store(0, Ordering::SeqCst);
    with_scheduled_update(PV, |storage, update, activation| {
        let proposal_id = U256::from(1);

        let ctx = block_ctx(storage.clone(), activation);
        update
            .process_begin_block_with_handlers(&ctx, &REGISTERED_HANDLER_REGISTRY)
            .unwrap();

        assert_eq!(REGISTERED_HANDLER_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(
            scheduled_status(update, proposal_id),
            ScheduledUpdateStatus::Activated
        );
        assert_eq!(get_active_version(storage).unwrap(), PV);
    });
}

#[test]
fn handler_failure_is_fatal_and_leaves_update_unactivated() {
    with_scheduled_update(PV, |storage, update, activation| {
        let proposal_id = U256::from(1);

        let ctx = block_ctx(storage.clone(), activation);
        let err = update
            .process_begin_block_with_handlers(&ctx, &FAILING_HANDLER_REGISTRY)
            .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::Fatal(message) if message.contains("handler failed")
        ));

        assert_eq!(
            scheduled_status(update, proposal_id),
            ScheduledUpdateStatus::Scheduled
        );
        assert_ne!(get_active_version(storage).unwrap(), PV);
    });
}

#[test]
fn activated_update_does_not_reinvoke_handler_on_replay() {
    REPLAY_HANDLER_CALLS.store(0, Ordering::SeqCst);
    let provider = scheduled_update_provider(PV, "", |storage, update, activation| {
        let ctx = block_ctx(storage.clone(), activation);
        update
            .process_begin_block_with_handlers(&ctx, &REPLAY_HANDLER_REGISTRY)
            .unwrap();
        update
            .process_begin_block_with_handlers(&ctx, &REPLAY_HANDLER_REGISTRY)
            .unwrap();
    });

    assert_eq!(REPLAY_HANDLER_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(
        event_count(
            &provider,
            crate::precompile::IUpdate::UpgradeActivated::SIGNATURE_HASH,
        ),
        1
    );
}

fn event_count(
    provider: &outbe_primitives::storage::hashmap::HashMapStorageProvider,
    topic0: alloy_primitives::B256,
) -> usize {
    use outbe_primitives::addresses::UPDATE_ADDRESS;
    provider
        .get_events(UPDATE_ADDRESS)
        .iter()
        .filter(|log| log.topics().first() == Some(&topic0))
        .count()
}
