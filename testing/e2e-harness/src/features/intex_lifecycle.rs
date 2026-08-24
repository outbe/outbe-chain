//! An Intex after the mint: qualification, settlement, and the burn into Promis.

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
