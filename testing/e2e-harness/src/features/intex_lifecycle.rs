//! An Intex after the mint: qualification, settlement, and the burn into Promis.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
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
const QUALIFICATION_PERIOD_SECS: u64 = 24 * 3600;
const QUALIFICATION_MARGIN_SECS: u64 = 3600;
/// Long enough for the chain to close a one-day gap, which it does per block.
const CATCH_UP_TIMEOUT_SECS: u64 = 600;
/// The sweep runs in begin-block; a handful of blocks is plenty.
const QUALIFY_SWEEP_TIMEOUT_SECS: u64 = 180;
/// `IntexState::Qualified`.
const QUALIFIED: u8 = 1;

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

    // Same day and reference currency, different issuance currencies: one group,
    // two members, which is what makes the group promotion and the mark batch real.
    let series = test_issuance::issue_series(
        &url,
        DEPLOYER_KEY,
        chain_worldwide_day(world, port),
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
fn chain_worldwide_day(world: &World, port: u16) -> u32 {
    let timestamp = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    outbe_primitives::time::worldwide_day_from_timestamp(timestamp)
}

#[when("the day advances past the qualification period")]
fn advance_past_qualification(world: &mut World) {
    let port = world.validators.primary_port();
    let target = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp")
        + QUALIFICATION_PERIOD_SECS
        + QUALIFICATION_MARGIN_SECS;

    // The restart resumes the price feeder itself; only the catch-up is ours to await.
    crate::features::ocomp::restart_committee_at_logical_time(world, target);
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
fn wait_for_chain_time(world: &World, port: u16, target: u64) {
    let deadline = Instant::now() + Duration::from_secs(CATCH_UP_TIMEOUT_SECS);
    loop {
        if world
            .rpc
            .latest_block_timestamp(port)
            .is_some_and(|now| now >= target)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the committee never reached the requested logical time"
        );
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
