use alloy_primitives::{B256, U256};
use outbe_ocomp_protocol::{
    profile::ProtocolBundleV1,
    test_utils::{minimal_capacity_profile, minimal_protocol_bundle},
};
use outbe_ocompregistry::{
    poc_schema_limits, OcompProtocolAuthorityV1, OcompRequestProfile, OcompSuccessorV1,
};

use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::constants::{MIN_ACTIVATION_BUFFER, PROTOCOL_VERSION};
use crate::handlers::UpgradeHandlerRegistry;
use crate::payload::ScheduleUpdatePayload;
use crate::schema::Update;
use crate::{encode_protocol_version, ProtocolVersion};
use outbe_primitives::error::{PrecompileError, Result};
use serde_json::Value;

mod events;
mod handlers;
mod lifecycle;
mod precompile;
mod records;
mod scheduled;
mod tee_fixture;
mod unset_version;
mod vote_dispatch;

static EMPTY_UPGRADE_HANDLER_REGISTRY: UpgradeHandlerRegistry = UpgradeHandlerRegistry::new(&[]);

// The fixtures install DCAP policies, so use a network that permits them.
pub(super) const TEST_CHAIN_ID: u64 = outbe_primitives::chain::MAINNET_CHAIN_ID;

/// Binary protocol version - safe to activate in tests.
pub(super) const PV: ProtocolVersion = PROTOCOL_VERSION;

pub(super) const V1_2: ProtocolVersion = encode_protocol_version(1, 2);
pub(super) const V1_3: ProtocolVersion = encode_protocol_version(1, 3);
pub(super) const V1_5: ProtocolVersion = encode_protocol_version(1, 5);
pub(super) const V2_0: ProtocolVersion = encode_protocol_version(2, 0);
pub(super) const V3_0: ProtocolVersion = encode_protocol_version(3, 0);
pub(super) const V3_1: ProtocolVersion = encode_protocol_version(3, 1);
pub(super) const V9_8: ProtocolVersion = encode_protocol_version(9, 8);

pub(super) fn with_update<F: FnOnce(StorageHandle)>(f: F) {
    let mut provider = HashMapStorageProvider::new(TEST_CHAIN_ID);
    let storage = StorageHandle::new(&mut provider);
    f(storage);
}

pub(super) fn with_update_provider<F: FnOnce(StorageHandle)>(f: F) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(TEST_CHAIN_ID);
    let storage = StorageHandle::new(&mut provider);
    f(storage);
    provider
}

pub(super) fn block_ctx(storage: StorageHandle, block_number: u64) -> BlockRuntimeContext {
    BlockRuntimeContext::new(
        BlockContext::empty_for_tests(block_number, 0, TEST_CHAIN_ID),
        storage,
    )
}

pub(super) fn min_activation(current: u64) -> u64 {
    current.saturating_add(MIN_ACTIVATION_BUFFER)
}

pub(super) fn ocomp_authority(genesis_hash: B256) -> OcompProtocolAuthorityV1 {
    let hash = B256::repeat_byte;
    let protocol_bundle = ProtocolBundleV1 {
        fork_id: hash(21),
        release_gate_authority_envelope_hash: hash(22),
        release_approval_policy_hash: hash(24),
        release_validator_command_artifact_hash: hash(25),
        migration_manifest_hash: hash(26),
        required_upgrade_handler_set_hash: hash(27),
        ..minimal_protocol_bundle()
    };
    let protocol_bundle_hash = protocol_bundle
        .protocol_bundle_hash(&poc_schema_limits())
        .unwrap();
    OcompProtocolAuthorityV1 {
        request_profile: OcompRequestProfile {
            chain_id: TEST_CHAIN_ID,
            genesis_hash,
            fork_id: protocol_bundle.fork_id,
            protocol_bundle_hash,
            correctness_profile_id: protocol_bundle.correctness_profile_id,
            capacity_profile: minimal_capacity_profile(),
            source_availability_policy_id: hash(44),
        },
        protocol_bundle,
    }
}

pub(super) fn ocomp_successor(genesis_hash: B256, activation_height: u64) -> OcompSuccessorV1 {
    let predecessor = ocomp_authority(genesis_hash);
    let mut protocol_bundle = predecessor.protocol_bundle.clone();
    protocol_bundle.protocol_version += 1;
    protocol_bundle.fork_id = B256::repeat_byte(61);
    protocol_bundle.request_semantics_version += 1;
    protocol_bundle.lysis_program_semantics_hash = B256::repeat_byte(62);
    let protocol_bundle_hash = protocol_bundle
        .protocol_bundle_hash(&poc_schema_limits())
        .unwrap();
    OcompSuccessorV1 {
        activation_height,
        predecessor_protocol_bundle_hash: predecessor.request_profile.protocol_bundle_hash,
        authority: OcompProtocolAuthorityV1 {
            request_profile: OcompRequestProfile {
                fork_id: protocol_bundle.fork_id,
                protocol_bundle_hash,
                correctness_profile_id: protocol_bundle.correctness_profile_id,
                ..predecessor.request_profile
            },
            protocol_bundle,
        },
    }
}

/// The block height from which most tests schedule an update.
pub(super) const SCHEDULE_HEIGHT: u64 = 100;

/// Schedules `proposal_id` for `version` at `activation` from
/// [`SCHEDULE_HEIGHT`], without release notes.
pub(super) fn schedule_version(
    update: &mut Update<'_>,
    proposal_id: U256,
    version: ProtocolVersion,
    activation: u64,
) -> Result<()> {
    schedule_update(
        update,
        proposal_id,
        ScheduleUpdatePayload::new(version, activation, ""),
        SCHEDULE_HEIGHT,
    )
}

/// Runs `f` on a new Update contract after it schedules proposal 1 for
/// `version` at the earliest activation height from [`SCHEDULE_HEIGHT`]. `f`
/// receives that activation height.
pub(super) fn with_scheduled_update<F: FnOnce(StorageHandle, &mut Update<'_>, u64)>(
    version: ProtocolVersion,
    f: F,
) {
    with_scheduled_release(version, "", f);
}

/// [`with_scheduled_update`] with the release notes `info`.
pub(super) fn with_scheduled_release<F: FnOnce(StorageHandle, &mut Update<'_>, u64)>(
    version: ProtocolVersion,
    info: &str,
    f: F,
) {
    with_update(|storage| scheduled_update(storage, version, info, f));
}

/// [`with_scheduled_release`] that returns the storage provider.
pub(super) fn scheduled_update_provider<F: FnOnce(StorageHandle, &mut Update<'_>, u64)>(
    version: ProtocolVersion,
    info: &str,
    f: F,
) -> HashMapStorageProvider {
    with_update_provider(|storage| scheduled_update(storage, version, info, f))
}

fn scheduled_update<F: FnOnce(StorageHandle, &mut Update<'_>, u64)>(
    storage: StorageHandle,
    version: ProtocolVersion,
    info: &str,
    f: F,
) {
    let mut update = Update::new(storage.clone());
    let activation = min_activation(SCHEDULE_HEIGHT);
    schedule_update(
        &mut update,
        U256::from(1),
        ScheduleUpdatePayload::new(version, activation, info),
        SCHEDULE_HEIGHT,
    )
    .unwrap();
    f(storage, &mut update, activation);
}

/// Schedules proposal 1 at the earliest activation height from
/// [`SCHEDULE_HEIGHT`] and proposal 2 at 500 blocks later, both for
/// [`PV`]. Returns both activation heights.
pub(super) fn schedule_early_and_late(update: &mut Update<'_>) -> (u64, u64) {
    let activation_early = min_activation(SCHEDULE_HEIGHT);
    let activation_late = activation_early + 500;
    schedule_version(update, U256::from(1), PV, activation_early).unwrap();
    schedule_version(update, U256::from(2), PV, activation_late).unwrap();
    (activation_early, activation_late)
}

/// Runs `f` on a new Update contract whose active version is `version` from
/// `height`.
pub(super) fn with_active_version<F: FnOnce(StorageHandle, &mut Update<'_>)>(
    version: ProtocolVersion,
    height: u64,
    f: F,
) {
    with_update(|storage| {
        let mut update = Update::new(storage.clone());
        update.set_active_version(version, height).unwrap();
        f(storage, &mut update);
    });
}

/// Asserts that the read helpers report `version` as the active version and
/// as the version activated at `height`.
pub(super) fn assert_active_version(
    storage: &StorageHandle,
    version: ProtocolVersion,
    height: u64,
) {
    assert_eq!(
        crate::api::get_active_version(storage.clone()).unwrap(),
        version
    );
    assert_eq!(
        crate::api::version_at_height(storage.clone(), height).unwrap(),
        version
    );
    assert!(crate::api::is_version_active_eq(storage.clone(), version).unwrap());
}

/// The status of the scheduled update `proposal_id`, which must exist.
pub(super) fn scheduled_status(
    update: &Update<'_>,
    proposal_id: U256,
) -> crate::schema::ScheduledUpdateStatus {
    update
        .read_scheduled_update(proposal_id)
        .unwrap()
        .unwrap()
        .status
}

pub(super) fn schedule_update(
    update: &mut Update<'_>,
    proposal_id: U256,
    payload: ScheduleUpdatePayload,
    current_height: u64,
) -> Result<()> {
    let encoded = serde_json::to_string(&payload).map_err(|error| {
        PrecompileError::Fatal(format!("schedule update JSON should serialize: {error}"))
    })?;
    let payload: Value = serde_json::from_str(&encoded).map_err(|error| {
        PrecompileError::Fatal(format!("schedule update JSON should parse: {error}"))
    })?;
    update.schedule_update_from_propose(proposal_id, &payload, current_height)
}

/// Test-only helper: runs begin-block processing with an empty handler registry.
pub(super) trait UpdateTestExt {
    fn process_begin_block_test(&mut self, block_number: u64) -> Result<()>;
}

impl UpdateTestExt for Update<'_> {
    fn process_begin_block_test(&mut self, block_number: u64) -> Result<()> {
        let ctx = block_ctx(self.storage.clone(), block_number);
        self.process_begin_block_with_handlers(&ctx, &EMPTY_UPGRADE_HANDLER_REGISTRY)
    }
}
