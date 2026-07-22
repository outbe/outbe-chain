//! Auction-day e2e: metadosis brief -> desis schedule -> cross-chain bid
//! fan-in -> clearing gate -> issuance -> lysis contributor map -> creator
//! payout. Ticks run the production pre-execution hook chain (IntexLifecycle +
//! DesisLifecycle); the two Cycle handlers (`start_metadosis`, `tick_schedule`)
//! are invoked directly, as in the sibling WWD e2e. OriginRouter/NFT are
//! stubbed at the EVM boundary; bids and proceeds enter through the real
//! precompile dispatchers with the OriginRouter as caller.

use std::sync::Arc;

use alloy_primitives::{address, keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{
    begin_block, derive_poseidon_entity_id, end_block, CandidateCacheLimits, CeMdbx, CeWorkConfig,
    CompressedTreeService, EnvironmentIdentity, ExactParentIdentity, ExecutionScope,
    FinalizedMarker, ACTIVE_COMMITMENT_SCHEME, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_desis::{
    constants::{
        BIDS_FANIN_TIMEOUT_SECS, COMMIT_WINDOW_SECONDS, ORIGIN_ROUTER_ADDRESS,
        REVEAL_WINDOW_SECONDS,
    },
    precompile::{dispatch as desis_dispatch, IDesis},
    AuctionStage, DesisContract,
};
use outbe_intexfactory::precompile::{dispatch as intexfactory_dispatch, IIntexFactory};
use outbe_metadosis::{
    constants::{
        FORMING_PERIOD_HOURS, LOOKBACK_DELAY_HOURS, OFFERING_PERIOD_HOURS, SECONDS_PER_HOUR,
        WAITING_PERIOD_HOURS,
    },
    emission_sink,
    runtime::date_key_to_timestamp,
    schema::{day_type, status, MetadosisContract},
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
use outbe_primitives::addresses::{INTEX_FACTORY_ADDRESS, VAULT_PROVIDER_ADDRESS};
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

/// The day's snapshot: a remote target and the origin's loopback target.
const CHAIN_REMOTE: u32 = 97;
const CHAIN_LOCAL: u32 = 54_322_345;

const ALICE: Address = address!("0x1111111111111111111111111111111111111111");
const BIDDER_REMOTE_1: Address = address!("0x00000000000000000000000000000000000000b1");
const BIDDER_REMOTE_2: Address = address!("0x00000000000000000000000000000000000000b2");
const BIDDER_LOCAL: Address = address!("0x00000000000000000000000000000000000000b3");

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
        let genesis_hash = B256::repeat_byte(0x22);
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
                topology: outbe_compressed_entities::CeTopologyV1.encode(),
                tree_format: "ckb-smt-v0.6.1-poseidon-catalog-v3".to_owned(),
                vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
            },
            FinalizedMarker {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                height: 0,
                block_hash: genesis_hash,
                parent_block_hash: B256::ZERO,
                parent_root: B256::ZERO,
                new_root: outbe_compressed_entities::sealed_root(B256::ZERO).unwrap(),
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

/// Timestamp inside the WWD's forming window whose UTC date equals the WWD key.
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
    let block_hash =
        keccak256([b"ce-auction-block".as_slice(), &block_number.to_be_bytes()].concat());
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

/// One begin-block tick: production hook chain + the two Cycle handlers.
fn tick(
    provider: &mut HashMapStorageProvider,
    bodies: &mut BodyProjectionHarness,
    block_number: u64,
    timestamp: u64,
    emission: Option<U256>,
) {
    with_body_lifecycle(provider, bodies, |storage, scope, parent| {
        storage.set_block_timestamp(U256::from(timestamp)).unwrap();
        let ctx = build_ctx(storage.clone(), block_number, timestamp);
        if let Some(amount) = emission {
            emission_sink::apply(&ctx, amount).unwrap();
        }
        outbe_evm::executor::run_outbe_pre_execution_hooks_with_readers(&ctx, None, parent, scope)
            .unwrap();
        outbe_metadosis::runtime::start_metadosis(&ctx, scope, parent).unwrap();
        outbe_desis::tick_schedule(&ctx).unwrap();
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

/// Previous WWD's finalized VWAP (day-type baseline).
fn seed_previous_wwd_vwap(
    storage: StorageHandle,
    pair_id: u32,
    previous_wwd: WorldwideDay,
    vwap: U256,
) {
    let mut oracle = OracleContract::new(storage);
    let start = previous_wwd.start_timestamp();
    let end = start + FORMING_PERIOD_HOURS * SECONDS_PER_HOUR;
    oracle
        .write_snapshot(
            start + SECONDS_PER_HOUR,
            &[(pair_id, vwap, U256::from(1u64))],
        )
        .unwrap();
    oracle
        .store_worldwide_day_vwap_snapshot(previous_wwd, start, end)
        .unwrap();
}

/// Raw oracle observation inside the WWD's forming window (production VWAP capture path).
fn seed_current_wwd_vwap(storage: StorageHandle, pair_id: u32, wwd: WorldwideDay, vwap: U256) {
    let mut oracle = OracleContract::new(storage);
    let start = wwd.start_timestamp();
    oracle
        .write_snapshot(
            start + 30 * SECONDS_PER_HOUR,
            &[(pair_id, vwap, U256::from(1u64))],
        )
        .unwrap();
}

/// ABI-encoded `targetsOf` return for the OriginRouter stub (send* calls ignore it).
fn targets_stub(chains: &[u32]) -> Bytes {
    use alloy_sol_types::SolValue;
    Bytes::from(chains.to_vec().abi_encode())
}

/// Pre-create the sealed WWD with default hours so begin_block's auto-create is a no-op.
fn pre_create_wwd(storage: StorageHandle, wwd: WorldwideDay) {
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
    let mut tribute = TributeContract::new(storage);
    tribute.seal_day(wwd).unwrap();
}

fn issue_tribute(
    provider: &mut HashMapStorageProvider,
    bodies: &mut BodyProjectionHarness,
    owner: Address,
    wwd: WorldwideDay,
    nominal: U256,
) {
    let tribute_id = derive_poseidon_entity_id(owner, wwd).unwrap();
    with_body_lifecycle(provider, bodies, |storage, scope, parent| {
        TributeContract::new(storage)
            .issue(
                scope,
                parent,
                &TributeData {
                    tribute_id,
                    owner,
                    worldwide_day: wwd,
                    issuance_amount_minor: nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: nominal,
                    reference_currency: 840,
                    exclude_from_intex_issuance: false,
                    tribute_price_minor: U256::ZERO,
                },
            )
            .unwrap();
    });
    bodies.project_completed_block(provider);
}

/// One chain's bids through the Desis precompile: a single batch + BIDS_DONE.
fn relay_chain_bids(
    storage: &StorageHandle<'_>,
    wwd: u32,
    src_chain: u32,
    bids: &[(Address, u16, u32)],
    now: u64,
) {
    let calldata = IDesis::processBidsBatchCall {
        worldwideDay: wwd,
        srcChainId: src_chain,
        relayGeneration: 1,
        batchIndex: 0,
        totalBatches: 1,
        bidderAddresses: bids.iter().map(|(a, _, _)| *a).collect(),
        intexQuantities: bids.iter().map(|(_, q, _)| *q).collect(),
        intexBidRates: bids.iter().map(|(_, _, r)| *r).collect(),
        timestamps: bids.iter().map(|_| u32::try_from(now).unwrap()).collect(),
    }
    .abi_encode();
    desis_dispatch(
        storage.clone(),
        &calldata,
        ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
    )
    .unwrap();

    let done = IDesis::processBidsDoneCall {
        worldwideDay: wwd,
        srcChainId: src_chain,
        relayGeneration: 1,
        totalBatches: 1,
        totalBids: u32::try_from(bids.len()).unwrap(),
    }
    .abi_encode();
    desis_dispatch(storage.clone(), &done, ORIGIN_ROUTER_ADDRESS, U256::ZERO).unwrap();
}

/// Proceeds from one source chain via the IntexFactory precompile (`distribute{value}`).
fn deliver_proceeds(storage: &StorageHandle<'_>, wwd: u32, src_chain: u32, amount: U256) {
    storage
        .increase_balance(INTEX_FACTORY_ADDRESS, amount)
        .unwrap();
    let calldata = IIntexFactory::distributeCall {
        worldwideDay: wwd,
        srcChainId: src_chain,
    }
    .abi_encode();
    intexfactory_dispatch(storage.clone(), &calldata, ORIGIN_ROUTER_ADDRESS, amount).unwrap();
}

fn auction_stage(storage: &StorageHandle<'_>, wwd: u32) -> AuctionStage {
    let contract = storage.contract::<DesisContract>();
    AuctionStage::from_u8(contract.auction_stage.read(&wwd).unwrap()).unwrap()
}

/// Digest compared across the proposer/validator runs.
#[derive(Debug, PartialEq, Eq)]
struct ScenarioOutcome {
    green_stage: AuctionStage,
    red_stage: AuctionStage,
    green_issued: u32,
    green_brief_supply: U256,
    promis_unallocated_after: U256,
    alice_balance: U256,
    factory_balance: U256,
    event_count: usize,
}

/// Green day clears with bids from two chains and pays the creator; red cancels.
fn run_green_red_auction() -> ScenarioOutcome {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    let mut bodies = BodyProjectionHarness::new();
    provider.enable_sub_call_stub();
    provider.stub_sub_call_at(VAULT_PROVIDER_ADDRESS, Bytes::from(vec![0u8; 32]));
    provider.stub_sub_call_at(
        ORIGIN_ROUTER_ADDRESS,
        targets_stub(&[CHAIN_REMOTE, CHAIN_LOCAL]),
    );
    provider.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );

    let green_wwd = WorldwideDay::new(20250301);
    let red_wwd = WorldwideDay::new(20250302);
    let green = phases_for(green_wwd);
    let red = phases_for(red_wwd);

    let pair_id = with_storage(&mut provider, init_oracle);
    with_storage(&mut provider, |storage| {
        seed_previous_wwd_vwap(
            storage.clone(),
            pair_id,
            green_wwd.previous_date_key(),
            U256::from(100u64),
        );
        seed_current_wwd_vwap(storage.clone(), pair_id, green_wwd, U256::from(150u64));
        seed_current_wwd_vwap(storage.clone(), pair_id, red_wwd, U256::from(90u64));
        pre_create_wwd(storage.clone(), green_wwd);
        pre_create_wwd(storage, red_wwd);
    });

    let day_limit = U256::in_units(2_000_000u64);

    tick(
        &mut provider,
        &mut bodies,
        1,
        emission_timestamp(green_wwd),
        Some(day_limit),
    );
    tick(
        &mut provider,
        &mut bodies,
        2,
        emission_timestamp(red_wwd),
        Some(day_limit),
    );

    // FORMING -> LOOKBACK: VWAP captured, day type inferred.
    tick(&mut provider, &mut bodies, 3, green.forming_end, None);
    with_storage(&mut provider, |storage| {
        let m = MetadosisContract::new(storage);
        assert_eq!(m.get_wwd_day_type(green_wwd).unwrap(), day_type::GREEN);
    });
    tick(&mut provider, &mut bodies, 4, red.forming_end, None);
    with_storage(&mut provider, |storage| {
        let m = MetadosisContract::new(storage);
        assert_eq!(m.get_wwd_day_type(red_wwd).unwrap(), day_type::RED);
    });

    // GREEN OFFERING: Alice's tribute makes her the day's sole contributor.
    tick(&mut provider, &mut bodies, 5, green.offering_entry, None);
    issue_tribute(
        &mut provider,
        &mut bodies,
        ALICE,
        green_wwd,
        U256::in_units(1_000u64),
    );
    tick(&mut provider, &mut bodies, 6, red.offering_entry, None);

    // GREEN READY: lysis runs and the one-shot brief reaches Desis.
    tick(
        &mut provider,
        &mut bodies,
        7,
        green.offering_end + SECONDS_PER_HOUR,
        None,
    );
    tick(
        &mut provider,
        &mut bodies,
        8,
        green.scheduled + SECONDS_PER_HOUR,
        None,
    );
    let (green_brief_supply, green_anchor) = with_storage(&mut provider, |storage| {
        let m = MetadosisContract::new(storage.clone());
        assert_eq!(m.get_wwd_status(green_wwd).unwrap(), status::COMPLETED);
        let desis = storage.contract::<DesisContract>();
        // The brief anchors to the midnight just behind `scheduled`, so the same tick starts the auction.
        assert_eq!(
            auction_stage(&storage, u32::from(green_wwd)),
            AuctionStage::Started
        );
        assert_eq!(desis.brief_green.read(&u32::from(green_wwd)).unwrap(), 1);
        let supply = desis
            .pending_supply_promis
            .read(&u32::from(green_wwd))
            .unwrap();
        assert!(supply > U256::ZERO, "green brief must carry supply");
        let anchor = u64::from(desis.auction_at.read(&u32::from(green_wwd)).unwrap());
        (supply, anchor)
    });
    assert!(
        with_storage(&mut provider, |storage| {
            outbe_intex::api::contributor_total(&storage, u32::from(green_wwd)).unwrap()
        }) > U256::ZERO,
        "lysis must key the contributor map by the series id (== worldwide day)"
    );

    // RED READY: the zero-supply brief lands and the same tick cancels the day;
    // green hits its commit end here and flips to Revealing.
    tick(
        &mut provider,
        &mut bodies,
        9,
        red.scheduled + SECONDS_PER_HOUR,
        None,
    );
    with_storage(&mut provider, |storage| {
        let desis = storage.contract::<DesisContract>();
        assert_eq!(
            auction_stage(&storage, u32::from(red_wwd)),
            AuctionStage::Cancelled
        );
        assert_eq!(desis.brief_green.read(&u32::from(red_wwd)).unwrap(), 0);
        assert_eq!(
            auction_stage(&storage, u32::from(green_wwd)),
            AuctionStage::Revealing
        );
    });

    // Target chains relay revealed bids + BIDS_DONE.
    let green_commit_end = green_anchor + COMMIT_WINDOW_SECONDS;
    let bids_at = red.scheduled + 2 * SECONDS_PER_HOUR;
    tick(&mut provider, &mut bodies, 10, bids_at, None);
    with_storage(&mut provider, |storage| {
        assert_eq!(
            auction_stage(&storage, u32::from(green_wwd)),
            AuctionStage::Revealing
        );
        relay_chain_bids(
            &storage,
            u32::from(green_wwd),
            CHAIN_REMOTE,
            &[(BIDDER_REMOTE_1, 2, 900_000), (BIDDER_REMOTE_2, 1, 850_000)],
            bids_at,
        );
        relay_chain_bids(
            &storage,
            u32::from(green_wwd),
            CHAIN_LOCAL,
            &[(BIDDER_LOCAL, 1, 800_000)],
            bids_at,
        );
        let desis = storage.contract::<DesisContract>();
        assert_eq!(desis.day_bid_count.read(&u32::from(green_wwd)).unwrap(), 3);
    });

    // Reveal end arms the gate; the next tick's hook clears (all chains done).
    let green_reveal_end = green_commit_end + u64::from(REVEAL_WINDOW_SECONDS);
    tick(&mut provider, &mut bodies, 11, green_reveal_end + 60, None);
    tick(&mut provider, &mut bodies, 12, green_reveal_end + 120, None);
    let green_issued = with_storage(&mut provider, |storage| {
        assert_eq!(
            auction_stage(&storage, u32::from(green_wwd)),
            AuctionStage::Cleared
        );
        let desis = storage.contract::<DesisContract>();
        desis.last_clearing_issued_count.read().unwrap()
    });
    assert_eq!(green_issued, 4, "2 + 1 + 1 Intex units across both chains");
    assert!(
        with_storage(&mut provider, |storage| {
            outbe_intex::api::series_exists(&storage, u32::from(green_wwd)).unwrap()
        }),
        "clearing must create the series"
    );

    // Proceeds fan in; the second arrival completes the pot, the next tick pays.
    let proceeds_remote = U256::from(700u64);
    let proceeds_local = U256::from(300u64);
    with_storage(&mut provider, |storage| {
        deliver_proceeds(
            &storage,
            u32::from(green_wwd),
            CHAIN_REMOTE,
            proceeds_remote,
        );
        assert_eq!(
            storage.balance(ALICE).unwrap(),
            U256::ZERO,
            "partial fan-in must not pay"
        );
        deliver_proceeds(&storage, u32::from(green_wwd), CHAIN_LOCAL, proceeds_local);
    });
    tick(&mut provider, &mut bodies, 13, green_reveal_end + 180, None);
    let alice_balance = with_storage(&mut provider, |storage| storage.balance(ALICE).unwrap());
    assert_eq!(
        alice_balance,
        proceeds_remote + proceeds_local,
        "the sole contributor receives the full pot"
    );

    let event_count = provider.get_ordered_events().len();
    with_storage(&mut provider, |storage| {
        let factory_balance = storage.balance(INTEX_FACTORY_ADDRESS).unwrap();
        assert_eq!(
            factory_balance,
            U256::ZERO,
            "the drain empties the factory pot"
        );
        ScenarioOutcome {
            green_stage: auction_stage(&storage, u32::from(green_wwd)),
            red_stage: auction_stage(&storage, u32::from(red_wwd)),
            green_issued,
            green_brief_supply,
            promis_unallocated_after: PromisLimitContract::new(storage)
                .get_total_unallocated()
                .unwrap(),
            alice_balance,
            factory_balance,
            event_count,
        }
    })
}

#[test]
fn test_runtime_e2e_auction_green_clears_red_cancels() {
    let proposer = run_green_red_auction();
    let validator = run_green_red_auction();
    assert_eq!(proposer, validator);
    assert_eq!(proposer.green_stage, AuctionStage::Cleared);
    assert_eq!(proposer.red_stage, AuctionStage::Cancelled);
}

/// A silent chain is excluded once the fan-in deadline passes.
#[test]
fn test_runtime_e2e_auction_gate_deadline_skips_silent_chain() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_sub_call_stub();
    provider.stub_sub_call_at(
        ORIGIN_ROUTER_ADDRESS,
        targets_stub(&[CHAIN_REMOTE, CHAIN_LOCAL]),
    );
    provider.stub_sub_call_at(
        outbe_intexfactory::constants::INTEX_NFT1155_ADDRESS,
        Bytes::from(vec![0u8; 32]),
    );
    provider.stub_sub_call_at(VAULT_PROVIDER_ADDRESS, Bytes::from(vec![0u8; 32]));

    let wwd = 20250310u32;
    // Anchor lands on this midnight (now is early in the UTC day).
    let brief_at = date_key_to_timestamp(wwd) + SECONDS_PER_HOUR;

    StorageHandle::enter(&mut provider, |storage| {
        storage.set_block_timestamp(U256::from(brief_at)).unwrap();
        // Brief via the public API; the metadosis handoff is covered by the green/red e2e.
        assert!(outbe_desis::api::dispatch_auction_brief(
            storage.clone(),
            wwd,
            U256::from(
                outbe_desis::AuctionConfig::from_entry_price(U256::from(1u64)).promis_load_minor
            ) * U256::from(10u64),
            U256::from(1_000_000_000_000_000_000u128),
            true,
            brief_at,
        )
        .unwrap());
        assert_eq!(auction_stage(&storage, wwd), AuctionStage::Briefed);

        let tick_at = |storage: &StorageHandle<'_>, ts: u64| {
            storage.set_block_timestamp(U256::from(ts)).unwrap();
            let ctx = build_ctx(storage.clone(), 1, ts);
            outbe_desis::tick_schedule(&ctx).unwrap();
        };
        let gate_at = |storage: &StorageHandle<'_>, ts: u64| {
            storage.set_block_timestamp(U256::from(ts)).unwrap();
            let ctx = build_ctx(storage.clone(), 1, ts);
            use outbe_primitives::block::BlockLifecycle;
            <outbe_desis::DesisLifecycle as BlockLifecycle>::begin_block(&ctx).unwrap();
        };

        let anchor = {
            let contract = storage.contract::<DesisContract>();
            u64::from(contract.auction_at.read(&wwd).unwrap())
        };
        tick_at(&storage, anchor + 60);
        assert_eq!(auction_stage(&storage, wwd), AuctionStage::Started);

        let commit_end = anchor + COMMIT_WINDOW_SECONDS;
        tick_at(&storage, commit_end + 60);
        assert_eq!(auction_stage(&storage, wwd), AuctionStage::Revealing);

        relay_chain_bids(
            &storage,
            wwd,
            CHAIN_REMOTE,
            &[(BIDDER_REMOTE_1, 2, 900_000)],
            commit_end + 60,
        );

        let reveal_end = commit_end + u64::from(REVEAL_WINDOW_SECONDS);
        tick_at(&storage, reveal_end + 60);

        gate_at(&storage, reveal_end + 120);
        assert_ne!(auction_stage(&storage, wwd), AuctionStage::Cleared);

        gate_at(&storage, reveal_end + BIDS_FANIN_TIMEOUT_SECS + 120);
        assert_eq!(auction_stage(&storage, wwd), AuctionStage::Cleared);
        let contract = storage.contract::<DesisContract>();
        assert_eq!(
            contract.last_clearing_issued_count.read().unwrap(),
            2,
            "only the reporting chain's bids are cleared"
        );
    });
}
