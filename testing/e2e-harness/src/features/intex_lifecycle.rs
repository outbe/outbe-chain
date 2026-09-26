//! An Intex after the mint: qualification, settlement, and the burn into Promis.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{FixedBytes, U256};
use alloy_sol_types::sol;
use base64::Engine as _;
use outbe_tee::protocol::{Ledger, PromisOp};

use crate::internal::{addresses, eth};
use cucumber::{then, when};

use crate::env::environment;
use crate::world::forge::DEPLOYER_KEY;
use crate::world::relay::{Relay, RelayEnd};
use crate::world::rpc::FinalizedCheckpoint;
use crate::world::settlement_currency::{self, SettlementCurrency};
use crate::world::test_issuance::{self, SeriesSpec};
use crate::world::{venue_probes, World};

sol! {
    interface ILifecyclePaymentToken {
        function decimals() external view returns (uint8);
    }

    interface IIntexCard {
        function issuedTokenId(bytes14 seriesId) external pure returns (uint256);
        function uri(uint256 tokenId) external view returns (string memory);
        function vwapSource() external view returns (address);
    }

    interface IVwapSource {
        function maxUtcDayVwapSince(uint16 isoCode, uint32 fromUtcDay) external view returns (uint256);
    }
}

/// Every series carries the same entry price, so their call prices share a bin and
/// one sweep pass calls them together.
const ENTRY_PRICE_MINOR: u64 = 1_000_000;
/// PROMIS-units per Intex unit, on the wire scale.
const PROMIS_LOAD_MINOR: u128 = 100_000;
/// Units each series mints to the owner; settled in two goes, so keep it even.
/// Units each series mints per chain. The holding is split so bringing units home
/// is a real step rather than a formality.
const COMMITTEE_UNITS: u32 = 4;
const TARGET_UNITS: u32 = 6;
/// Brought home while the series are still tradable; the rest travels under Called,
/// where the bridge admits a move only to the owner's own address.
const TRADABLE_HOP_UNITS: u32 = 2;
const UNITS: u32 = COMMITTEE_UNITS + TARGET_UNITS;
/// USD (840) as the reference for every series, spelled `U` in the series id.
const REFERENCE_BYTE: u8 = b'U';
/// Long enough for the chain to close a one-day gap, which it does per block.
const CATCH_UP_TIMEOUT_SECS: u64 = 900;
/// Qualification is read off the seeded day, so it waits only for that block.
const QUALIFY_TIMEOUT_SECS: u64 = 180;
/// The sender fires every minute in e2e, then the relay carries the day over.
const VWAP_PUSH_TIMEOUT_SECS: u64 = 600;
/// `IntexState::Called`.
const CALLED: u8 = 2;
/// Derived against the clock on both chains; never written by anything.
const EXPIRED: u8 = 3;
/// Slack past the deadline so the sweep has a block to run in.
const EXPIRY_MARGIN_SECS: u64 = 30;
/// The expiry queue buckets deadlines by the hour they fall in.
const EXPIRY_BUCKET_SECS: u64 = 3_600;
/// Settled out of the expiring series, so the forfeit is the tirage less these.
const EXPIRING_SETTLED_UNITS: u32 = 2;
/// The credit lands in the block the sweep reaches the queue head.
const FORFEIT_TIMEOUT_SECS: u64 = 180;
/// DEV calls a series once the VWAP held above the call price on two of three days.
/// DEV requires two of the last three days above the trigger.
const CALL_THRESHOLD_DAYS: u32 = 2;
/// How far back the series are issued so closed days exist after their issuance.
const CALL_LOOKBACK_DAYS: u32 = 3;
/// A relayed message is asynchronous; scenarios wait for arrival rather than assume it.
const DELIVERY_TIMEOUT_SECS: u64 = 180;
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
    let checkpoint = lifecycle_checkpoint(world);
    for port in world.validators.committee_ports() {
        assert_eq!(
            eth::read_call_at(
                &world.rpc.url(port),
                currency.asset,
                &ILifecyclePaymentToken::decimalsCall {},
                checkpoint.height
            ),
            Some(6),
            "lifecycle fixture payment token must have six decimals"
        );
    }
    verify_checkpoint(world, checkpoint);
    assert_vault_payment(world, 0);
}

#[then("owners may settle in that currency")]
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

#[when("four test Intex series sharing a reference currency are issued to a funded owner")]
fn issue_two_series(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");
    let asset = world
        .state
        .settlement_currency
        .expect("settlement currency was registered")
        .asset;
    let owner = crate::world::origin_venue::deployer_address();

    // Enough to settle every unit of every series at any price the sweeps derive.
    test_issuance::fund_settler(&url, asset, DEPLOYER_KEY, U256::from(u64::MAX))
        .expect("fund the settling owner");

    let origin_router = world
        .state
        .origin_contracts
        .as_ref()
        .expect("intex engine was deployed")
        .origin_router;

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
        origin_router,
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
        // Issuance is stamped where the seeded days already lie behind it.
        u32::try_from(
            world
                .rpc
                .latest_block_timestamp(port)
                .expect("committee head timestamp")
                .saturating_sub(u64::from(CALL_LOOKBACK_DAYS) * 86_400),
        )
        .expect("backdated stamp fits a uint32"),
        settlement_currency::USD_ISO,
        REFERENCE_BYTE,
        U256::from(ENTRY_PRICE_MINOR),
        PROMIS_LOAD_MINOR,
        owner,
        &[COMMITTEE_UNITS, TARGET_UNITS],
        &[
            u32::try_from(chain_id).expect("committee chain id fits a uint32"),
            u32::try_from(world.target_chain.chain_id()).expect("target chain id fits a uint32"),
        ],
        &[
            SeriesSpec {
                issuance: *b"USD",
                issuance_currency: settlement_currency::USD_ISO,
            },
            SeriesSpec {
                issuance: *b"EUR",
                issuance_currency: 978,
            },
            // Only part of this one is settled, so it is still holding units when the
            // notice runs out.
            SeriesSpec {
                issuance: *b"GBP",
                issuance_currency: 826,
            },
            // Nobody touches this one at all, so the sweep forfeits its whole tirage
            // and the two together prove the subtraction rather than one case of it.
            SeriesSpec {
                issuance: *b"JPY",
                issuance_currency: 392,
            },
        ],
    )
    .expect("issue the lifecycle series");

    // The capacity committee runs ahead of wall time, so advance Anvil only
    // after all legs have their timestamps fixed.
    let issued_through = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee timestamp after issuance");
    let target_url = world
        .target_chain
        .rpc_url()
        .expect("target chain is running");
    if eth::latest_block_timestamp(&target_url).expect("target timestamp before delivery")
        < issued_through
    {
        world
            .target_chain
            .sync_clock_to(issued_through)
            .expect("synchronize target time after issuance");
    }
    assert!(
        eth::latest_block_timestamp(&target_url)
            .expect("verify target timestamp after synchronization")
            >= issued_through,
        "target clock remains behind the issuing committee block"
    );

    let mut series = series;
    let untouched = series.pop().expect("the untouched series was issued last");
    let expiring = series
        .pop()
        .expect("the expiring series was issued next to last");
    world.state.lifecycle_series = series;
    world.state.expiring_series = Some(expiring);
    world.state.untouched_series = Some(untouched);
    world.state.lifecycle_day = Some(day);
}

#[then("the owner holds issued units of every series on each chain")]
fn owner_holds_issued_units(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let target_url = target_rpc_url(world);
    let nft = intex_nft(world);
    let target_nft = target_intex_nft(world);
    let owner = crate::world::origin_venue::deployer_address();

    assert_eq!(
        world.state.lifecycle_series.len(),
        2,
        "the scenario issues two series"
    );

    // The target-chain leg travels as a real message, so give the relay its round.
    let deadline = Instant::now() + Duration::from_secs(DELIVERY_TIMEOUT_SECS);
    for series in world.state.lifecycle_series.clone() {
        assert!(
            venue_probes::series_exists(&url, nft, series),
            "the committee collection does not know series {series}"
        );
        assert_eq!(
            venue_probes::series_balances(&url, nft, series, owner),
            Some((u64::from(COMMITTEE_UNITS), 0)),
            "series {series} did not mint its committee units to the owner"
        );
        loop {
            if venue_probes::series_balances(&target_url, target_nft, series, owner)
                == Some((u64::from(TARGET_UNITS), 0))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "series {series} did not reach its exact target-chain balance before the delivery deadline"
            );
            // The committee runs on a logical clock days ahead of real time, and the
            // issuance carries its stamps: the target rejects them as its own future
            // until its clock is carried over, and the relay retries in silence.
            carry_target_clock(world);
            sleep(Duration::from_secs(2));
        }
    }
}

/// Carry the committee's logical clock over to the target chain. Nothing on a
/// localnet plays the operator who keeps a second chain in step, and a stamp that
/// sits in the target's future is refused rather than queued.
fn carry_target_clock(world: &World) {
    let Some(now) = world
        .rpc
        .latest_block_timestamp(world.validators.primary_port())
    else {
        return;
    };
    let _ = world.target_chain.sync_clock_to(now);
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

    // Both series share an entry price, so one floor decides the group. Qualification
    // reads the closed day's VWAP, so that day is seeded like the call window's.
    let (_, floor, _) = venue_probes::series_prices(&url, nft, series).expect("series prices");
    test_issuance::seed_day_vwaps(
        &url,
        DEPLOYER_KEY,
        settlement_currency::USD_ISO,
        1,
        U256::from(floor * 2),
    )
    .expect("seed the closed day's VWAP");
}

#[then("every series qualifies on the seeded day")]
fn both_series_qualify(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let deadline = Instant::now() + Duration::from_secs(QUALIFY_TIMEOUT_SECS);

    for series in world.state.lifecycle_series.clone() {
        loop {
            let qualified = eth::read_call(
                &url,
                addresses::INTEX_FACTORY_ADDR,
                &eth::IIntexFactory::isSeriesQualifiedCall { seriesId: series },
            );
            if qualified == Some(true) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "series {series} did not qualify on the seeded day"
            );
            sleep(Duration::from_secs(2));
        }
    }
}

/// The origin's collection reads the IntexFactory; the target's reads the registry the origin pushes to.
#[then("every series card reads Qualified on both chains")]
fn cards_read_qualified(world: &mut World) {
    let chains = [
        (
            world.rpc.url(world.validators.primary_port()),
            intex_nft(world),
        ),
        (target_rpc_url(world), target_intex_nft(world)),
    ];
    let deadline = Instant::now() + Duration::from_secs(VWAP_PUSH_TIMEOUT_SECS);

    for (url, nft) in &chains {
        let source = eth::read_call(url, *nft, &IIntexCard::vwapSourceCall {})
            .expect("the collection names its VWAP source");
        for series in world.state.lifecycle_series.clone() {
            loop {
                let state = card_state(url, *nft, series);
                if state.as_deref() == Some("Qualified") {
                    break;
                }
                let (iso_code, floor, from) = venue_probes::series_floor_terms(url, *nft, series)
                    .expect("the chain knows the series");
                let max = eth::read_call(
                    url,
                    source,
                    &IVwapSource::maxUtcDayVwapSinceCall {
                        isoCode: iso_code,
                        fromUtcDay: from,
                    },
                );
                assert!(
                    Instant::now() < deadline,
                    "the card of {series} on {url} reads {state:?}; its source {source} has {max:?} against floor {floor}"
                );
                sleep(Duration::from_secs(5));
            }
        }
    }
}

/// The `Series State` trait of the issued class's card.
fn card_state(url: &str, nft: alloy_primitives::Address, series: FixedBytes<14>) -> Option<String> {
    let token = eth::read_call(
        url,
        nft,
        &IIntexCard::issuedTokenIdCall { seriesId: series },
    )?;
    let uri = eth::read_call(url, nft, &IIntexCard::uriCall { tokenId: token })?;
    let json = base64::engine::general_purpose::STANDARD
        .decode(uri.strip_prefix("data:application/json;base64,")?)
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&json).ok()?;
    json["attributes"]
        .as_array()?
        .iter()
        .find(|attribute| attribute["trait_type"] == "Series State")?["value"]
        .as_str()
        .map(str::to_owned)
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

/// The same collection on the target chain, where the other half of every series lives.
fn target_intex_nft(world: &World) -> alloy_primitives::Address {
    world
        .state
        .target_contracts
        .as_ref()
        .expect("intex venue was deployed on the target chain")
        .intex_nft
}

fn target_rpc_url(world: &World) -> String {
    world
        .target_chain
        .rpc_url()
        .expect("target chain is running")
}

#[when("the owner settles part of their units")]
fn settle_part(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let currency = world
        .state
        .settlement_currency
        .expect("settlement currency was registered");
    let owner = crate::world::origin_venue::deployer_address();

    // Settle what is already home. The rest is on the target chain and cannot be
    // settled until the owner brings it back, which is a later step.
    world.state.settled_units = COMMITTEE_UNITS + TRADABLE_HOP_UNITS;
    for series in world.state.lifecycle_series.clone() {
        let issued = venue_probes::series_balances(&url, nft, series, owner)
            .expect("series balances")
            .0;
        assert_eq!(
            issued,
            u64::from(COMMITTEE_UNITS + TRADABLE_HOP_UNITS),
            "series {series} does not hold what was minted here plus what came home"
        );

        // A price the series refuses would fail here rather than inside `settle`,
        // where the revert reads as a balance problem instead of a currency one.
        let units = COMMITTEE_UNITS + TRADABLE_HOP_UNITS;
        let cost = test_issuance::quote_cost(&url, series, currency.asset, units)
            .unwrap_or_else(|| panic!("series {series} does not accept the settlement token"));
        assert_eq!(
            cost,
            expected_settlement_cost() * U256::from(units),
            "series {series} settlement quote differs from fixture entry price and load"
        );

        let proof = settlement_note(
            world,
            owner,
            currency.asset,
            expected_settlement_cost(),
            units,
        );
        test_issuance::settle(&url, DEPLOYER_KEY, series, owner, units, &proof)
            .expect("settle the units at home");
    }
}

/// One note per settle: a nullifier is booked once, so notes never carry over.
fn settlement_note(
    world: &World,
    owner: alloy_primitives::Address,
    asset: alloy_primitives::Address,
    per_unit: U256,
    units: u32,
) -> Vec<u8> {
    let total = per_unit
        .checked_mul(U256::from(units))
        .expect("settlement cost fits a U256");
    crate::features::paynote::deposit_and_prove(
        world,
        world.validators.primary_port(),
        DEPLOYER_KEY,
        owner,
        asset,
        total,
    )
}

#[then("those units move from issued to settled")]
fn units_moved_to_settled(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let owner = crate::world::origin_venue::deployer_address();

    for series in &world.state.lifecycle_series {
        assert_eq!(
            venue_probes::series_balances(&url, nft, *series, owner),
            Some((0, u64::from(COMMITTEE_UNITS + TRADABLE_HOP_UNITS))),
            "series {series} left issued units at home after settling"
        );
    }
}

#[then("the settlement payment lands in the reserve vault")]
fn payment_in_vault(world: &mut World) {
    assert_vault_payment(world, COMMITTEE_UNITS + TRADABLE_HOP_UNITS);
}

fn expected_settlement_cost() -> U256 {
    // Entry price and load each use six decimals; the fixture pays in its
    // six-decimal reference USD asset, so no cross-currency conversion applies.
    // The product divides exactly, so the per-unit cost times the units settled
    // is what the chain's single floor over the whole operation charges.
    U256::from(ENTRY_PRICE_MINOR) * U256::from(PROMIS_LOAD_MINOR) / U256::from(1_000_000)
}

fn lifecycle_checkpoint(world: &World) -> FinalizedCheckpoint {
    // Transaction helpers have already required a successful mined receipt.
    // Finalize at least that primary head on every port before reading state.
    let head = world
        .rpc
        .head(world.validators.primary_port())
        .expect("lifecycle primary head");
    world
        .rpc
        .wait_finalized_checkpoint(&world.validators.committee_ports(), head, 120)
        .expect("finalized lifecycle checkpoint on every validator")
}

fn verify_checkpoint(world: &World, checkpoint: FinalizedCheckpoint) {
    for port in world.validators.committee_ports() {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, checkpoint.height)
                .expect("lifecycle checkpoint"),
            checkpoint
        );
    }
}

fn assert_vault_payment(world: &World, settled_per_series: u32) {
    let currency = world
        .state
        .settlement_currency
        .expect("registered settlement currency");
    let expected = expected_settlement_cost()
        * U256::from(settled_per_series)
        * U256::from(world.state.lifecycle_series.len());
    let checkpoint = lifecycle_checkpoint(world);
    for port in world.validators.committee_ports() {
        let balance = settlement_currency::vault_balance_at(
            &world.rpc.url(port),
            currency.asset,
            currency.vault,
            checkpoint.height,
        )
        .expect("finalized settlement vault token balance");
        assert_eq!(
            balance, expected,
            "vault payment differs from fixture cost times units on port {port}"
        );
    }
    verify_checkpoint(world, checkpoint);
    eprintln!("INTEX_VAULT_EXPECTATION height={} hash={} state_root={} cost={} units_per_series={} series={} expected={expected}",
        checkpoint.height, checkpoint.block_hash, checkpoint.state_root, expected_settlement_cost(), settled_per_series, world.state.lifecycle_series.len());
}

fn promis_on_all_validators(world: &World, view_key: &[u8; 32]) -> U256 {
    let owner = crate::world::origin_venue::deployer_address();
    let checkpoint = lifecycle_checkpoint(world);
    let mut common = None;
    for port in world.validators.committee_ports() {
        let blob = eth::read_call_at(
            &world.rpc.url(port),
            addresses::PROMIS_ADDR,
            &eth::IPromis::balanceOfCall { account: owner },
            checkpoint.height,
        )
        .expect("finalized lifecycle Promis ciphertext");
        let amount = if blob.is_empty() {
            U256::ZERO
        } else {
            outbe_tee_enclave::promis::decrypt_balance(view_key, owner, blob.as_ref())
                .expect("decrypt finalized lifecycle Promis balance")
        };
        if let Some(common) = common {
            assert_eq!(
                amount, common,
                "Promis balance differs on validator port {port}"
            );
        } else {
            common = Some(amount);
        }
    }
    verify_checkpoint(world, checkpoint);
    let balance = common.expect("nonempty lifecycle validator cohort");
    eprintln!(
        "INTEX_PROMIS_OBSERVATION height={} hash={} state_root={} balance={balance}",
        checkpoint.height, checkpoint.block_hash, checkpoint.state_root
    );
    balance
}

#[when("the owner mines Promis against their settled units")]
fn mine_promis(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let nft = intex_nft(world);
    let owner = crate::world::origin_venue::deployer_address();
    let chain_id = world.rpc.chain_id(port).expect("committee chain id");

    let keys = eth::derive_account_keys(&url, DEPLOYER_KEY, Ledger::Promis)
        .expect("derive the owner's Promis modify key");
    world.state.promis_before_mining = Some(promis_on_all_validators(world, &keys.view));

    for series in world.state.lifecycle_series.clone() {
        let settled = venue_probes::series_balances(&url, nft, series, owner)
            .expect("series balances")
            .1;
        assert_eq!(
            settled,
            u64::from(UNITS),
            "series {series} must have every fixture unit settled"
        );

        // Promis is minted per unit at the series' load, and the engine derives the
        // same figure - a mismatch here would fail the proof rather than the mint.
        let promis_load =
            venue_probes::series_promis_load(&url, nft, series).expect("series promis load");
        assert_eq!(
            promis_load, PROMIS_LOAD_MINOR,
            "series {series} load differs from issuance fixture"
        );
        let amount = U256::from(PROMIS_LOAD_MINOR) * U256::from(UNITS);
        let op_nonce = eth::read_call(
            &url,
            addresses::PROMIS_ADDR,
            &IPromisNonce::opNonceOfCall { account: owner },
        )
        .expect("read the owner's Promis op nonce");
        let nonce = test_issuance::mine_nonce(owner, amount, series, 0)
            .expect("a nonce clearing one leading zero byte");
        let mac = outbe_tee_enclave::promis::modify_mac(
            &keys.modify,
            owner,
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

#[then("the settled units are burned and Promis is mined")]
fn settled_burned_into_promis(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let owner = crate::world::origin_venue::deployer_address();

    for series in &world.state.lifecycle_series {
        assert_eq!(
            venue_probes::series_balances(&url, nft, *series, owner).map(|(_, settled)| settled),
            Some(0),
            "series {series} still holds settled units after mining"
        );
        // The engine counts what it burned, so the series' classes stay disjoint.
        let counts = eth::read_call(
            &url,
            test_issuance::INTEX_FACTORY,
            &IIntexFactoryCounts::seriesUnitCountsCall { seriesId: *series },
        )
        .expect("series unit counts");
        assert_eq!(
            counts.exercisedUnits, UNITS,
            "series {series} did not count its exercised units"
        );
        assert_eq!(
            counts.settledUnits, 0,
            "series {series} left units counted as settled"
        );
    }
    let keys = eth::derive_account_keys(&url, DEPLOYER_KEY, Ledger::Promis)
        .expect("derive owner Promis view key");
    let expected_mint = U256::from(PROMIS_LOAD_MINOR)
        * U256::from(UNITS)
        * U256::from(world.state.lifecycle_series.len());
    let before = world
        .state
        .promis_before_mining
        .expect("Promis balance before mining");
    assert_eq!(
        promis_on_all_validators(world, &keys.view),
        before
            .checked_add(expected_mint)
            .expect("expected Promis balance fits U256"),
        "burned units did not mint the exact fixture Promis load"
    );
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

sol! {
    interface IIntexFactoryCounts {
        struct UnitCounts {
            uint32 issuedUnits;
            uint32 activeUnits;
            uint32 settledUnits;
            uint32 exercisedUnits;
            uint32 gemFactoryUnits;
            uint32 forfeitedUnits;
        }

        function seriesUnitCounts(bytes14 seriesId) external view returns (UnitCounts memory);
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

#[then("every series becomes Called")]
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

#[when("the owner settles the remaining units inside the notice period")]
fn settle_remainder(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let currency = world
        .state
        .settlement_currency
        .expect("settlement currency was registered");
    let owner = crate::world::origin_venue::deployer_address();

    for series in world.state.lifecycle_series.clone() {
        let issued = venue_probes::series_balances(&url, nft, series, owner)
            .expect("series balances")
            .0;
        assert_eq!(
            issued,
            u64::from(TARGET_UNITS - TRADABLE_HOP_UNITS),
            "series {series} remaining issued units differ from fixture"
        );
        let units = u32::try_from(issued).expect("issued units fit a uint32");
        let cost = test_issuance::quote_cost(&url, series, currency.asset, units)
            .unwrap_or_else(|| panic!("series {series} does not accept the settlement token"));
        assert_eq!(
            cost,
            expected_settlement_cost() * U256::from(units),
            "Called series {series} settlement quote differs from fixture"
        );
        let proof = settlement_note(
            world,
            owner,
            currency.asset,
            expected_settlement_cost(),
            units,
        );
        test_issuance::settle(&url, DEPLOYER_KEY, series, owner, units, &proof)
            .expect("settle the remainder under Called");
    }
}

#[then("no issued units remain of the pair being settled whole")]
fn everything_settled(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let owner = crate::world::origin_venue::deployer_address();

    for series in &world.state.lifecycle_series {
        assert_eq!(
            venue_probes::series_balances(&url, nft, *series, owner),
            Some((0, u64::from(UNITS))),
            "series {series} did not end with every unit settled"
        );
    }
    assert_vault_payment(world, UNITS);
}

#[when("a relay carries messages between the two chains")]
fn start_relay(world: &mut World) {
    let port = world.validators.primary_port();
    let committee = RelayEnd {
        url: world.rpc.url(port),
        mailbox: world
            .state
            .origin_contracts
            .as_ref()
            .expect("intex engine was deployed")
            .mailbox,
        domain: u32::try_from(world.rpc.chain_id(port).expect("committee chain id"))
            .expect("committee chain id fits a uint32"),
    };
    let target = RelayEnd {
        url: world
            .target_chain
            .rpc_url()
            .expect("target chain is running"),
        mailbox: world
            .state
            .target_contracts
            .as_ref()
            .expect("intex venue was deployed on the target chain")
            .mailbox,
        domain: u32::try_from(world.target_chain.chain_id())
            .expect("target chain id fits a uint32"),
    };

    // The NFT bridges have to know each other before either can quote a hop; nothing
    // in the deploy scripts pairs them, so the scenario that uses both does it.
    let committee_bridge = world
        .state
        .origin_contracts
        .as_ref()
        .expect("intex engine was deployed")
        .nft_bridge;
    let target_bridge = world
        .state
        .target_contracts
        .as_ref()
        .expect("intex venue was deployed on the target chain")
        .nft_bridge;
    test_issuance::set_remote_messenger(
        &committee.url,
        DEPLOYER_KEY,
        committee_bridge,
        world.target_chain.chain_id(),
        target_bridge,
    )
    .expect("point the committee bridge at the target chain");
    test_issuance::set_remote_messenger(
        &target.url,
        DEPLOYER_KEY,
        target_bridge,
        u64::from(committee.domain),
        committee_bridge,
    )
    .expect("point the target bridge home");

    world.relay = Some(Relay::start(committee, target, DEPLOYER_KEY.to_owned()));
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

#[when("the owner brings part of the target-chain units home")]
fn bridge_part_home(world: &mut World) {
    bring_home(world, TRADABLE_HOP_UNITS);
}

#[when("the owner brings the remaining units home to their own address in one batch")]
fn bridge_rest_home(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let target_url = world
        .target_chain
        .rpc_url()
        .expect("target chain is running");
    let nft = intex_nft(world);
    let bridge = world
        .state
        .target_contracts
        .as_ref()
        .expect("intex venue was deployed on the target chain")
        .nft_bridge;
    let owner = crate::world::origin_venue::deployer_address();
    let home_chain = u32::try_from(world.rpc.chain_id(port).expect("committee chain id"))
        .expect("fits a uint32");
    let amount = TARGET_UNITS - TRADABLE_HOP_UNITS;

    // A owner with more than one series moves them together, so this hop takes the
    // batch route the first one did not: one burn set here, one mint set at home.
    let tokens: Vec<(alloy_primitives::U256, u32)> = world
        .state
        .lifecycle_series
        .iter()
        .map(|series| {
            (
                venue_probes::issued_token_id(&url, nft, *series).expect("issued token id"),
                amount,
            )
        })
        .collect();
    let before: Vec<u64> = world
        .state
        .lifecycle_series
        .iter()
        .map(|series| {
            venue_probes::series_balances(&url, nft, *series, owner)
                .expect("series balances at home")
                .0
        })
        .collect();

    test_issuance::batch_bridge_home(
        &target_url,
        DEPLOYER_KEY,
        bridge,
        home_chain,
        owner,
        &tokens,
    )
    .expect("send the remaining units home in one batch");

    let deadline = Instant::now() + Duration::from_secs(DELIVERY_TIMEOUT_SECS);
    for (series, before) in world.state.lifecycle_series.clone().into_iter().zip(before) {
        let want = before + u64::from(amount);
        loop {
            if venue_probes::series_balances(&url, nft, series, owner)
                .is_some_and(|(issued, _)| issued >= want)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "series {series} never arrived home in the batch hop"
            );
            sleep(Duration::from_secs(2));
        }
    }
}

/// Drive the owner's own bridge hop for every series and wait for the units to land.
fn bring_home(world: &mut World, amount: u32) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let target_url = world
        .target_chain
        .rpc_url()
        .expect("target chain is running");
    let nft = intex_nft(world);
    let bridge = world
        .state
        .target_contracts
        .as_ref()
        .expect("intex venue was deployed on the target chain")
        .nft_bridge;
    let owner = crate::world::origin_venue::deployer_address();
    let home_chain = u32::try_from(world.rpc.chain_id(port).expect("committee chain id"))
        .expect("fits a uint32");

    for series in world.state.lifecycle_series.clone() {
        let before = venue_probes::series_balances(&url, nft, series, owner)
            .expect("series balances at home")
            .0;
        let token_id = venue_probes::issued_token_id(&url, nft, series).expect("issued token id");

        test_issuance::bridge_home(
            &target_url,
            DEPLOYER_KEY,
            bridge,
            home_chain,
            token_id,
            owner,
            amount,
        )
        .expect("send the units home");

        let want = before + u64::from(amount);
        let deadline = Instant::now() + Duration::from_secs(DELIVERY_TIMEOUT_SECS);
        loop {
            if venue_probes::series_balances(&url, nft, series, owner)
                .is_some_and(|(issued, _)| issued >= want)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "series {series} units never arrived home; the relay carried nothing back"
            );
            sleep(Duration::from_secs(2));
        }
    }
}

/// The two series left to run out: one settled in part, one never touched.
fn expiring_series(world: &World) -> [alloy_primitives::FixedBytes<14>; 2] {
    [
        world
            .state
            .expiring_series
            .expect("a series was issued to be settled in part"),
        world
            .state
            .untouched_series
            .expect("a series was issued to be left untouched"),
    ]
}

/// Part settled and part not, so the sweep has to return the load of the unrealized
/// units alone rather than the tirage the series was issued with.
#[when("the owner settles part of one series they let run out")]
fn settle_part_of_expiring(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nft = intex_nft(world);
    let currency = world
        .state
        .settlement_currency
        .expect("settlement currency was registered");
    let owner = crate::world::origin_venue::deployer_address();
    let series = world
        .state
        .expiring_series
        .expect("a series was issued to be left running out");

    // Only the units that stayed on the committee can be settled: nothing brings this
    // series home, and the rest expire where they are.
    let issued = venue_probes::series_balances(&url, nft, series, owner)
        .expect("read what the owner holds of the expiring series")
        .0;
    assert!(
        issued > u64::from(EXPIRING_SETTLED_UNITS),
        "series {series} holds {issued} units here, too few to settle part and leave \
         the rest to run out"
    );
    let proof = settlement_note(
        world,
        owner,
        currency.asset,
        expected_settlement_cost(),
        EXPIRING_SETTLED_UNITS,
    );
    test_issuance::settle(
        &url,
        DEPLOYER_KEY,
        series,
        owner,
        EXPIRING_SETTLED_UNITS,
        &proof,
    )
    .expect("settle part of the expiring series");
}

/// Waiting past the notice is the only way to reach expiry: the deadline is derived
/// against the clock, and neither side writes anything when it passes.
#[when("the call notice runs out on both of them")]
fn notice_runs_out(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let nft = intex_nft(world);
    let series = world
        .state
        .expiring_series
        .expect("a series was issued to be left unsettled");

    // Taken before the deadline so the forfeit shows up as a delta, not a total.
    world.state.unallocated_before_expiry = Some(
        world
            .rpc
            .promis_limit_total_unallocated_on(port)
            .expect("read the unallocated PROMIS the forfeit will return into"),
    );

    let deadline = venue_probes::series_call_deadline(&url, nft, series)
        .expect("the expiring series carries a call deadline");
    // A notice measured in days means the DEV profile never took, and the wait below
    // would sit out the whole run for no reason anyone could see.
    let notice = deadline.saturating_sub(u64::from(
        venue_probes::series_called_at(&url, nft, series).expect("the series was Called"),
    ));
    assert!(
        notice <= 3600,
        "call notice is {notice}s: the DEV parameter profile is not active, so this \
         scenario would wait out the production window"
    );
    assert!(
        deadline > 0,
        "series {series} has no deadline, so it was never Called"
    );
    wait_for_chain_time(world, port, deadline + EXPIRY_MARGIN_SECS);

    // The notice above is waited out for real, but the sweep opens a bucket only once
    // its hour has closed - so the group is re-queued on a deadline already behind a
    // closed one rather than idling out the rest of the hour.
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    test_issuance::close_call_notice(
        &url,
        DEPLOYER_KEY,
        settlement_currency::USD_ISO,
        world
            .state
            .lifecycle_day
            .expect("the lifecycle series were issued into a day"),
        now.saturating_sub(EXPIRY_BUCKET_SECS),
    )
    .expect("close the expiry bucket the group sits in");
}

#[then("both series read Expired on both chains")]
fn unsettled_series_expired(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let target_url = target_rpc_url(world);
    let nft = intex_nft(world);
    let target_nft = target_intex_nft(world);
    // No message carries expiry across: each chain derives it from the same calledAt
    // and notice, so both have to agree on their own.
    let target_router = world
        .state
        .target_contracts
        .as_ref()
        .expect("intex venue was deployed on the target chain")
        .target_router;

    // Anvil does not mine while this step only reads. Advance its clock even when
    // the Called mark was already applied and there is nothing left to retry.
    let now = eth::latest_block_timestamp(&url).expect("committee head timestamp");
    world
        .target_chain
        .sync_clock_to(now)
        .expect("carry the elapsed call notice to the target chain");

    for series in expiring_series(world) {
        // A mark whose calledAt sits ahead of this chain's clock is parked, not
        // applied, and nothing in a localnet plays the operator who retries it.
        let parked = eth::read_call(
            &target_url,
            target_router,
            &venue_probes::IIssuedSeries::parkedMarkCall { seriesId: series },
        );
        if parked.is_some_and(|mark| mark != 0) {
            eth::send_call(
                &target_url,
                target_router,
                crate::world::forge::DEPLOYER_KEY,
                &venue_probes::IIssuedSeries::applyParkedMarkCall { seriesId: series },
                None,
            )
            .expect("apply the mark the target chain parked");
        }

        for (label, at, collection) in [
            ("committee", url.as_str(), nft),
            ("target chain", target_url.as_str(), target_nft),
        ] {
            let deadline = Instant::now() + Duration::from_secs(DELIVERY_TIMEOUT_SECS);
            loop {
                if venue_probes::series_state(at, collection, series) == Some(EXPIRED) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "series {series} never read Expired on the {label}: {:?}; the mark parked \
                     on the target router reads {:?}",
                    venue_probes::series_state(at, collection, series),
                    eth::read_call(
                        &target_url,
                        target_router,
                        &venue_probes::IIssuedSeries::parkedMarkCall { seriesId: series },
                    )
                );
                sleep(Duration::from_secs(2));
            }
        }
        let committee_deadline = venue_probes::series_call_deadline(&url, nft, series)
            .expect("committee expiry deadline");
        let target_deadline = venue_probes::series_call_deadline(&target_url, target_nft, series)
            .expect("target expiry deadline");
        let target_timestamp =
            eth::latest_block_timestamp(&target_url).expect("target expiry observation timestamp");
        assert_eq!(
            target_deadline, committee_deadline,
            "series {series} expiry deadline parity"
        );
        assert!(
            target_timestamp > target_deadline,
            "series {series} target clock did not pass expiry"
        );
        eprintln!(
            "INTEX_EXPIRY_CLOCK series={series} committee_timestamp={now} \
             target_timestamp={target_timestamp} deadline={target_deadline} pending_mark={parked:?}"
        );
    }
}

#[then("only their unrealized load returns to the unallocated pool")]
fn forfeited_load_returns(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let nft = intex_nft(world);
    let owner = crate::world::origin_venue::deployer_address();
    let before = world
        .state
        .unallocated_before_expiry
        .expect("the unallocated pool was read before the notice ran out");

    // One series was settled in part and one was never touched, so the credit owed is
    // the sum of what each still carries unrealized - never either tirage on its own.
    let mut want = alloy_primitives::U256::ZERO;
    let target_url = target_rpc_url(world);
    let target_nft = target_intex_nft(world);

    for (series, settled_units) in expiring_series(world)
        .into_iter()
        .zip([EXPIRING_SETTLED_UNITS, 0])
    {
        let load = venue_probes::series_promis_load(&url, nft, series)
            .expect("the expiring series carries a PROMIS load");
        // Measured against the series' whole tirage, not one chain's balance: the
        // units live on both chains, and only committee-side ones could be settled.
        let tirage = venue_probes::series_issued_count(&url, nft, series)
            .expect("the expiring series carries an issued count");
        let unrealized = tirage
            .checked_sub(settled_units)
            .expect("the expiring series was issued with more units than it settled");
        let (committee_issued, committee_settled) =
            venue_probes::series_balances(&url, nft, series, owner)
                .expect("read what the owner still holds of the expiring series");
        let (target_issued, target_settled) =
            venue_probes::series_balances(&target_url, target_nft, series, owner)
                .expect("read what the owner still holds on the target chain");
        assert_eq!(
            committee_settled + target_settled,
            u64::from(settled_units),
            "series {series} did not end with exactly the settled part this asserts"
        );
        assert_eq!(
            committee_issued + target_issued,
            u64::from(unrealized),
            "the owner carries {} unrealized units of {series}, not {unrealized}: the \
             rest was settled or parked, and the forfeit is not what this asserts",
            committee_issued + target_issued
        );
        assert!(
            unrealized > 0,
            "series {series} held nothing at the deadline, so the forfeit proves nothing"
        );
        want += alloy_primitives::U256::from(load) * alloy_primitives::U256::from(unrealized);
    }

    let deadline = Instant::now() + Duration::from_secs(FORFEIT_TIMEOUT_SECS);
    loop {
        let now = world
            .rpc
            .promis_limit_total_unallocated_on(port)
            .expect("read the unallocated PROMIS after the forfeit");
        if now == before + want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "unallocated PROMIS went from {before} to {now}, expected {} back",
            before + want
        );
        sleep(Duration::from_secs(2));
    }
}
