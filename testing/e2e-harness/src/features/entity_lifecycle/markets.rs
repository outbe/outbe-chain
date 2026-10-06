//! The accounts, currencies and pricing window a lifecycle scenario settles against.

use std::time::Duration;

use alloy_primitives::U256;
use cucumber::{given, then, when};
use outbe_primitives::addresses::ORACLE_ADDRESS;

use super::chain::{finalized_checkpoint, head_time, poll_until, verify_checkpoint};
use crate::env::environment;
use crate::internal::eth;
use crate::world::settlement_currency::{self, SettlementCurrency};
use crate::world::World;

alloy_sol_types::sol! {
    interface ISettlementTokenDecimals {
        function decimals() external view returns (uint8);
    }
}

/// Malaysian ringgit: the issuance currency lifecycle entities carry beside USD, which
/// no reference currency list includes.
pub(crate) const MYR_ISO: u16 = 458;
/// Ringgit per COEN on the controlled feed, at six decimals. The value is not round, so
/// a converted cost leaves a remainder to floor.
pub(crate) const MYR_RATE_MINOR: u64 = 4_512_345;
/// Euro: registered with its own vault, and foreign to the MYR-issued holdings.
pub(crate) const EUR_ISO: u16 = 978;
/// Scenarios with this tag price and settle in MYR as well as USD.
const ISSUANCE_MARKET_TAG: &str = "myr-issuance";

const DEPLOY_FUNDING_COEN: u64 = 100;
/// A pricing window closes on a whole hour. The margin lands the committee inside the next one.
const WINDOW_CLOSE_MARGIN_SECS: u64 = 60;
const WINDOW_CLOSE_TIMEOUT: Duration = Duration::from_secs(300);

/// Choose the scenario's markets before its genesis is written: a tagged scenario gets a
/// COEN/MYR pair in genesis and on every feeder.
pub(crate) fn configure(world: &mut World, tags: &[String]) {
    if tags.iter().any(|tag| tag == ISSUANCE_MARKET_TAG) {
        world.state.issuance_market = Some(MYR_ISO);
        world.price_oracle.add_fixed_pair(
            MYR_ISO,
            U256::from(MYR_RATE_MINOR),
            crate::features::price_oracle::MOCK_VOLUME,
        );
    }
}

/// The genesis Oracle registry for a scenario's markets, or `None` to keep the seed's.
pub(crate) fn genesis_oracle_pairs(world: &World) -> Option<Vec<(String, String, String)>> {
    let iso = world.state.issuance_market?;
    Some(vec![
        (
            "COEN".into(),
            settlement_currency::USD_ISO.to_string(),
            "1000000".into(),
        ),
        ("COEN".into(), iso.to_string(), MYR_RATE_MINOR.to_string()),
    ])
}

/// COEN in `iso` on the controlled feed, which every closed window and seeded day repeats.
pub(crate) fn coen_rate(iso: u16) -> U256 {
    match iso {
        settlement_currency::USD_ISO => crate::features::price_oracle::EXPECTED_RATE,
        MYR_ISO => U256::from(MYR_RATE_MINOR),
        other => panic!("no controlled COEN quote in {other}"),
    }
}

/// The currencies a lifecycle scenario prices: USD, and its issuance market if it has one.
pub(crate) fn priced_currencies(world: &World) -> Vec<u16> {
    std::iter::once(settlement_currency::USD_ISO)
        .chain(world.state.issuance_market)
        .collect()
}

/// The registered settlement currency answering `iso_code`.
pub(crate) fn currency(world: &World, iso_code: u16) -> SettlementCurrency {
    *world
        .state
        .currencies
        .get(&iso_code)
        .unwrap_or_else(|| panic!("no settlement currency was registered for {iso_code}"))
}

#[given("the deploy account is funded on the committee chain")]
fn fund_deploy_account(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    // Genesis funds validators, not the account that deploys fixtures and arms the hooks.
    let funder = world
        .validators
        .get(0)
        .evm_key()
        .expect("validator-0 funding key");
    eth::send_value(
        &url,
        crate::world::origin_venue::deployer_address(),
        &funder,
        eth::coen(DEPLOY_FUNDING_COEN),
    )
    .expect("fund the deploy account");
}

/// USD, the issuance market and a foreign EUR: every currency a payment names.
#[when("the settlement currencies are registered on the committee chain")]
fn register_settlement_currencies(world: &mut World) {
    let isos = priced_currencies(world)
        .into_iter()
        .chain(std::iter::once(EUR_ISO))
        .collect::<Vec<_>>();
    for iso in isos {
        let currency = register_currency(world, iso);
        world.state.currencies.insert(iso, currency);
    }
}

#[then("owners may settle in each of them")]
fn settlement_currencies_are_acceptable(world: &mut World) {
    for (&iso, &currency) in &world.state.currencies {
        assert_currency_routes(world, currency, iso);
    }
}

#[then("the pricing window closes over those quotes")]
fn pricing_window_closes(world: &mut World) {
    let currencies = priced_currencies(world);
    close_price_window(world, &currencies);
}

/// Deploy a six-decimal stablecoin answering `iso_code` with its vault, and register the
/// vault with the VaultRouter. The vault starts empty on every validator.
pub(crate) fn register_currency(world: &World, iso_code: u16) -> SettlementCurrency {
    let url = world.rpc.url(world.validators.primary_port());
    // `addVault` admits the router's owner alone, and genesis seeds that to validator 0.
    let owner_key = world
        .validators
        .get(0)
        .evm_key()
        .expect("VaultRouter owner key");
    let currency = settlement_currency::deploy_for(
        &environment().repo.join("contracts/intex"),
        &url,
        &owner_key,
        iso_code,
    )
    .expect("register the settlement currency");

    let checkpoint = finalized_checkpoint(world);
    for port in world.validators.committee_ports() {
        let url = world.rpc.url(port);
        assert_eq!(
            eth::read_call_at(
                &url,
                currency.asset,
                &ISettlementTokenDecimals::decimalsCall {},
                checkpoint.height
            ),
            Some(6),
            "the {iso_code} settlement token must have six decimals"
        );
        assert_eq!(
            settlement_currency::vault_balance_at(
                &url,
                currency.asset,
                currency.vault,
                checkpoint.height
            ),
            Some(U256::ZERO),
            "the {iso_code} vault must start empty on port {port}"
        );
    }
    verify_checkpoint(world, checkpoint);
    currency
}

/// The VaultRouter routes the asset to its vault, and the asset answers `iso_code`.
pub(crate) fn assert_currency_routes(world: &World, currency: SettlementCurrency, iso_code: u16) {
    let url = world.rpc.url(world.validators.primary_port());
    assert_eq!(
        settlement_currency::registered_vaults(&url, currency.asset),
        vec![currency.vault],
        "the VaultRouter does not route the {iso_code} asset to its vault"
    );
    assert_eq!(
        settlement_currency::iso_code(&url, currency.asset),
        Some(iso_code),
        "the settlement asset does not answer {iso_code}"
    );
}

/// Move the committee past the next whole hour, so every quote published so far lies in
/// a closed pricing window, and wait until that window prices each of `currencies`.
pub(crate) fn close_price_window(world: &mut World, currencies: &[u16]) {
    let head = head_time(world);
    let target = head - head % 3_600 + 3_600 + WINDOW_CLOSE_MARGIN_SECS;
    let (_, _, _, pending) =
        crate::features::ocomp::restart_committee_at_logical_time(world, target);
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let mut price_ready = pending.is_none();
    poll_until(
        WINDOW_CLOSE_TIMEOUT,
        || format!("the closed pricing window never priced COEN in {currencies:?}"),
        || {
            // The committee has just restarted, so a head read may briefly fail.
            let time_ready = world
                .rpc
                .latest_block_timestamp(port)
                .is_some_and(|now| now >= target);
            price_ready = price_ready
                || pending.as_ref().is_some_and(|pending| {
                    crate::features::price_oracle::observe_pending_publication(world, pending)
                });
            time_ready
                && price_ready
                && currencies.iter().all(|&currency| {
                    window_vwap(&url, currency).is_some_and(|vwap| !vwap.is_zero())
                })
        },
    );
}

/// The finalized pricing-window VWAP of COEN in `currency` at the current snapshot.
pub(crate) fn window_vwap(url: &str, currency: u16) -> Option<U256> {
    let snapshot = eth::read_call(url, ORACLE_ADDRESS, &eth::IOracle::getVwapSnapshotIdCall {})?;
    eth::read_call(
        url,
        ORACLE_ADDRESS,
        &eth::IOracle::getFinalizedWindowVwapCall {
            currency,
            snapshotId: snapshot,
        },
    )
}
