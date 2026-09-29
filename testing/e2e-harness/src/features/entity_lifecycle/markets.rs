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

const DEPLOY_FUNDING_COEN: u64 = 100;
/// A pricing window closes on a whole hour; the margin lands the committee inside the next one.
const WINDOW_CLOSE_MARGIN_SECS: u64 = 60;
const WINDOW_CLOSE_TIMEOUT: Duration = Duration::from_secs(300);

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

#[when("the settlement currency is registered on the committee chain")]
fn register_settlement_currency(world: &mut World) {
    let currency = register_currency(world, settlement_currency::USD_ISO);
    world.state.settlement_currency = Some(currency);
}

#[then("owners may settle in that currency")]
fn settlement_currency_is_acceptable(world: &mut World) {
    let currency = world
        .state
        .settlement_currency
        .expect("settlement currency was registered");
    assert_currency_routes(world, currency, settlement_currency::USD_ISO);
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
    let url = world.rpc.url(world.validators.primary_port());
    let mut price_ready = pending.is_none();
    poll_until(
        WINDOW_CLOSE_TIMEOUT,
        || format!("the closed pricing window never priced COEN in {currencies:?}"),
        || {
            let time_ready = head_time(world) >= target;
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
