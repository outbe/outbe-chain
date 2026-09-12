//! A Gem from the merchant side: parking an Intex, issuing gems out of the
//! position, and the three ways one ends - mined, forfeited, or never issued.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolEvent;
use cucumber::{then, when};
use outbe_tee::protocol::{Ledger, PromisOp};

use crate::features::settlement::{
    assert_mined_success, chain_id_b256, find_pow_nonce, promis_balance,
};
use crate::internal::{addresses, eth};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::settlement_currency::{self, SettlementCurrency};
use crate::world::test_issuance::{self, SeriesSpec};
use crate::world::{venue_probes, World};

/// The source series' entry price; the gems derive their own from it.
const ENTRY_PRICE_MINOR: u64 = 1_000_000;
/// PROMIS-units per Intex unit, on the wire scale.
const PROMIS_LOAD_MINOR: u128 = 100_000;
/// Units minted to the merchant, and how many of them are parked. Parking part
/// of the holding keeps the burn visible against what stays.
const UNITS: u32 = 8;
const PARKED_UNITS: u32 = 4;
/// Load per issued gem: a quarter of the parked capacity, so two gems leave
/// half the position unissued for the expiry to return.
const GEM_LOAD_MINOR: u128 = 100_000;
/// USD (840) as the reference, spelled `U` in the series id.
const REFERENCE_BYTE: u8 = b'U';
/// `GemTypes::Merchant`.
const MERCHANT_GEM_TYPE: u8 = 5;
/// `GemState::Issued` / `Qualified` / `Called`.
const ISSUED: u8 = 0;
const QUALIFIED: u8 = 1;
const CALLED: u8 = 2;
/// DEV calls a gem once the VWAP held above its Call Price on two of three days.
const CALL_THRESHOLD_DAYS: u32 = 2;
/// How far back the gem's issuance is stamped, so the seeded days lie after it.
const CALL_LOOKBACK_DAYS: u64 = 3;
/// Issuance mints through a message, not inside the issuing call.
const ISSUANCE_TIMEOUT_SECS: u64 = 180;
/// The qualify scan runs in begin-block; a handful of blocks is plenty.
const QUALIFY_TIMEOUT_SECS: u64 = 180;
/// The call sweep is on a shortened cadence, not instant.
const CALL_TIMEOUT_SECS: u64 = 300;
/// Bounded processing allowance after the notice's UTC hour bucket closes.
const EXPIRY_SWEEP_SLACK_SECS: u64 = 300;
/// The position sweep runs after its deadline, on the same cadence.
const POSITION_SWEEP_TIMEOUT_SECS: u64 = 300;
/// The DEV validity (15 minutes) runs from parking, and most of it is spent
/// waiting out the call notice before this step is reached.
const POSITION_DEADLINE_TIMEOUT_SECS: u64 = 900;

#[when("a test Intex series is issued to a funded merchant")]
fn issue_source_series(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    let asset = settlement_asset(world);
    let merchant = crate::world::origin_venue::deployer_address();

    // Enough to settle the gem at any price the sweeps derive.
    test_issuance::fund_settler(&url, asset, DEPLOYER_KEY, U256::from(u64::MAX))
        .expect("fund the settling merchant");

    let origin_router = world
        .state
        .origin_contracts
        .as_ref()
        .expect("intex engine was deployed")
        .origin_router;
    let head = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    let day = outbe_primitives::time::worldwide_day_from_timestamp(head);
    let now = u32::try_from(head).expect("timestamp fits a uint32");

    test_issuance::open_day(
        &url,
        DEPLOYER_KEY,
        origin_router,
        day,
        now,
        settlement_currency::USD_ISO,
        ENTRY_PRICE_MINOR,
        PROMIS_LOAD_MINOR,
    )
    .expect("open the day the source series is issued into");

    // One chain, one series: gems never leave the committee, so this scenario
    // needs neither a second venue nor a relay.
    let series = test_issuance::issue_series(
        &url,
        DEPLOYER_KEY,
        day,
        now,
        settlement_currency::USD_ISO,
        REFERENCE_BYTE,
        U256::from(ENTRY_PRICE_MINOR),
        PROMIS_LOAD_MINOR,
        merchant,
        &[UNITS],
        &[u32::try_from(chain_id).expect("committee chain id fits a uint32")],
        &[SeriesSpec {
            issuance: *b"USD",
            issuance_currency: settlement_currency::USD_ISO,
        }],
    )
    .expect("issue the source series");

    let series = *series.first().expect("one series was issued");

    // Issuance reaches the collection as its own message; parking before the
    // units land reverts with NonexistentToken.
    let nft = intex_nft(world);
    let deadline = Instant::now() + Duration::from_secs(ISSUANCE_TIMEOUT_SECS);
    loop {
        if venue_probes::series_balances(&url, nft, series, merchant) == Some((u64::from(UNITS), 0))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "series {series} never minted its units to the merchant"
        );
        sleep(Duration::from_secs(2));
    }

    world.state.gem_source_series = Some(series);
}

#[when("the merchant parks part of their units into a gem position")]
fn park_units(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let merchant = crate::world::origin_venue::deployer_address();
    let series = source_series(world);

    let call = eth::IGemFactory::issueGemPositionCall {
        sourceIntexId: series,
        amount: U256::from(PARKED_UNITS),
    };
    // The precompile reports a decode failure rather than the collection's own
    // revert, so simulate first: that keeps the real reason in the failure.
    if let Err(error) = eth::simulate_call(&url, addresses::GEM_FACTORY_ADDR, merchant, &call) {
        panic!("parking units reverts: {error}");
    }
    let parked =
        eth::send_call_outcome(&url, addresses::GEM_FACTORY_ADDR, DEPLOYER_KEY, &call, None)
            .expect("park units into a gem position");
    assert_mined_success(&parked, "park units into a gem position");

    // The position is a single-owner NFT, and this is the merchant's first.
    let position_id = eth::read_call(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::tokenOfOwnerByIndexCall {
            owner: merchant,
            index: U256::ZERO,
        },
    )
    .expect("the parked position was issued to the merchant");
    world.state.gem_position = Some(position_id);
}

#[then("the position holds the parked capacity and the units are burned")]
fn position_holds_capacity(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let merchant = crate::world::origin_venue::deployer_address();
    let nft = intex_nft(world);
    let series = source_series(world);
    let position = read_position(world);

    assert_eq!(
        position.remainingCapacity,
        U256::from(PROMIS_LOAD_MINOR) * U256::from(PARKED_UNITS),
        "the position did not take the parked units' whole load as capacity"
    );
    assert_eq!(
        position.merchant, merchant,
        "the position was issued to somebody other than the merchant who parked"
    );
    assert!(
        position.expiresAt > position.parkedAt,
        "the position was parked without a deadline to expire at"
    );
    assert_eq!(
        venue_probes::series_balances(&url, nft, series, merchant),
        Some((u64::from(UNITS - PARKED_UNITS), 0)),
        "parking did not burn exactly the units it took"
    );
}

#[when("the merchant issues two gems from the position, leaving capacity unissued")]
fn issue_two_gems(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let merchant = crate::world::origin_venue::deployer_address();
    let position_id = world.state.gem_position.expect("a position was parked");

    for _ in 0..2 {
        let issued = eth::send_call_outcome(
            &url,
            addresses::GEM_FACTORY_ADDR,
            DEPLOYER_KEY,
            &eth::IGemFactory::issueGemCall {
                positionId: position_id,
                owner: merchant,
                promisLoad: U256::from(GEM_LOAD_MINOR),
            },
            None,
        )
        .expect("issue a merchant gem");
        assert_mined_success(&issued, "issue a merchant gem");
    }

    let gems: Vec<U256> = (0..2)
        .map(|index| {
            eth::read_call(
                &url,
                addresses::GEM_ADDR,
                &eth::IGem::tokenOfOwnerByIndexCall {
                    owner: merchant,
                    index: U256::from(index),
                },
            )
            .expect("the merchant owns the gem just issued")
        })
        .collect();
    world.state.mined_gem = Some(gems[0]);
    world.state.forfeited_gem = Some(gems[1]);
}

#[then("both gems read Issued and carry the position's terms")]
fn gems_read_issued(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let merchant = crate::world::origin_venue::deployer_address();
    let position = read_position(world);

    assert_eq!(
        position.remainingCapacity,
        U256::from(PROMIS_LOAD_MINOR) * U256::from(PARKED_UNITS)
            - U256::from(GEM_LOAD_MINOR) * U256::from(2),
        "issuing the gems did not drain exactly their load from the position"
    );

    for gem_id in [mined_gem(world), forfeited_gem(world)] {
        let gem = read_gem(&url, gem_id);
        assert_eq!(gem.state, ISSUED, "gem {gem_id} was not born Issued");
        assert_eq!(gem.owner, merchant, "gem {gem_id} went to the wrong owner");
        assert_eq!(
            gem.gemType, MERCHANT_GEM_TYPE,
            "a gem issued from a parked position is a Merchant gem"
        );
        assert_eq!(
            gem.promisLoad,
            U256::from(GEM_LOAD_MINOR),
            "gem {gem_id} does not carry the load it drained"
        );
        assert_eq!(
            (gem.issuanceCurrency, gem.referenceCurrency),
            (position.issuanceCurrency, position.referenceCurrency),
            "gem {gem_id} does not inherit the parked series' currencies"
        );
        // The anti-dilution floor: never below the Intex the position came from.
        assert!(
            gem.entryPrice >= position.sourceEntryPrice,
            "gem {gem_id} priced below the Intex it was parked from"
        );
    }
}

#[when("the reference rate stands above the gem floor")]
fn rate_above_gem_floor(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let floor = read_gem(&url, mined_gem(world)).floorPrice;
    // Both gems share an entry price, so one floor decides them both.
    crate::features::price_oracle::publish_controlled_quote(world, floor * U256::from(2));
}

#[then("both gems qualify")]
fn both_gems_qualify(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    for gem_id in [mined_gem(world), forfeited_gem(world)] {
        wait_for_gem_state(&url, gem_id, QUALIFIED, QUALIFY_TIMEOUT_SECS);
    }
}

#[when("the merchant settles the first gem and mines its Promis")]
fn settle_and_mine(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let merchant = crate::world::origin_venue::deployer_address();
    let gem_id = mined_gem(world);
    let asset = settlement_asset(world);
    let load = read_gem(&url, gem_id).promisLoad;

    // The cost is derived, so the note covers the factory's own quote.
    let payable = eth::read_call(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::quoteSettlementCall {
            gemId: gem_id,
            asset,
        },
    )
    .expect("quote settling the merchant gem")
    .payableUnits;
    // A zero quote would otherwise become a one-unit note and fail inside the
    // settle, where the revert reads as a balance problem instead of a price one.
    assert!(!payable.is_zero(), "the gem quoted a zero settlement cost");
    // The cost is discharged by burning a note, so the vault is credited here
    // rather than at settle time.
    let paynote_proof = crate::features::paynote::deposit_and_prove(
        world,
        world.validators.primary_port(),
        DEPLOYER_KEY,
        merchant,
        asset,
        payable,
    );

    let settle = eth::send_call_outcome(
        &url,
        addresses::GEM_FACTORY_ADDR,
        DEPLOYER_KEY,
        &eth::IGemFactory::settleGemCall {
            gemId: gem_id,
            payNoteProof: paynote_proof.into(),
        },
        None,
    )
    .expect("settle the merchant gem");
    assert_mined_success(&settle, "settle the merchant gem");

    let keys = eth::derive_account_keys(&url, DEPLOYER_KEY, Ledger::Promis)
        .expect("derive merchant Promis keys");
    world.state.promis_before_mining = Some(promis_balance(&url, merchant, &keys.view));

    let op_nonce = eth::read_call(
        &url,
        addresses::PROMIS_ADDR,
        &eth::IPromis::opNonceOfCall { account: merchant },
    )
    .expect("Promis nonce before gem mining");
    let mac = outbe_tee_enclave::promis::modify_mac(
        &keys.modify,
        merchant,
        PromisOp::Mint,
        load,
        op_nonce,
        chain_id_b256(world),
    );
    let mine = eth::send_call_outcome(
        &url,
        addresses::GEM_FACTORY_ADDR,
        DEPLOYER_KEY,
        &eth::IGemFactory::minePromisCall {
            gemId: gem_id,
            nonce: find_pow_nonce(gem_id),
            mac: B256::from(mac),
            opNonce: op_nonce,
        },
        None,
    )
    .expect("mine Promis from the settled gem");
    assert_mined_success(&mine, "mine Promis from the settled gem");
}

#[then("that gem is burned and its load lands in the merchant's Promis")]
fn gem_burned_into_promis(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let merchant = crate::world::origin_venue::deployer_address();
    let keys = eth::derive_account_keys(&url, DEPLOYER_KEY, Ledger::Promis)
        .expect("derive merchant Promis keys");
    let before = world
        .state
        .promis_before_mining
        .expect("the Promis balance was read before mining");

    assert_eq!(
        promis_balance(&url, merchant, &keys.view),
        before + U256::from(GEM_LOAD_MINOR),
        "the gem's load was not minted exactly into the merchant's Promis"
    );
    // A positive read, not the absence of one: a status read can also come back
    // empty because the node was busy.
    assert_eq!(
        gem_count(&url, merchant),
        U256::from(1),
        "mining did not burn the gem out of the merchant's holding"
    );
}

#[when("the call trigger holds above the second gem's call price across its window")]
fn call_trigger_holds(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let gem_id = forfeited_gem(world);
    let entry = read_gem(&url, gem_id).entryPrice;

    // Either expiry may credit the pool first. Capture one baseline while the
    // position still holds all unissued capacity and the gem is not Called.
    let height = finalized_observation_height(world);
    let position = eth::read_call_at(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::getPositionCall {
            positionId: world.state.gem_position.expect("parked position"),
        },
        height,
    )
    .expect("position at expiry baseline");
    assert_eq!(position.remainingCapacity, unissued_capacity());
    assert_eq!(read_gem(&url, gem_id).state, QUALIFIED);
    let pool = eth::read_call_at(
        &url,
        addresses::PROMIS_LIMIT_ADDR,
        &eth::IPromisLimit::totalUnallocatedCall {},
        height,
    )
    .expect("pool at expiry baseline");
    world.state.gem_expiry_baseline = Some((height, pool));

    // A gem counts breach days from its own issuance day forward, and this one
    // was issued minutes ago: stamp it behind the days about to be seeded.
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    eth::send_call(
        &url,
        addresses::GEM_ADDR,
        DEPLOYER_KEY,
        &IGemTestArming::backdateGemForTestCall {
            gemId: gem_id,
            issuedAt: now.saturating_sub(CALL_LOOKBACK_DAYS * 86_400),
        },
        None,
    )
    .expect("backdate the gem's issuance stamp");

    // Seed the days rather than live through them; the sweep still counts them
    // itself. A rate far above entry clears the derived Call Price by any margin.
    test_issuance::seed_day_vwaps(
        &url,
        DEPLOYER_KEY,
        settlement_currency::USD_ISO,
        CALL_THRESHOLD_DAYS,
        entry * U256::from(100),
    )
    .expect("seed the call-window VWAPs");
}

#[then("that gem becomes Called")]
fn gem_becomes_called(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    wait_for_gem_state(&url, forfeited_gem(world), CALLED, CALL_TIMEOUT_SECS);
}

#[then("it is forfeited and its load returns to the unallocated pool")]
fn gem_is_forfeited(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let merchant = crate::world::origin_venue::deployer_address();
    let gem_id = forfeited_gem(world);
    let gem = read_gem(&url, gem_id);
    assert_eq!(gem.state, CALLED);
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("head timestamp");
    let wait = forfeit_wait_seconds(now, gem.calledAt, u64::from(gem.callNoticePeriod));
    eprintln!(
        "Gem forfeit: called_at={} notice={} wait_budget_secs={wait}",
        gem.calledAt, gem.callNoticePeriod
    );
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        if gem_count(&url, merchant).is_zero() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "gem {gem_id} outlived its call notice; the forfeit sweep never burned it"
        );
        sleep(Duration::from_secs(2));
    }

    assert_expiry_returns(world, finalized_observation_height(world), false);
}

#[when("the position's validity runs out")]
fn wait_for_position_expiry(world: &mut World) {
    let port = world.validators.primary_port();
    let expires_at = read_position(world).expiresAt;
    let deadline = Instant::now() + Duration::from_secs(POSITION_DEADLINE_TIMEOUT_SECS);
    loop {
        let now = world
            .rpc
            .latest_block_timestamp(port)
            .expect("committee head timestamp");
        if now > expires_at {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the chain never reached the position's deadline {expires_at}, stuck at {now}"
        );
        sleep(Duration::from_secs(2));
    }
}

#[then("the position returns its unissued capacity to the same pool")]
fn position_returns_capacity(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let position_id = world.state.gem_position.expect("a position was parked");

    // A retired position keeps its record and drops its capacity to zero; only
    // the sweep's live queue forgets it.
    let deadline = Instant::now() + Duration::from_secs(POSITION_SWEEP_TIMEOUT_SECS);
    loop {
        let remaining = eth::read_call(
            &url,
            addresses::GEM_FACTORY_ADDR,
            &eth::IGemFactory::getPositionCall {
                positionId: position_id,
            },
        )
        .expect("the position reads back after its deadline")
        .remainingCapacity;
        if remaining.is_zero() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "position {position_id} is past its deadline still holding {remaining}; the sweep has not retired it"
        );
        sleep(Duration::from_secs(2));
    }

    assert_expiry_returns(world, finalized_observation_height(world), true);
}

fn forfeit_wait_seconds(now: u64, called_at: u64, notice: u64) -> u64 {
    // Production queues by deadline hour and drains only closed buckets,
    // including when the notice ends exactly on an hour boundary.
    let bucket_end = ((called_at + notice) / 3_600 + 1) * 3_600;
    bucket_end.saturating_sub(now) + EXPIRY_SWEEP_SLACK_SECS
}

fn unissued_capacity() -> U256 {
    U256::from(PROMIS_LOAD_MINOR) * U256::from(PARKED_UNITS)
        - U256::from(GEM_LOAD_MINOR) * U256::from(2)
}

fn finalized_observation_height(world: &World) -> u64 {
    let port = world.validators.primary_port();
    let height = eth::block_number(&world.rpc.url(port)).expect("observation height");
    assert!(
        world.rpc.wait_finalized_at_least(port, height, 30),
        "expiry observation did not finalize"
    );
    height
}

fn assert_expiry_returns(world: &World, height: u64, require_position_expiry: bool) {
    let url = world.rpc.url(world.validators.primary_port());
    let (from, before) = world
        .state
        .gem_expiry_baseline
        .expect("baseline before both returns");
    let merchant = crate::world::origin_venue::deployer_address();
    let position_id = world.state.gem_position.expect("parked position");
    let position = eth::read_call_at(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::getPositionCall {
            positionId: position_id,
        },
        height,
    )
    .expect("finalized position after return");
    let position_expired = position.remainingCapacity.is_zero();
    if require_position_expiry {
        assert!(position_expired, "position retained unissued capacity");
    } else if !position_expired {
        assert_eq!(position.remainingCapacity, unissued_capacity());
    }
    assert_expiry_event(
        &url,
        addresses::GEM_ADDR,
        from,
        height,
        eth::IGem::GemExpired {
            gemId: forfeited_gem(world),
            owner: merchant,
            promisLoad: U256::from(GEM_LOAD_MINOR),
        },
    );
    let returned = if position_expired {
        assert_expiry_event(
            &url,
            addresses::GEM_FACTORY_ADDR,
            from,
            height,
            eth::IGemFactory::GemPositionExpired {
                positionId: position_id,
                merchant,
                sourceIntexId: source_series(world),
                returnedCapacity: unissued_capacity(),
            },
        );
        U256::from(GEM_LOAD_MINOR) + unissued_capacity()
    } else {
        U256::from(GEM_LOAD_MINOR)
    };
    assert_eq!(
        eth::read_call_at(
            &url,
            addresses::PROMIS_LIMIT_ADDR,
            &eth::IPromisLimit::totalUnallocatedCall {},
            height
        ),
        Some(before + returned),
        "expiry returns did not credit the exact unallocated load"
    );
}

fn assert_expiry_event<E: SolEvent>(url: &str, address: Address, from: u64, to: u64, expected: E) {
    let encoded = expected.encode_log_data();
    let logs = eth::raw_json_result(
        url,
        "eth_getLogs",
        serde_json::json!([{
            "address": address, "fromBlock": format!("0x{from:x}"), "toBlock": format!("0x{to:x}"),
            "topics": [encoded.topics()[0], encoded.topics()[1]],
        }]),
    )
    .expect("finalized expiry events");
    let logs = logs.as_array().expect("expiry log array");
    assert_eq!(logs.len(), 1, "expected exactly one expiry for this asset");
    assert_eq!(
        logs[0]["topics"],
        serde_json::json!(encoded.topics()),
        "expiry identity mismatch"
    );
    assert_eq!(
        logs[0]["data"],
        serde_json::json!(encoded.data),
        "expiry amount mismatch"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forfeit_wait_covers_the_closed_hour_not_just_the_notice() {
        // Failed run: Called 18:39:01, notice ends 18:49:01, sweep due 19:00.
        assert_eq!(
            forfeit_wait_seconds(1_789_151_941, 1_789_151_941, 600),
            1_259 + 300
        );
        assert_eq!(forfeit_wait_seconds(3_599, 2_999, 600), 1 + 300);
        assert_eq!(forfeit_wait_seconds(3_600, 3_000, 600), 3_600 + 300);
        assert_eq!(forfeit_wait_seconds(7_201, 3_000, 600), 300);
    }
}

alloy_sol_types::sol! {
    interface IGemTestArming {
        function backdateGemForTest(uint256 gemId, uint64 issuedAt) external;
    }
}

fn settlement_asset(world: &World) -> Address {
    let SettlementCurrency { asset, .. } = world
        .state
        .settlement_currency
        .expect("settlement currency was registered");
    asset
}

fn source_series(world: &World) -> alloy_primitives::FixedBytes<14> {
    world
        .state
        .gem_source_series
        .expect("the source series was issued")
}

fn mined_gem(world: &World) -> U256 {
    world.state.mined_gem.expect("the first gem was issued")
}

fn forfeited_gem(world: &World) -> U256 {
    world
        .state
        .forfeited_gem
        .expect("the second gem was issued")
}

fn intex_nft(world: &World) -> Address {
    world
        .state
        .origin_contracts
        .as_ref()
        .expect("intex engine was deployed")
        .intex_nft
}

fn read_position(world: &World) -> eth::IGemFactory::PositionData {
    let url = world.rpc.url(world.validators.primary_port());
    eth::read_call(
        &url,
        addresses::GEM_FACTORY_ADDR,
        &eth::IGemFactory::getPositionCall {
            positionId: world.state.gem_position.expect("a position was parked"),
        },
    )
    .expect("the parked position reads back")
}

fn gem_count(url: &str, owner: Address) -> U256 {
    eth::read_call(
        url,
        addresses::GEM_ADDR,
        &eth::IGem::balanceOfCall { owner },
    )
    .expect("the gem collection answers a balance read")
}

fn read_gem(url: &str, gem_id: U256) -> eth::IGem::GemData {
    eth::read_call(
        url,
        addresses::GEM_ADDR,
        &eth::IGem::getGemStatusCall { gemId: gem_id },
    )
    .unwrap_or_else(|| panic!("gem {gem_id} does not read back"))
}

fn wait_for_gem_state(url: &str, gem_id: U256, want: u8, timeout_secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let state = read_gem(url, gem_id).state;
        if state == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "gem {gem_id} sat at state {state} instead of reaching {want}"
        );
        sleep(Duration::from_secs(2));
    }
}
