//! Lifecycle-driven e2e for the WWD -> Tribute -> Lysis -> NOD -> mine_gratis -> GRATIS flow.
//!
//! Each tick runs the full Outbe pre-execution hook chain through
//! `outbe_evm::executor::run_outbe_pre_execution_hooks` — the same helper the
//! real `OutbeBlockExecutor::apply_pre_execution_changes` uses. That covers, in
//! production order:
//!   1. Genesis-state validation (skipped here — we pass `None`);
//!   2. `EmissionLimitLifecycle::begin_block` (writes per-block emission input
//!      and dispatches it into Validator/AgentReward/Metadosis sinks);
//!   3. Validator-set epoch boundary (no-op here — no validator set seeded,
//!      `is_epoch_boundary` returns false on unconfigured state);
//!   4. `MetadosisLifecycle::begin_block` — WWD state machine + lysis;
//!   5. Staking matured-unbonding processing (no-op without stakers);
//!   6. `OracleLifecycle::begin_block` — tally + daily S-curve.
//!
//! Oracle slash-window penalties run after begin-zone system phases and before user txs.
//!
//! User-triggered `mineGratis` goes through the NodFactory precompile
//! (`outbe_nodfactory::precompile::dispatch_with_reader`) so the atomic burn-of-Nod +
//! `Gratis::mine` wiring inside the dispatcher is exercised, not duplicated.
//! The Nod precompile (0x1006) is read-only after the Nod/NodFactory split.
//!
//! Bucket qualification is oracle-driven: after lysis issues a NOD the bucket
//! starts UNQUALIFIED, and `NodLifecycle::begin_block` promotes it once the
//! COEN/0xUSD exchange rate rises strictly above `bucket.floor_price_minor`
//! (a rate exactly equal to the floor leaves it unqualified). The test seeds
//! the rate via `seed_exchange_rate(...)` between lysis and mining and asserts
//! the bit flips after the next tick.
//!
//! What is still bypassed in this test:
//!   - NOD `cost_amount_minor` payment goes through the precompile's new
//!     `IERC20.transferFrom` / `IERC20.approve` /
//!     `IVaultProvider.depositLiquidity` sequence, but the storage provider
//!     stubs all sub-calls via `enable_sub_call_stub()`. The miner's balance
//!     is not debited and no real vault deposit occurs — vault-side wiring
//!     is covered separately.
//!   - An explicit `metadosis::emission_sink::apply(...)` call per day on top
//!     of `EmissionLimitLifecycle`'s per-block emission, so day limits are
//!     large enough to fund the test tributes deterministically. Exercising
//!     the full emission schedule end-to-end is out of scope here.
//!   - Reth payload building, state-root computation, and txpool admission
//!     (we drive only the pre-execution hook phase, not the full executor).

use std::{collections::HashMap, sync::Arc};

use alloy_primitives::{address, keccak256, Address, Bytes, Log, B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    begin_block, derive_poseidon_entity_id, end_block, CandidateCacheLimits, CeMdbx, CeWorkConfig,
    CompressedTreeService, EntityId36, EnvironmentIdentity, ExactParentIdentity, ExecutionScope,
    FinalizedMarker, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_gratis::Gratis;
use outbe_metadosis::{
    constants::{
        FORMING_PERIOD_HOURS, LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS, SECONDS_PER_HOUR,
        WAITING_PERIOD_HOURS,
    },
    emission_sink,
    runtime::date_key_to_timestamp,
    schema::{day_type, status, MetadosisContract},
};
use outbe_nod::api as nod_api;
use outbe_nodfactory::{
    precompile::{dispatch as nodfactory_dispatch, INodFactory},
    runtime as nodfactory_runtime,
};
use outbe_offchain_data::{
    FinalizedBlock, FinalizedLog, FinalizedReceipt, OffchainDataProjection, ProjectionConfig,
    RuntimeBodyReaders,
};
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle, StorageWriterHandle};
use outbe_oracle::{
    contract::OracleContract,
    logic::{init_from_genesis, OracleGenesisConfig},
};
use outbe_primitives::addresses::VAULT_PROVIDER_ADDRESS;
use outbe_primitives::units::Units;
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::{TributeContract, TributeData};

// Mainnet-style id — effective_hours() returns DEFAULT_LOOKBACK / DEFAULT_OFFERING
// and bootstrap init is skipped.
const CHAIN_ID: u64 = 1;

// Dummy asset address passed into `mineGratis`. The provider has
// `enable_sub_call_stub()` flipped on, so the resulting `IERC20.transferFrom` /
// `IERC20.approve` sub-calls return `default_success()` without touching real
// ERC20 state. The e2e exercises the lysis → nod → gratis pipeline; vault-side
// behavior is covered separately.
const MINE_GRATIS_ASSET: Address = address!("0x000000000000000000000000000000000000A11C");

struct WwdPhases {
    forming_end: u64,
    offering_entry: u64,
    offering_end: u64,
    scheduled: u64,
}

struct BodyProjectionHarness {
    storage: Arc<MemoryStorage>,
    projector: OffchainDataProjection,
    projected_events: usize,
    next_block: u64,
    _tree_directory: tempfile::TempDir,
    tree_service: Arc<CompressedTreeService>,
}

impl BodyProjectionHarness {
    fn new() -> Self {
        let storage = Arc::new(MemoryStorage::new());
        let genesis_hash = B256::repeat_byte(0x11);
        let reader: StorageReaderHandle = storage.clone();
        let writer: StorageWriterHandle = storage.clone();
        let projector = OffchainDataProjection::open(
            ProjectionConfig {
                chain_id: CHAIN_ID,
                genesis_hash,
                start_block: 1,
            },
            reader,
            writer,
        )
        .unwrap();
        let tree_directory = tempfile::tempdir().unwrap();
        let tree_db = CeMdbx::open(
            tree_directory.path(),
            EnvironmentIdentity {
                local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
                chain_id: CHAIN_ID,
                genesis_hash,
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                shard_count: outbe_compressed_entities::K_TARGET,
                tree_format: "ckb-smt-v0.6.1-poseidon-sharded-v2".to_owned(),
                vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
            },
            FinalizedMarker {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                height: 0,
                block_hash: genesis_hash,
                parent_block_hash: B256::ZERO,
                parent_root: B256::ZERO,
                new_root: outbe_compressed_entities::empty_shard_top_root(
                    outbe_compressed_entities::K_TARGET,
                )
                .unwrap(),
            },
        )
        .unwrap();
        let tree_service = Arc::new(
            CompressedTreeService::new(
                tree_db,
                CandidateCacheLimits {
                    max_candidates: 4,
                    max_encoded_bytes: 10_000_000,
                },
            )
            .unwrap(),
        );
        Self {
            storage,
            projector,
            projected_events: 0,
            next_block: 1,
            _tree_directory: tree_directory,
            tree_service,
        }
    }

    fn runtime_readers(&self) -> RuntimeBodyReaders {
        RuntimeBodyReaders::new(self.storage.clone())
    }

    fn project_completed_block(&mut self, provider: &HashMapStorageProvider) {
        let events = provider.get_ordered_events();
        if events.len() == self.projected_events {
            return;
        }
        let logs = events[self.projected_events..]
            .iter()
            .enumerate()
            .map(|(index, log)| FinalizedLog {
                log_index: u64::try_from(index).unwrap(),
                emitter: log.address,
                data: log.data.clone(),
            })
            .collect();
        let number = self.next_block;
        let hash = keccak256(number.to_be_bytes());
        self.projector
            .project_block(&FinalizedBlock {
                number,
                hash,
                receipts: vec![FinalizedReceipt {
                    tx_hash: keccak256([number.to_be_bytes(), 1_u64.to_be_bytes()].concat()),
                    transaction_index: 0,
                    success: true,
                    logs,
                }],
            })
            .unwrap();
        self.projected_events = events.len();
        self.next_block += 1;
    }
}

fn phases_for(wwd: WorldwideDay) -> WwdPhases {
    let forming_start = wwd.start_timestamp();
    let forming_end = forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
    let offering_entry = forming_end + LOOKBACK_DELAY_HOURS * SECONDS_PER_HOUR;
    let offering_end = offering_entry + OFFERING_PERIOD_HOURS * SECONDS_PER_HOUR;
    let scheduled = offering_end + WAITING_PERIOD_HOURS * SECONDS_PER_HOUR;
    WwdPhases {
        forming_end,
        offering_entry,
        offering_end,
        scheduled,
    }
}

/// Timestamp inside the WWD's FORMING window whose UTC date equals the WWD key.
/// `emission_sink::apply(ctx, amount)` keys by `timestamp_to_date_key(ctx.timestamp)` (UTC),
/// while `process_metadosis(wwd)` reads by UTC+14 WWD; using this timestamp keeps both aligned.
fn emission_timestamp(wwd: WorldwideDay) -> u64 {
    date_key_to_timestamp(u32::from(wwd)) + SECONDS_PER_HOUR
}

fn build_ctx<'s>(
    storage: StorageHandle<'s>,
    block_number: u64,
    timestamp: u64,
) -> BlockRuntimeContext<'s> {
    BlockRuntimeContext::new(
        BlockContext::empty_for_tests(block_number, timestamp, CHAIN_ID),
        storage,
    )
}

fn with_storage<R>(
    provider: &mut HashMapStorageProvider,
    operation: impl FnOnce(StorageHandle<'_>) -> R,
) -> R {
    StorageHandle::enter(provider, operation)
}

fn with_body_lifecycle<R>(
    provider: &mut HashMapStorageProvider,
    bodies: &mut BodyProjectionHarness,
    operation: impl FnOnce(StorageHandle<'_>, &ExecutionScope, &RuntimeBodyReaders) -> R,
) -> R {
    let parent = bodies.runtime_readers();
    let marker = bodies.tree_service.finalized_marker().unwrap();
    let block_number = marker.height + 1;
    provider.set_block_number(block_number);
    if marker.height == 0 {
        StorageHandle::enter(provider, |storage| {
            storage
                .sstore(
                    outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                    U256::ZERO,
                    U256::from(2_u64),
                )
                .unwrap();
            storage
                .sstore(
                    outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                    U256::from(1_u64),
                    U256::from_be_bytes(marker.new_root.0),
                )
                .unwrap();
        });
    }
    let parent_tree = bodies
        .tree_service
        .open_parent(ExactParentIdentity {
            commitment_scheme_version: marker.commitment_scheme_version,
            block_number: marker.height,
            block_hash: marker.block_hash,
            root: marker.new_root,
        })
        .unwrap();
    let scope = ExecutionScope::with_parent_tree(parent_tree, CeWorkConfig::new(0, 0, u64::MAX));
    let (result, seal) = StorageHandle::enter(provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        let result = operation(storage.clone(), &scope, &parent);
        let seal = end_block(storage, &scope).unwrap();
        (result, seal)
    });
    let block_hash = keccak256([b"ce-test-block".as_slice(), &block_number.to_be_bytes()].concat());
    bodies
        .tree_service
        .publish_candidate(block_hash, seal.staged_tree_batch)
        .unwrap();
    bodies
        .tree_service
        .apply_finalized(block_number, block_hash, seal.new_root)
        .unwrap();
    result
}

/// One begin-block tick running the full Outbe pre-execution hook chain, in
/// the same order as `OutbeBlockExecutor::apply_pre_execution_changes`
/// (executor.rs), followed by an explicit `start_metadosis` call.
///
/// the WWD state machine and `process_metadosis` no longer run
/// from the executor's pre-execution hook chain — they were moved to the
/// daily Cycle handler at UTC midnight. To preserve this test's per-block
/// state-machine driving (which deliberately exercises lookback-delay /
/// offering / completion transitions sub-day), we invoke
/// `outbe_metadosis::runtime::start_metadosis` directly here.
///
/// An optional `emission` is written to Metadosis's day-limit sink first so
/// the day has a deterministic budget; this keeps the test's processing
/// outcome stable without depending on the daily Cycle handler's exact
/// allocation math.
fn tick(
    provider: &mut HashMapStorageProvider,
    bodies: &mut BodyProjectionHarness,
    block_number: u64,
    timestamp: u64,
    emission: Option<U256>,
) {
    with_body_lifecycle(provider, bodies, |storage, scope, parent| {
        // Mirror the block timestamp into the HashMap provider so precompiles that
        // read `self.storage.timestamp()` see the simulated block time advance with each tick.
        storage.set_block_timestamp(U256::from(timestamp)).unwrap();
        let ctx = build_ctx(storage.clone(), block_number, timestamp);
        if let Some(amount) = emission {
            emission_sink::apply(&ctx, amount).unwrap();
        }
        outbe_evm::executor::run_outbe_pre_execution_hooks_with_readers(&ctx, None, parent, scope)
            .unwrap();
        outbe_nod::hooks::qualify_nods(&ctx, scope, parent).unwrap();
        outbe_metadosis::runtime::start_metadosis(&ctx, scope, parent).unwrap();
    });
    bodies.project_completed_block(provider);
}

fn init_oracle(storage: StorageHandle) -> u32 {
    let mut oracle = OracleContract::new(storage);
    let config = OracleGenesisConfig {
        settlement_currencies: vec![(840, "0xUSD".into(), "COEN".into(), "0xUSD".into())],
        ..OracleGenesisConfig::default_config()
    };
    init_from_genesis(&mut oracle, &config).unwrap();
    oracle.get_pair_id("COEN", "0xUSD").unwrap()
}

/// Store a VWAP snapshot for the previous WWD so day-type inference on the
/// FORMING->LOOKBACK transition has a baseline to compare against.
fn seed_previous_wwd_vwap(
    storage: StorageHandle,
    pair_id: u32,
    previous_wwd: WorldwideDay,
    vwap: U256,
) {
    let mut oracle = OracleContract::new(storage);
    let previous_forming_start = previous_wwd.start_timestamp();
    let previous_forming_end = previous_forming_start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
    oracle
        .write_snapshot(
            previous_forming_start + SECONDS_PER_HOUR,
            &[(pair_id, vwap, U256::from(1u64))],
        )
        .unwrap();
    oracle
        .store_worldwide_day_vwap_snapshot(
            previous_wwd,
            previous_forming_start,
            previous_forming_end,
        )
        .unwrap();
}

/// Write a VWAP sample inside the target WWD's FORMING window; the status
/// machine will auto-store it as the day's VWAP on FORMING->LOOKBACK.
fn seed_current_wwd_vwap(storage: StorageHandle, pair_id: u32, wwd: WorldwideDay, vwap: U256) {
    let mut oracle = OracleContract::new(storage);
    let forming_start = wwd.start_timestamp();
    oracle
        .write_snapshot(
            forming_start + SECONDS_PER_HOUR,
            &[(pair_id, vwap, U256::from(1u64))],
        )
        .unwrap();
}

/// System write of the COEN/0xUSD exchange rate used by `NodLifecycle` to
/// qualify buckets whose `floor_price_minor < rate` (strict). In production this rate
/// comes from validator vote tally at vote-period boundaries; the test writes
/// it directly as a deterministic substitute.
fn seed_exchange_rate(storage: StorageHandle, rate: U256) {
    let mut oracle = OracleContract::new(storage);
    oracle
        .set_exchange_rate(Address::ZERO, "COEN", "0xUSD", rate, 0, 0)
        .unwrap();
}

/// Pre-create the WWD record with explicit DEFAULT hours so begin_block's
/// `create_worldwide_day_if_needed` becomes a no-op for this day and we control
/// the schedule deterministically.
fn pre_create_wwd(storage: StorageHandle, wwd: WorldwideDay) {
    let mut metadosis = MetadosisContract::new(storage.clone());
    let forming_start = wwd.start_timestamp();
    metadosis
        .create_worldwide_day(
            wwd,
            forming_start,
            LOOKBACK_DELAY_HOURS,
            OFFERING_PERIOD_HOURS,
        )
        .unwrap();
    metadosis.add_active_wwd(wwd).unwrap();
    let mut tribute = TributeContract::new(storage);
    tribute.seal_day(wwd).unwrap();
}

fn find_valid_nonce(nod_id: EntityId36) -> U256 {
    for n in 0u64..100_000 {
        let nonce = U256::from(n);
        if nodfactory_runtime::validate_pow(nod_id, nonce).is_ok() {
            return nonce;
        }
    }
    panic!("couldn't find valid nonce in 100k attempts");
}

/// User-triggered mining via the production NOD precompile.
///
/// The dispatcher runs PoW validation, bucket qualification check, noop settlement
/// hook, NOD burn, and `Gratis::mine` as one atomic handler. The caller is
/// responsible for first seeding an exchange rate high enough that
/// `NodLifecycle::begin_block` qualified the bucket; `mine_gratis` itself does
/// not query oracle.
fn mine_via_precompile(
    provider: &mut HashMapStorageProvider,
    bodies: &mut BodyProjectionHarness,
    owner: Address,
) -> U256 {
    let mined = with_body_lifecycle(provider, bodies, |storage, scope, parent| {
        let nods = nod_api::list_by_owner(&storage, scope, parent, owner).unwrap();
        assert_eq!(nods.len(), 1, "expected exactly one NOD for {owner}");
        let item = nods.into_iter().next().unwrap();

        let nonce = find_valid_nonce(item.nod_id);
        let balance_before = Gratis::new(storage.clone()).balance_of(owner).unwrap();

        let call = INodFactory::mineGratisCall {
            nodId: Bytes::copy_from_slice(item.nod_id.as_bytes()),
            nonce,
            asset: MINE_GRATIS_ASSET,
        };
        let output = nodfactory_dispatch(
            storage.clone(),
            scope,
            parent,
            &call.abi_encode(),
            owner,
            U256::ZERO,
        )
        .unwrap();
        let mined = INodFactory::mineGratisCall::abi_decode_returns(&output).unwrap();
        assert_eq!(mined, item.gratis_load_minor);

        assert!(nod_api::get_item(&storage, scope, parent, item.nod_id)
            .unwrap()
            .is_none());
        let balance_after = Gratis::new(storage).balance_of(owner).unwrap();
        assert_eq!(balance_after, balance_before + mined);
        mined
    });
    bodies.project_completed_block(provider);
    mined
}

#[derive(Debug, Eq, PartialEq)]
struct ScenarioOutcome {
    consensus_storage: HashMap<(Address, U256), U256>,
    events: Vec<Log>,
    native_balances: [U256; 3],
    gratis_balances: [U256; 3],
    projected_carol_nod: (EntityId36, U256, B256),
    projected_carol_bucket: (bool, u64),
}

fn run_green_then_red_wwd_lysis_nod_mine_gratis() -> ScenarioOutcome {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let mut bodies = BodyProjectionHarness::new();
    // The NOD-cost payment branch deposits into the vault provider via an EVM
    // sub-call to VAULT_PROVIDER_ADDRESS. enable_sub_call_stub covers the ERC-20
    // legs; the provider is stubbed to return a decodable uint256 (shares).
    provider.enable_sub_call_stub();
    provider.stub_sub_call_at(VAULT_PROVIDER_ADDRESS, Bytes::from(vec![0u8; 32]));
    // Pick non-adjacent WWDs so each day's full ~24-day lifecycle does not
    // accidentally interleave with the other's.
    let green_wwd = WorldwideDay::new(20241221);
    let red_wwd = WorldwideDay::new(20241222);

    let green = phases_for(green_wwd);
    let red = phases_for(red_wwd);

    let alice = address!("0x1111111111111111111111111111111111111111");
    let bob = address!("0x2222222222222222222222222222222222222222");
    let carol = address!("0x3333333333333333333333333333333333333333");

    let pair_id = with_storage(&mut provider, init_oracle);

    with_storage(&mut provider, |storage| {
        // GREEN: current VWAP > previous -> GREEN day_type.
        seed_previous_wwd_vwap(
            storage.clone(),
            pair_id,
            green_wwd.previous_date_key(),
            U256::from(100u64),
        );
        seed_current_wwd_vwap(storage.clone(), pair_id, green_wwd, U256::from(150u64));

        // RED: current VWAP <= previous -> RED day_type.
        seed_previous_wwd_vwap(
            storage.clone(),
            pair_id,
            red_wwd.previous_date_key(),
            U256::from(200u64),
        );
        seed_current_wwd_vwap(storage.clone(), pair_id, red_wwd, U256::from(100u64));

        pre_create_wwd(storage.clone(), green_wwd);
        pre_create_wwd(storage.clone(), red_wwd);
    });

    let green_day_limit = U256::in_units(500_000u64);
    let red_day_limit = U256::in_units(5_000u64);

    // Ticks are ordered by ascending timestamp; both days progress in parallel.
    tick(
        &mut provider,
        &mut bodies,
        1,
        emission_timestamp(green_wwd),
        Some(green_day_limit),
    );
    tick(
        &mut provider,
        &mut bodies,
        2,
        emission_timestamp(red_wwd),
        Some(red_day_limit),
    );

    // GREEN FORMING -> LOOKBACK: VWAPs captured, day_type inferred.
    tick(&mut provider, &mut bodies, 3, green.forming_end, None);
    with_storage(&mut provider, |storage| {
        let m = MetadosisContract::new(storage);
        assert_eq!(m.get_wwd_status(green_wwd).unwrap(), status::LOOKBACK_DELAY);
        assert_eq!(m.get_wwd_day_type(green_wwd).unwrap(), day_type::GREEN);
    });

    // RED FORMING -> LOOKBACK.
    tick(&mut provider, &mut bodies, 4, red.forming_end, None);
    with_storage(&mut provider, |storage| {
        let m = MetadosisContract::new(storage);
        assert_eq!(m.get_wwd_status(red_wwd).unwrap(), status::LOOKBACK_DELAY);
        assert_eq!(m.get_wwd_day_type(red_wwd).unwrap(), day_type::RED);
    });

    // GREEN -> OFFERING: tribute unsealed by status machine; issue alice's tribute.
    tick(&mut provider, &mut bodies, 5, green.offering_entry, None);
    let green_tribute_id = derive_poseidon_entity_id(alice, green_wwd).unwrap();
    let green_nominal = U256::in_units(1_000_000u64);
    with_body_lifecycle(&mut provider, &mut bodies, |storage, scope, parent| {
        TributeContract::new(storage)
            .issue(
                scope,
                parent,
                &TributeData {
                    tribute_id: green_tribute_id,
                    owner: alice,
                    worldwide_day: green_wwd,
                    issuance_amount_minor: green_nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: green_nominal,
                    reference_currency: 840,
                    exclude_from_intex_issuance: false,
                    tribute_price_minor: U256::ZERO,
                },
            )
            .unwrap();
    });
    bodies.project_completed_block(&provider);

    // RED -> OFFERING; issue bob's small and carol's large tribute.
    tick(&mut provider, &mut bodies, 6, red.offering_entry, None);
    let red_small_tribute_id = derive_poseidon_entity_id(bob, red_wwd).unwrap();
    let red_large_tribute_id = derive_poseidon_entity_id(carol, red_wwd).unwrap();
    let red_small_nominal = U256::in_units(20u64);
    let red_large_nominal = U256::in_units(1_000u64);
    with_body_lifecycle(&mut provider, &mut bodies, |storage, scope, parent| {
        let mut tribute = TributeContract::new(storage);
        tribute
            .issue(
                scope,
                parent,
                &TributeData {
                    tribute_id: red_small_tribute_id,
                    owner: bob,
                    worldwide_day: red_wwd,
                    issuance_amount_minor: red_small_nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: red_small_nominal,
                    reference_currency: 840,
                    exclude_from_intex_issuance: false,
                    tribute_price_minor: U256::ZERO,
                },
            )
            .unwrap();
        tribute
            .issue(
                scope,
                parent,
                &TributeData {
                    tribute_id: red_large_tribute_id,
                    owner: carol,
                    worldwide_day: red_wwd,
                    issuance_amount_minor: red_large_nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: red_large_nominal,
                    reference_currency: 840,
                    exclude_from_intex_issuance: false,
                    // Keep the two RED tributes in distinct NOD buckets so the
                    // qualification assertions below exercise both outcomes.
                    tribute_price_minor: U256::from(10_000u64),
                },
            )
            .unwrap();
    });
    bodies.project_completed_block(&provider);

    // GREEN OFFERING -> WAITING; RED stays OFFERING.
    tick(&mut provider, &mut bodies, 7, green.offering_end, None);

    // GREEN crosses WAITING -> READY and is processed; RED enters WAITING.
    tick(&mut provider, &mut bodies, 8, red.offering_end, None);
    with_storage(&mut provider, |storage| {
        let m = MetadosisContract::new(storage);
        assert_eq!(m.get_wwd_status(green_wwd).unwrap(), status::COMPLETED);
        assert_eq!(m.get_wwd_day_type(green_wwd).unwrap(), day_type::GREEN);
    });

    // The canonical bodies are now available only through the projected repository.
    assert!(
        with_body_lifecycle(&mut provider, &mut bodies, |storage, scope, parent| {
            TributeContract::new(storage)
                .get_tribute_ids_by_owner(scope, parent, alice)
                .unwrap()
        })
        .is_empty()
    );
    let alice_nods = with_body_lifecycle(&mut provider, &mut bodies, |storage, scope, parent| {
        nod_api::list_by_owner(&storage, scope, parent, alice).unwrap()
    });
    assert_eq!(alice_nods.len(), 1);
    let alice_item = &alice_nods[0];
    assert_eq!(alice_item.worldwide_day, green_wwd);
    let alice_floor_price = alice_item.floor_price_minor;
    let alice_bucket_id = EntityId36::new(alice_item.worldwide_day, alice_item.bucket_key.0);
    assert!(
        !with_body_lifecycle(&mut provider, &mut bodies, |storage, scope, parent| {
            nod_api::get_bucket(&storage, scope, parent, alice_bucket_id).unwrap()
        })
        .map(|bucket| bucket.is_qualified)
        .unwrap_or(false),
        "lysis must not qualify the bucket before the oracle rate rises"
    );

    // Advance once with a rate above both floors so NodLifecycle emits qualification events.
    with_storage(&mut provider, |storage| {
        seed_exchange_rate(storage, U256::from(500u64));
    });
    tick(
        &mut provider,
        &mut bodies,
        9,
        red.offering_end + SECONDS_PER_HOUR,
        None,
    );
    assert!(
        with_body_lifecycle(&mut provider, &mut bodies, |storage, scope, parent| {
            nod_api::get_bucket(&storage, scope, parent, alice_bucket_id).unwrap()
        })
        .map(|bucket| bucket.is_qualified)
        .unwrap_or(false),
        "NodLifecycle must qualify the projected bucket once rate exceeds its floor"
    );
    assert!(alice_floor_price < U256::from(500u64));

    let promis_after_green = with_storage(&mut provider, |storage| {
        PromisLimitContract::new(storage)
            .get_total_unallocated()
            .unwrap()
    });
    assert!(promis_after_green > U256::ZERO);
    assert!(mine_via_precompile(&mut provider, &mut bodies, alice) > U256::ZERO);

    // RED WAITING -> READY -> process_metadosis.
    tick(
        &mut provider,
        &mut bodies,
        10,
        red.scheduled + SECONDS_PER_HOUR,
        None,
    );
    with_storage(&mut provider, |storage| {
        let m = MetadosisContract::new(storage);
        assert_eq!(m.get_wwd_status(red_wwd).unwrap(), status::COMPLETED);
        assert_eq!(m.get_wwd_day_type(red_wwd).unwrap(), day_type::RED);
    });

    // Production Cycle ordering qualifies existing buckets before it runs
    // Metadosis/Lysis. Red-day Nods were minted in block 10, so they become
    // eligible for qualification only in the following block.
    tick(
        &mut provider,
        &mut bodies,
        11,
        red.scheduled + 2 * SECONDS_PER_HOUR,
        None,
    );

    for (owner, expected_qualified) in [(bob, true), (carol, false)] {
        let owner_nods =
            with_body_lifecycle(&mut provider, &mut bodies, |storage, scope, parent| {
                nod_api::list_by_owner(&storage, scope, parent, owner).unwrap()
            });
        assert_eq!(owner_nods.len(), 1);
        let owner_bucket_id =
            EntityId36::new(owner_nods[0].worldwide_day, owner_nods[0].bucket_key.0);
        assert_eq!(
            with_body_lifecycle(&mut provider, &mut bodies, |storage, scope, parent| {
                nod_api::get_bucket(&storage, scope, parent, owner_bucket_id).unwrap()
            })
            .map(|bucket| bucket.is_qualified),
            Some(expected_qualified)
        );
        assert!(
            with_body_lifecycle(&mut provider, &mut bodies, |storage, scope, parent| {
                TributeContract::new(storage)
                    .get_tribute_ids_by_owner(scope, parent, owner)
                    .unwrap()
            })
            .is_empty()
        );
    }

    let promis_after_red = with_storage(&mut provider, |storage| {
        PromisLimitContract::new(storage)
            .get_total_unallocated()
            .unwrap()
    });
    assert!(promis_after_red > promis_after_green);
    assert!(mine_via_precompile(&mut provider, &mut bodies, bob) > U256::ZERO);

    // This read opens a fresh block scope immediately after the final mining
    // block. It therefore also proves that end_block removed every temporary
    // body/index overlay entry left by the dependent burn + Gratis mint.
    let (projected_carol_nod, projected_carol_bucket) =
        with_body_lifecycle(&mut provider, &mut bodies, |storage, scope, parent| {
            assert!(nod_api::list_by_owner(&storage, scope, parent, bob)
                .unwrap()
                .is_empty());
            let carol_nods = nod_api::list_by_owner(&storage, scope, parent, carol).unwrap();
            assert_eq!(carol_nods.len(), 1);
            let carol_nod = &carol_nods[0];
            let carol_bucket = nod_api::get_bucket(
                &storage,
                scope,
                parent,
                EntityId36::new(carol_nod.worldwide_day, carol_nod.bucket_key.0),
            )
            .unwrap()
            .expect("Carol's unqualified bucket remains projected");
            (
                (
                    carol_nod.nod_id,
                    carol_nod.gratis_load_minor,
                    carol_nod.bucket_key,
                ),
                (carol_bucket.is_qualified, carol_bucket.total_nods),
            )
        });

    let gratis_balances = with_storage(&mut provider, |storage| {
        let gratis = Gratis::new(storage);
        [
            gratis.balance_of(alice).unwrap(),
            gratis.balance_of(bob).unwrap(),
            gratis.balance_of(carol).unwrap(),
        ]
    });
    ScenarioOutcome {
        consensus_storage: provider.storage.clone(),
        events: provider.get_ordered_events().to_vec(),
        native_balances: [
            provider.get_balance(alice),
            provider.get_balance(bob),
            provider.get_balance(carol),
        ],
        gratis_balances,
        projected_carol_nod,
        projected_carol_bucket,
    }
}

#[test]
fn test_runtime_e2e_green_then_red_wwd_lysis_nod_mine_gratis() {
    // The two executions use distinct consensus providers and distinct parent
    // projection stores. Equal results prove that the dependent Tribute ->
    // Lysis -> Nod qualification/mining -> Gratis flow is deterministic across
    // proposer/validator-style replay, including logs and projected survivors.
    let proposer = run_green_then_red_wwd_lysis_nod_mine_gratis();
    let validator = run_green_then_red_wwd_lysis_nod_mine_gratis();
    assert_eq!(proposer, validator);
}
