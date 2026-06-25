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
//! (`outbe_nodfactory::precompile::dispatch`) so the atomic burn-of-Nod +
//! `Gratis::mine` wiring inside the dispatcher is exercised, not duplicated.
//! The Nod precompile (0x1006) is read-only after the Nod/NodFactory split.
//!
//! Bucket qualification is oracle-driven: after lysis issues a NOD the bucket
//! starts UNQUALIFIED, and `NodLifecycle::begin_block` promotes it once the
//! COEN/0xUSD exchange rate reaches `bucket.floor_price_minor`. The test seeds
//! the rate via `seed_exchange_rate(...)` between lysis and mining and asserts
//! the bit flips after the next tick — matching the Cosmos reference
//! (`x/nod/keeper/qualification.go::QualifyBucketsByOracleRate`).
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

use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
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
use outbe_nod::NodContract;
use outbe_nodfactory::{
    precompile::{dispatch as nodfactory_dispatch, INodFactory},
    runtime as nodfactory_runtime,
};
use outbe_oracle::{
    contract::OracleContract,
    logic::{init_from_genesis, OracleGenesisConfig},
};
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

// Dummy asset + vault provider addresses passed into `mineGratis`. The
// provider has `enable_sub_call_stub()` flipped on, so the resulting
// `IERC20.transferFrom` / `IERC20.approve` / `IVaultProvider.depositLiquidity`
// sub-calls return `default_success()` without touching real ERC20 or vault
// state. The e2e exercises the lysis → nod → gratis pipeline; vault-side
// behavior is covered separately.
const MINE_GRATIS_ASSET: Address = address!("0x000000000000000000000000000000000000A11C");
const MINE_GRATIS_VAULT_PROVIDER: Address = address!("0x000000000000000000000000000000000000A11D");

struct WwdPhases {
    forming_end: u64,
    offering_entry: u64,
    offering_end: u64,
    scheduled: u64,
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
fn tick(storage: StorageHandle, block_number: u64, timestamp: u64, emission: Option<U256>) {
    // Mirror the block timestamp into the HashMap provider so precompiles that
    // read `self.storage.timestamp()` see the simulated block time advance with each tick.
    storage.set_block_timestamp(U256::from(timestamp)).unwrap();
    let ctx = build_ctx(storage, block_number, timestamp);
    if let Some(amount) = emission {
        emission_sink::apply(&ctx, amount).unwrap();
    }
    // drive Metadosis directly before the pre-execution hook
    // chain so subsequent NOD/Oracle hooks observe the same WWD state
    // they used to see when `MetadosisLifecycle::begin_block` was wired
    // between Rewards and Staking. Production now drives Metadosis from
    // the daily Cycle handler (post-execution); this keeps the test's
    // sub-day state-machine driving working without re-introducing the
    // legacy lifecycle.
    outbe_metadosis::runtime::start_metadosis(&ctx).unwrap();
    outbe_evm::executor::run_outbe_pre_execution_hooks(&ctx, None).unwrap();
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
/// qualify buckets whose `floor_price_minor <= rate`. In production this rate
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

fn find_valid_nonce(nod_id: U256) -> U256 {
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
fn mine_via_precompile(storage: StorageHandle, owner: Address) -> U256 {
    let nod = NodContract::new(storage.clone());
    let nods = nod.get_nods_by_owner(owner).unwrap();
    assert_eq!(nods.len(), 1, "expected exactly one NOD for {owner}");
    let item = nod.get_item(nods[0]).unwrap().unwrap();

    // Advance simulated block time past `unlocks_at` so the precompile's
    // 21-day lock check passes. The lifecycle ticks only cover the WWD's
    // ~14h state machine and do not naturally reach the unlock horizon.
    storage
        .set_block_timestamp(U256::from(item.unlocks_at + 1))
        .unwrap();

    let nonce = find_valid_nonce(item.nod_id);
    let balance_before = Gratis::new(storage.clone()).balance_of(owner).unwrap();

    let call = INodFactory::mineGratisCall {
        nodId: item.nod_id,
        nonce,
        asset: MINE_GRATIS_ASSET,
        vaultProvider: MINE_GRATIS_VAULT_PROVIDER,
    };
    let calldata = call.abi_encode();
    let output = nodfactory_dispatch(storage.clone(), &calldata, owner, U256::ZERO).unwrap();
    let mined = INodFactory::mineGratisCall::abi_decode_returns(&output).unwrap();
    assert_eq!(mined, item.gratis_load_minor);

    // Single precompile call must have burned the NOD and credited GRATIS atomically.
    assert!(NodContract::new(storage.clone())
        .get_item(item.nod_id)
        .unwrap()
        .is_none());
    let balance_after = Gratis::new(storage).balance_of(owner).unwrap();
    assert_eq!(balance_after, balance_before + mined);
    mined
}

#[test]
fn test_runtime_e2e_green_then_red_wwd_lysis_nod_mine_gratis() {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.enable_sub_call_stub();
    StorageHandle::enter(&mut provider, |storage| {
        // Pick non-adjacent WWDs so each day's full ~24-day lifecycle does not
        // accidentally interleave with the other's.
        let green_wwd = WorldwideDay::new(20241221);
        let red_wwd = WorldwideDay::new(20241222);

        let green = phases_for(green_wwd);
        let red = phases_for(red_wwd);

        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");
        let carol = address!("0x3333333333333333333333333333333333333333");

        let pair_id = init_oracle(storage.clone());

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

        let green_day_limit = U256::in_units(500_000u64);
        let red_day_limit = U256::in_units(5_000u64);

        // Ticks are ordered by ascending timestamp; both days progress in parallel.
        tick(
            storage.clone(),
            1,
            emission_timestamp(green_wwd),
            Some(green_day_limit),
        );
        tick(
            storage.clone(),
            2,
            emission_timestamp(red_wwd),
            Some(red_day_limit),
        );

        // GREEN FORMING -> LOOKBACK: VWAPs captured, day_type inferred.
        tick(storage.clone(), 3, green.forming_end, None);
        {
            let m = MetadosisContract::new(storage.clone());
            assert_eq!(m.get_wwd_status(green_wwd).unwrap(), status::LOOKBACK_DELAY);
            assert_eq!(m.get_wwd_day_type(green_wwd).unwrap(), day_type::GREEN);
        }

        // RED FORMING -> LOOKBACK.
        tick(storage.clone(), 4, red.forming_end, None);
        {
            let m = MetadosisContract::new(storage.clone());
            assert_eq!(m.get_wwd_status(red_wwd).unwrap(), status::LOOKBACK_DELAY);
            assert_eq!(m.get_wwd_day_type(red_wwd).unwrap(), day_type::RED);
        }

        // GREEN -> OFFERING: tribute unsealed by status machine; issue alice's tribute.
        tick(storage.clone(), 5, green.offering_entry, None);
        let green_token_id = U256::from_be_bytes(B256::left_padding_from(&[0xAA, 0x01]).0);
        let green_nominal = U256::in_units(1_000_000u64);
        {
            let mut tribute = TributeContract::new(storage.clone());
            tribute
                .issue(&TributeData {
                    token_id: green_token_id,
                    owner: alice,
                    worldwide_day: green_wwd,
                    issuance_amount_minor: green_nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: green_nominal,
                    reference_currency: 840,
                    tribute_price_minor: U256::ZERO,
                })
                .unwrap();
        }

        // RED -> OFFERING; issue bob's small and carol's large tribute.
        tick(storage.clone(), 6, red.offering_entry, None);
        let red_small_token = U256::from_be_bytes(B256::left_padding_from(&[0xBB, 0x01]).0);
        let red_large_token = U256::from_be_bytes(B256::left_padding_from(&[0xBB, 0x02]).0);
        let red_small_nominal = U256::in_units(20u64);
        let red_large_nominal = U256::in_units(1_000u64);
        {
            let mut tribute = TributeContract::new(storage.clone());
            tribute
                .issue(&TributeData {
                    token_id: red_small_token,
                    owner: bob,
                    worldwide_day: red_wwd,
                    issuance_amount_minor: red_small_nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: red_small_nominal,
                    reference_currency: 840,
                    tribute_price_minor: U256::ZERO,
                })
                .unwrap();
            tribute
                .issue(&TributeData {
                    token_id: red_large_token,
                    owner: carol,
                    worldwide_day: red_wwd,
                    issuance_amount_minor: red_large_nominal,
                    issuance_currency: 840,
                    nominal_amount_minor: red_large_nominal,
                    reference_currency: 840,
                    tribute_price_minor: U256::ZERO,
                })
                .unwrap();
        }

        // GREEN OFFERING -> WAITING; RED stays OFFERING.
        tick(storage.clone(), 7, green.offering_end, None);

        // block_time reaches red.offering_end: GREEN jumps OFFERING->WAITING->READY
        // (WAITING is crossed; process_metadosis runs), RED -> WAITING.
        tick(storage.clone(), 8, red.offering_end, None);

        // Tick at green.scheduled+1h is redundant here because block 8 already
        // pushed green past its scheduled time; assert COMPLETED.
        {
            let m = MetadosisContract::new(storage.clone());
            assert_eq!(
                m.get_wwd_status(green_wwd).unwrap(),
                status::COMPLETED,
                "GREEN should be COMPLETED after process_metadosis"
            );
            assert_eq!(m.get_wwd_day_type(green_wwd).unwrap(), day_type::GREEN);
        }

        // Alice got one NOD and her tribute is cleared, but the bucket is NOT
        // yet qualified — lysis only mints NODs, the oracle rate has to reach
        // floor_price for mining to unlock.
        let alice_bucket;
        let alice_floor_price;
        {
            let tribute = TributeContract::new(storage.clone());
            assert!(tribute.get_tributes_by_owner(alice).unwrap().is_empty());
            let nod = NodContract::new(storage.clone());
            let alice_nods = nod.get_nods_by_owner(alice).unwrap();
            assert_eq!(alice_nods.len(), 1);
            let alice_item = nod.get_item(alice_nods[0]).unwrap().unwrap();
            assert_eq!(alice_item.worldwide_day, green_wwd);

            alice_floor_price = alice_item.floor_price_minor;
            alice_bucket = NodContract::bucket_key(alice_item.worldwide_day, alice_floor_price);
            assert!(
                !nod.get_bucket(alice_bucket)
                    .unwrap()
                    .map(|b| b.is_qualified)
                    .unwrap_or(false),
                "lysis must NOT qualify the bucket — qualification is oracle-driven"
            );
        }

        // Mining is blocked until the COEN/0xUSD exchange rate reaches the
        // bucket's floor_price. Seed rate above both GREEN and RED floors and
        // advance one more tick so NodLifecycle promotes both buckets.
        seed_exchange_rate(storage.clone(), U256::from(500u64));
        tick(
            storage.clone(),
            9,
            red.offering_end + SECONDS_PER_HOUR,
            None,
        );
        {
            let nod = NodContract::new(storage.clone());
            assert!(
                nod.get_bucket(alice_bucket)
                    .unwrap()
                    .map(|b| b.is_qualified)
                    .unwrap_or(false),
                "NodLifecycle must qualify the bucket once oracle rate >= floor_price"
            );
            assert!(alice_floor_price <= U256::from(500u64));
        }

        // GREEN unused demand + day_metadosis_limit_remainder landed in PromisLimit.
        let promis_after_green = PromisLimitContract::new(storage.clone())
            .get_total_unallocated()
            .unwrap();
        assert!(
            promis_after_green > U256::ZERO,
            "GREEN remainder must accumulate in PromisLimit"
        );

        let green_mined = mine_via_precompile(storage.clone(), alice);
        assert!(green_mined > U256::ZERO);

        // RED WAITING -> READY -> process_metadosis.
        tick(storage.clone(), 10, red.scheduled + SECONDS_PER_HOUR, None);

        {
            let m = MetadosisContract::new(storage.clone());
            assert_eq!(m.get_wwd_status(red_wwd).unwrap(), status::COMPLETED);
            assert_eq!(m.get_wwd_day_type(red_wwd).unwrap(), day_type::RED);
        }

        // RED allocation is distributed proportionally across all tributes of
        // the day (lysis mints a NOD for each owner against a partial
        // gratis_load and consumes the tribute). Both bob's small and carol's
        // large tribute receive a NOD and are cleared — the allocation funds a
        // fraction of each, not whole tributes in size order.
        {
            let nod = NodContract::new(storage.clone());
            assert_eq!(nod.get_nods_by_owner(bob).unwrap().len(), 1);
            assert_eq!(nod.get_nods_by_owner(carol).unwrap().len(), 1);

            let tribute = TributeContract::new(storage.clone());
            assert!(tribute.get_tributes_by_owner(bob).unwrap().is_empty());
            assert!(tribute.get_tributes_by_owner(carol).unwrap().is_empty());
        }

        // RED remainder added on top of GREEN's.
        let promis_after_red = PromisLimitContract::new(storage.clone())
            .get_total_unallocated()
            .unwrap();
        assert!(
            promis_after_red > promis_after_green,
            "RED processing must add to PromisLimit total"
        );

        let bob_mined = mine_via_precompile(storage.clone(), bob);
        assert!(bob_mined > U256::ZERO);
    });
}
