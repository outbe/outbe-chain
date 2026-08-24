//! An Intex after the mint: qualification, settlement, and the burn into Promis.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use alloy_sol_types::sol;
use outbe_tee::protocol::{Ledger, PromisOp};

use crate::internal::{addresses, eth};
use cucumber::{then, when};

use crate::env::environment;
use crate::world::forge::DEPLOYER_KEY;
use crate::world::settlement_currency::{self, SettlementCurrency};
use crate::world::test_issuance::{self, SeriesSpec};
use crate::world::{venue_probes, World};

/// Both series carry the same entry price so their floors share a price bin and one
/// sweep pass decides them together.
const ENTRY_PRICE_MINOR: u64 = 1_000_000;
/// PROMIS-units per Intex unit, on the wire scale.
const PROMIS_LOAD_MINOR: u128 = 100_000;
/// Units each series mints to the holder; settled in two goes, so keep it even.
const UNITS: u32 = 10;
/// USD (840) as the reference for both series, spelled `U` in the series id.
const REFERENCE_BYTE: u8 = b'U';
/// The DEV profile qualifies a series a day after issuance; overshoot so the
/// sweep sees the period closed rather than exactly met.
const QUALIFICATION_PERIOD_SECS: u64 = 120;
const QUALIFICATION_MARGIN_SECS: u64 = 30;
/// Long enough for the chain to close a one-day gap, which it does per block.
const CATCH_UP_TIMEOUT_SECS: u64 = 900;
/// The sweep runs in begin-block; a handful of blocks is plenty.
const QUALIFY_SWEEP_TIMEOUT_SECS: u64 = 180;
/// `IntexState::Qualified`.
const QUALIFIED: u8 = 1;
/// `IntexState::Called`.
const CALLED: u8 = 2;
/// DEV calls a series once the VWAP held above the call price on two of three days.
/// DEV requires two of the last three days above the trigger.
const CALL_THRESHOLD_DAYS: u32 = 2;
/// How far back the series are issued so closed days exist after their issuance.
const CALL_LOOKBACK_DAYS: u32 = 3;
/// The call sweep is daily; give it a few blocks past the last rollover.
const CALL_SWEEP_TIMEOUT_SECS: u64 = 300;

#[when("the settlement currency is registered on the committee chain")]
fn register_settlement_currency(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    // `addVault` admits the router's owner alone, and genesis seeds that to validator 0.
    let owner_key = world
        .validators
        .get(0)
        .evm_key()
        .expect("VaultRouter owner key");

    let currency = settlement_currency::deploy(
        &environment().repo.join("contracts/intex"),
        &url,
        &owner_key,
    )
    .expect("register the settlement currency");

    world.state.settlement_currency = Some(currency);
}

#[then("holders may settle in that currency")]
fn settlement_currency_is_acceptable(world: &mut World) {
    let SettlementCurrency { asset, vault } = world
        .state
        .settlement_currency
        .expect("settlement currency was registered");
    let url = world.rpc.url(world.validators.primary_port());

    assert_eq!(
        settlement_currency::registered_vaults(&url, asset),
        vec![vault],
        "the VaultRouter does not route the settlement asset to its vault"
    );
    assert_eq!(
        settlement_currency::iso_code(&url, asset),
        Some(settlement_currency::USD_ISO),
        "the settlement asset does not answer the reference currency"
    );
}

#[when("two test Intex series sharing a reference currency are issued to a funded holder")]
fn issue_two_series(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    let asset = world
        .state
        .settlement_currency
        .expect("settlement currency was registered")
        .asset;
    let holder = crate::world::origin_venue::deployer_address();

    // Enough to settle every unit of both series at any price the sweeps derive.
    test_issuance::fund_settler(&url, asset, DEPLOYER_KEY, U256::from(u64::MAX))
        .expect("fund the settling holder");

    // The router addresses an issuance leg only to a chain the day was started on,
    // so the day has to be opened before anything can be issued into it.
    // Issued into a day already behind us: the call sweep counts breach days only
    // from the issuance day forward, and only closed days exist to count.
    let day = chain_worldwide_day_offset(world, port, -(i64::from(CALL_LOOKBACK_DAYS) * 86_400));
    let now = u32::try_from(
        world
            .rpc
            .latest_block_timestamp(port)
            .expect("committee head timestamp"),
    )
    .expect("timestamp fits a uint32");
    test_issuance::open_day(
        &url,
        DEPLOYER_KEY,
        world
            .state
            .origin_contracts
            .as_ref()
            .expect("intex engine was deployed")
            .origin_router,
        day,
        now,
        settlement_currency::USD_ISO,
        ENTRY_PRICE_MINOR,
        PROMIS_LOAD_MINOR,
    )
    .expect("open the day the series are issued into");

    // Same day and reference currency, different issuance currencies: one group,
    // two members, which is what makes the group promotion and the mark batch real.
    let series = test_issuance::issue_series(
        &url,
        DEPLOYER_KEY,
        day,
        settlement_currency::USD_ISO,
        REFERENCE_BYTE,
        U256::from(ENTRY_PRICE_MINOR),
        PROMIS_LOAD_MINOR,
        holder,
        UNITS,
        u32::try_from(chain_id).expect("committee chain id fits a uint32"),
        &[
            SeriesSpec {
                issuance: *b"USD",
                issuance_currency: settlement_currency::USD_ISO,
            },
            SeriesSpec {
                issuance: *b"EUR",
                issuance_currency: 978,
            },
        ],
    )
    .expect("issue the lifecycle series");

    world.state.lifecycle_series = series;
}

#[then("the holder holds issued units of both series and none are settled")]
fn holder_holds_issued_units(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = world
        .state
        .origin_contracts
        .as_ref()
        .expect("intex engine was deployed")
        .intex_nft;
    let holder = crate::world::origin_venue::deployer_address();

    assert_eq!(
        world.state.lifecycle_series.len(),
        2,
        "the scenario issues two series"
    );
    for series in &world.state.lifecycle_series {
        assert!(
            venue_probes::series_exists(&url, nft, *series),
            "the collection does not know series {series}"
        );
        assert_eq!(
            venue_probes::series_balances(&url, nft, *series, holder),
            Some((u64::from(UNITS), 0)),
            "series {series} did not mint its units to the holder"
        );
    }
}

/// The day the chain is in, taken from its own head rather than the host clock.
fn chain_worldwide_day_offset(world: &World, port: u16, offset_secs: i64) -> u32 {
    let timestamp = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    outbe_primitives::time::worldwide_day_from_timestamp(
        timestamp.saturating_add_signed(offset_secs),
    )
}

#[when("the day advances past the qualification period")]
fn advance_past_qualification(world: &mut World) {
    // Under the test build the period is seconds, not a day: jumping a day would
    // crash the metadosis day machine, and the sweep's decision is what matters.
    let port = world.validators.primary_port();
    let target = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp")
        + QUALIFICATION_PERIOD_SECS
        + QUALIFICATION_MARGIN_SECS;
    wait_for_chain_time(world, port, target);
}

#[when("the reference rate stands above the series floor")]
fn rate_above_floor(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let series = *world
        .state
        .lifecycle_series
        .first()
        .expect("a series was issued");

    // Both series share an entry price, so one floor decides the group.
    let (_, floor, _) = venue_probes::series_prices(&url, nft, series).expect("series prices");
    crate::features::price_oracle::publish_controlled_quote(world, U256::from(floor * 2));
}

#[then("both series qualify in one group decision")]
fn both_series_qualify(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let deadline = Instant::now() + Duration::from_secs(QUALIFY_SWEEP_TIMEOUT_SECS);

    for series in world.state.lifecycle_series.clone() {
        loop {
            if venue_probes::series_state(&url, nft, series) == Some(QUALIFIED) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "series {series} never left Issued; the qualify sweep did not promote its group"
            );
            sleep(Duration::from_secs(2));
        }
    }
}

/// Wait for the chain to reach `target` in its own time; it closes the gap per block.
///
/// Reports where it actually got to, and whether it was still moving: a ratchet that
/// stalled and one that is merely slow need different answers.
fn wait_for_chain_time(world: &World, port: u16, target: u64) {
    let deadline = Instant::now() + Duration::from_secs(CATCH_UP_TIMEOUT_SECS);
    let mut first = None;
    loop {
        let now = world.rpc.latest_block_timestamp(port);
        if first.is_none() {
            first = now;
        }
        if now.is_some_and(|now| now >= target) {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "the committee never reached logical time {target}: started at {first:?}, \
                 reached {now:?} (short by {:?}s) after {CATCH_UP_TIMEOUT_SECS}s",
                now.map(|now| target.saturating_sub(now))
            );
        }
        sleep(Duration::from_secs(2));
    }
}

fn intex_nft(world: &World) -> alloy_primitives::Address {
    world
        .state
        .origin_contracts
        .as_ref()
        .expect("intex engine was deployed")
        .intex_nft
}

#[when("the holder settles part of their units")]
fn settle_part(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let currency = world
        .state
        .settlement_currency
        .expect("settlement currency was registered");
    let holder = crate::world::origin_venue::deployer_address();

    world.state.settled_units = UNITS / 2;
    for series in world.state.lifecycle_series.clone() {
        // A price the series refuses would fail here rather than inside `settle`,
        // where the revert reads as a balance problem instead of a currency one.
        let cost = test_issuance::quote_cost(&url, series, currency.asset)
            .unwrap_or_else(|| panic!("series {series} does not accept the settlement token"));
        assert!(
            !cost.is_zero(),
            "series {series} quoted a zero settlement cost"
        );

        test_issuance::settle(
            &url,
            DEPLOYER_KEY,
            series,
            holder,
            world.state.settled_units,
            currency.asset,
        )
        .expect("settle part of the holding");
    }
}

#[then("those units move from issued to settled")]
fn units_moved_to_settled(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let holder = crate::world::origin_venue::deployer_address();
    let settled = u64::from(world.state.settled_units);

    for series in &world.state.lifecycle_series {
        assert_eq!(
            venue_probes::series_balances(&url, nft, *series, holder),
            Some((u64::from(UNITS) - settled, settled)),
            "series {series} did not move exactly the settled units"
        );
    }
}

#[then("the settlement payment lands in the reserve vault")]
fn payment_in_vault(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let currency = world
        .state
        .settlement_currency
        .expect("settlement currency was registered");

    let held = settlement_currency::vault_balance(&url, currency.asset, currency.vault)
        .expect("read the reserve vault balance");
    assert!(
        !held.is_zero(),
        "the reserve vault holds nothing after settlement"
    );
}

#[when("the holder mines Promis against their settled units")]
fn mine_promis(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let nft = intex_nft(world);
    let holder = crate::world::origin_venue::deployer_address();
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");

    let keys = eth::derive_account_keys(&url, DEPLOYER_KEY, Ledger::Promis)
        .expect("derive the holder's Promis modify key");

    for series in world.state.lifecycle_series.clone() {
        let settled = venue_probes::series_balances(&url, nft, series, holder)
            .expect("series balances")
            .1;
        assert!(settled > 0, "series {series} has nothing settled to mine");

        // Promis is minted per unit at the series' load, and the engine derives the
        // same figure — a mismatch here would fail the proof rather than the mint.
        let promis_load =
            venue_probes::series_promis_load(&url, nft, series).expect("series promis load");
        let amount = U256::from(promis_load) * U256::from(settled);
        let op_nonce = eth::read_call(
            &url,
            addresses::PROMIS_ADDR,
            &IPromisNonce::opNonceOfCall { account: holder },
        )
        .expect("read the holder's Promis op nonce");
        let nonce = test_issuance::mine_nonce(holder, amount, series, 0)
            .expect("a nonce clearing one leading zero byte");
        let mac = outbe_tee_enclave::promis::modify_mac(
            &keys.modify,
            holder,
            PromisOp::Mint,
            amount,
            op_nonce,
            chain_b256(chain_id),
        );

        test_issuance::mine_promis(
            &url,
            DEPLOYER_KEY,
            series,
            u32::try_from(settled).expect("settled units fit a uint32"),
            nonce,
            mac,
            op_nonce,
        )
        .expect("mine Promis from the settled units");
    }
}

#[then("the settled units are burned and Promis is minted")]
fn settled_burned_into_promis(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let holder = crate::world::origin_venue::deployer_address();

    for series in &world.state.lifecycle_series {
        assert_eq!(
            venue_probes::series_balances(&url, nft, *series, holder).map(|(_, settled)| settled),
            Some(0),
            "series {series} still holds settled units after mining"
        );
    }
}

/// The chain id as the enclave binds it into a MAC.
fn chain_b256(chain_id: u64) -> alloy_primitives::B256 {
    alloy_primitives::B256::from(U256::from(chain_id))
}

sol! {
    interface IPromisNonce {
        function opNonceOf(address account) external view returns (uint64);
    }
}

#[when("the call trigger holds above the call price across the call window")]
fn call_trigger_holds(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let nft = intex_nft(world);
    let series = *world
        .state
        .lifecycle_series
        .first()
        .expect("a series was issued");
    let (_, _, call_price) = venue_probes::series_prices(&url, nft, series).expect("series prices");

    // DEV calls a series whose VWAP cleared the trigger on two of the last three
    // days. Seed those days rather than living through them: the Oracle's own
    // arithmetic is not what this scenario is about, and the sweep still walks its
    // index, checks the watermark and counts the days itself.
    test_issuance::seed_day_vwaps(
        &url,
        DEPLOYER_KEY,
        settlement_currency::USD_ISO,
        CALL_THRESHOLD_DAYS,
        U256::from(call_price) * U256::from(2),
    )
    .expect("seed the call-window VWAPs");
}

#[then("both series become Called")]
fn both_series_called(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let deadline = Instant::now() + Duration::from_secs(CALL_SWEEP_TIMEOUT_SECS);

    for series in world.state.lifecycle_series.clone() {
        loop {
            if venue_probes::series_state(&url, nft, series) == Some(CALLED) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "series {series} never reached Called; the call sweep did not fire"
            );
            sleep(Duration::from_secs(2));
        }
    }
}

#[when("the holder settles the remaining units inside the notice period")]
fn settle_remainder(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let currency = world
        .state
        .settlement_currency
        .expect("settlement currency was registered");
    let holder = crate::world::origin_venue::deployer_address();

    for series in world.state.lifecycle_series.clone() {
        let issued = venue_probes::series_balances(&url, nft, series, holder)
            .expect("series balances")
            .0;
        assert!(issued > 0, "series {series} has nothing left to settle");
        test_issuance::settle(
            &url,
            DEPLOYER_KEY,
            series,
            holder,
            u32::try_from(issued).expect("issued units fit a uint32"),
            currency.asset,
        )
        .expect("settle the remainder under Called");
    }
}

#[then("no issued units remain and every unit is settled")]
fn everything_settled(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let holder = crate::world::origin_venue::deployer_address();

    for series in &world.state.lifecycle_series {
        assert_eq!(
            venue_probes::series_balances(&url, nft, *series, holder),
            Some((0, u64::from(UNITS))),
            "series {series} did not end with every unit settled"
        );
    }
}
