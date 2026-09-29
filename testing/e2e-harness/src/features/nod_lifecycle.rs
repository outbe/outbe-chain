//! Two Nods in one bucket: both called, one paid inside the notice, the other
//! forfeited once the notice runs out.

use std::thread::sleep;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::SolCall;
use cucumber::{then, when};
use outbe_nod::constants::{
    CALL_LOOKBACK_DAYS, CALL_NOTICE_PERIOD, CALL_RATE_PCT, CALL_THRESHOLD, CALL_THRESHOLD_DAYS,
    CALL_WINDOW, FLOOR_RATE_PCT,
};

use crate::features::settlement::{assert_mined_success, assert_receipt_event, fund_and_approve};
use crate::internal::{addresses, eth};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::settlement_currency::{SettlementCurrency, USD_ISO};
use crate::world::{test_issuance, World};

alloy_sol_types::sol! {
    interface INodFactoryTestArming {
        function issueForTest(
            address owner,
            uint32 worldwideDay,
            uint256 gratisLoadMinor,
            uint256 entryPriceMinor,
            uint16 issuanceCurrency,
            uint16 referenceCurrency,
            uint64 issuedAt
        ) external;
        function closeCallNoticeForTest(uint256 nodId, uint64 deadline) external;
    }
}

/// A Nod's id derives from its owner and day, so one bucket needs two owners.
const PAID_OWNER: Address = Address::repeat_byte(0xa1);
const FORFEITED_OWNER: Address = Address::repeat_byte(0xb2);
const ENTRY_PRICE_MINOR: u64 = 1_000_000;
const GRATIS_LOAD_MINOR: u64 = 5_000_000;
/// `effectiveState` of `INod.NodData`.
const ISSUED: u8 = 0;
const QUALIFIED: u8 = 1;
const CALLED: u8 = 2;
const SETTLED: u8 = 3;
/// Issuance goes through the compressed-body projection before the index shows it.
const ISSUANCE_TIMEOUT_SECS: u64 = 120;
/// Qualification is read off the seeded day, so it waits only for that block.
const QUALIFY_TIMEOUT_SECS: u64 = 120;
/// The call sweep runs on the shortened e2e cadence, not instantly.
const CALL_TIMEOUT_SECS: u64 = 300;
/// How long a Nod view may trail the block that wrote it.
const READ_TIMEOUT_SECS: u64 = 60;
/// Once the notice has lapsed the next sweep burns the unpaid Nod.
const FORFEIT_TIMEOUT_SECS: u64 = 300;

#[when("two owners are issued Nods in one bucket")]
fn issue_two_nods(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    let day: u32 = crate::world::localnet::worldwide_day()
        .parse()
        .expect("worldwide day key");
    // The bucket counts breach days only after it was issued: stamp it behind the
    // whole call window the scenario is about to seed.
    let issued_at = now.saturating_sub((u64::from(CALL_LOOKBACK_DAYS) + 2) * 86_400);
    for owner in [PAID_OWNER, FORFEITED_OWNER] {
        let issue = INodFactoryTestArming::issueForTestCall {
            owner,
            worldwideDay: day,
            gratisLoadMinor: U256::from(GRATIS_LOAD_MINOR),
            entryPriceMinor: U256::from(ENTRY_PRICE_MINOR),
            issuanceCurrency: USD_ISO,
            referenceCurrency: USD_ISO,
            issuedAt: issued_at,
        };
        let outcome = eth::send_call_outcome(
            &url,
            addresses::NOD_FACTORY_ADDR,
            DEPLOYER_KEY,
            &issue,
            None,
        )
        .expect("submit test Nod issuance");
        assert_mined_success(&outcome, "test Nod issuance");
    }
    world.state.paid_nod = Some(wait_for_nod_of(&url, PAID_OWNER));
    world.state.forfeited_nod = Some(wait_for_nod_of(&url, FORFEITED_OWNER));
}

#[then("both Nods read Issued and carry the bucket's call terms")]
fn both_issued(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let entry = U256::from(ENTRY_PRICE_MINOR);
    let paid = read_nod(&url, paid_nod(world));
    let forfeited = read_nod(&url, forfeited_nod(world));
    for (nod, owner) in [(&paid, PAID_OWNER), (&forfeited, FORFEITED_OWNER)] {
        assert_eq!(nod.owner, owner);
        assert_eq!(nod.effectiveState, ISSUED);
        assert!(!nod.isQualified);
        assert!(!nod.isSettled);
        assert_eq!(nod.calledAt, 0);
        assert_eq!(nod.settlementDeadline, 0);
        assert_eq!(nod.referenceCurrency, USD_ISO);
        assert_eq!(nod.entryPriceMinor, entry);
        assert_eq!(nod.gratisLoadMinor, U256::from(GRATIS_LOAD_MINOR));
        assert_eq!(
            nod.floorPriceMinor,
            entry * U256::from(100 + FLOOR_RATE_PCT) / U256::from(100)
        );
        assert_eq!(
            nod.callPriceMinor,
            entry * U256::from(100 + CALL_RATE_PCT) / U256::from(100)
        );
        assert_eq!(nod.callRate, CALL_RATE_PCT);
        assert_eq!(nod.callWindow, CALL_WINDOW);
        assert_eq!(nod.callThreshold, CALL_THRESHOLD);
        assert_eq!(nod.callNoticePeriod, CALL_NOTICE_PERIOD);
    }
    assert_eq!(
        (paid.worldwideDay, paid.floorPriceMinor),
        (forfeited.worldwideDay, forfeited.floorPriceMinor),
        "both Nods must share one bucket"
    );
}

#[when("the reference rate stands above the Nod floor")]
fn rate_above_floor(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nod = read_nod(&url, paid_nod(world));
    let rate = nod.floorPriceMinor * U256::from(2);
    assert!(
        rate < nod.callPriceMinor,
        "the qualifying rate must stay below the call price"
    );
    test_issuance::seed_day_vwaps(&url, DEPLOYER_KEY, USD_ISO, 1, rate)
        .expect("seed the closed day above the Nod floor");
}

#[then("both Nods qualify")]
fn both_qualify(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    for id in [paid_nod(world), forfeited_nod(world)] {
        let nod = wait_for_state(&url, id, QUALIFIED, QUALIFY_TIMEOUT_SECS);
        assert!(nod.isQualified);
    }
}

#[when("the reference rate holds above the call price across the call window")]
fn rate_above_call_price(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let nod = read_nod(&url, paid_nod(world));
    test_issuance::seed_day_vwaps(
        &url,
        DEPLOYER_KEY,
        USD_ISO,
        CALL_THRESHOLD_DAYS,
        nod.callPriceMinor * U256::from(2),
    )
    .expect("seed the call-window VWAPs");
}

#[then("the bucket is Called with its notice running")]
fn bucket_called(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let paid = wait_for_state(&url, paid_nod(world), CALLED, CALL_TIMEOUT_SECS);
    let forfeited = wait_for_state(&url, forfeited_nod(world), CALLED, CALL_TIMEOUT_SECS);
    assert_ne!(paid.calledAt, 0);
    assert_eq!(
        forfeited.calledAt, paid.calledAt,
        "a call is made on the whole bucket"
    );
    assert_eq!(
        paid.settlementDeadline,
        paid.calledAt + u64::from(CALL_NOTICE_PERIOD)
    );
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    assert!(
        now <= paid.settlementDeadline,
        "the notice is still running"
    );
}

#[when("the first Nod is paid inside the notice")]
fn pay_first_nod(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let SettlementCurrency { asset, .. } = world
        .state
        .settlement_currency
        .expect("settlement currency was registered");
    let id = paid_nod(world);
    let nod = read_nod(&url, id);
    let quote = eth::read_call(
        &url,
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::quoteSettlementCall { nodId: id, asset },
    )
    .expect("settlement quote");
    assert_eq!(quote.settlementCurrency, USD_ISO);
    assert_eq!(quote.payableUnits, nod.settlementCostMinor);
    let payer = eth::address_of(DEPLOYER_KEY).expect("payer address");
    fund_and_approve(
        world,
        asset,
        DEPLOYER_KEY,
        payer,
        addresses::NOD_FACTORY_ADDR,
        quote.payableUnits,
    );
    let settle = eth::INodFactory::settleNodCall {
        nodId: id,
        asset,
        snapshotId: quote.snapshotId,
    };
    let outcome = eth::send_call_outcome(
        &url,
        addresses::NOD_FACTORY_ADDR,
        DEPLOYER_KEY,
        &settle,
        None,
    )
    .expect("submit Nod settlement");
    assert_mined_success(&outcome, "Nod settlement inside the notice");
    assert_receipt_event(
        &outcome.receipt,
        addresses::NOD_FACTORY_ADDR,
        &eth::INodFactory::NodPaid {
            owner: PAID_OWNER,
            nodId: id,
            asset,
            nullifier: B256::ZERO,
            amountCovered: nod.settlementCostMinor,
        },
    );
}

#[then("that Nod is Settled while the other stays Called")]
fn first_settled(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let paid = read_nod(&url, paid_nod(world));
    assert!(paid.isSettled);
    assert_eq!(paid.effectiveState, SETTLED);
    assert_eq!(read_nod(&url, forfeited_nod(world)).effectiveState, CALLED);
}

#[when("the call notice lapses")]
fn notice_lapses(world: &mut World) {
    let port = world.validators.primary_port();
    let url = world.rpc.url(port);
    let now = world
        .rpc
        .latest_block_timestamp(port)
        .expect("committee head timestamp");
    let close = INodFactoryTestArming::closeCallNoticeForTestCall {
        nodId: forfeited_nod(world),
        deadline: now - 1,
    };
    let outcome = eth::send_call_outcome(
        &url,
        addresses::NOD_FACTORY_ADDR,
        DEPLOYER_KEY,
        &close,
        None,
    )
    .expect("submit call notice close");
    assert_mined_success(&outcome, "close the Nod call notice");
}

#[then("the unpaid Nod is forfeited and burned while the paid one stays Settled")]
fn unpaid_forfeited(world: &mut World) {
    let url = world.rpc.url(world.validators.primary_port());
    let forfeited = forfeited_nod(world);
    let deadline = Instant::now() + Duration::from_secs(FORFEIT_TIMEOUT_SECS);
    while nod_balance(&url, FORFEITED_OWNER) != U256::ZERO {
        assert!(
            Instant::now() < deadline,
            "the unpaid Nod was not burned after its notice lapsed"
        );
        sleep(Duration::from_secs(1));
    }
    assert!(
        eth::read_call(
            &url,
            addresses::NOD_ADDR,
            &eth::INod::nodDataCall { nodId: forfeited }
        )
        .is_none(),
        "a burned Nod must not be readable"
    );
    let paid = read_nod(&url, paid_nod(world));
    assert_eq!(paid.effectiveState, SETTLED);
    assert_eq!(nod_balance(&url, PAID_OWNER), U256::ONE);
}

fn paid_nod(world: &World) -> U256 {
    world.state.paid_nod.expect("paid Nod was issued")
}

fn forfeited_nod(world: &World) -> U256 {
    world.state.forfeited_nod.expect("forfeited Nod was issued")
}

fn nod_balance(url: &str, owner: Address) -> U256 {
    read_nod_view(url, &eth::INod::balanceOfCall { owner }, "Nod balance")
}

fn read_nod(url: &str, id: U256) -> eth::INod::NodData {
    read_nod_view(url, &eth::INod::nodDataCall { nodId: id }, "Nod data")
}

/// Nod views read through the compressed-body projection, which trails the block
/// that wrote the body, so a fresh write can answer an error for a moment.
fn read_nod_view<C>(url: &str, call: &C, what: &str) -> C::Return
where
    C: SolCall,
    C::Return: Send + 'static,
{
    let deadline = Instant::now() + Duration::from_secs(READ_TIMEOUT_SECS);
    loop {
        match eth::read_call_result(url, addresses::NOD_ADDR, call) {
            Ok(value) => return value,
            Err(error) => {
                assert!(Instant::now() < deadline, "{what}: {error}");
                sleep(Duration::from_secs(1));
            }
        }
    }
}

fn wait_for_nod_of(url: &str, owner: Address) -> U256 {
    let deadline = Instant::now() + Duration::from_secs(ISSUANCE_TIMEOUT_SECS);
    loop {
        if nod_balance(url, owner) == U256::ONE {
            return read_nod_view(
                url,
                &eth::INod::tokenOfOwnerByIndexCall {
                    owner,
                    index: U256::ZERO,
                },
                "owner's Nod id",
            );
        }
        assert!(
            Instant::now() < deadline,
            "the issued Nod never reached its owner's index"
        );
        sleep(Duration::from_secs(1));
    }
}

fn wait_for_state(url: &str, id: U256, state: u8, timeout_secs: u64) -> eth::INod::NodData {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let nod = read_nod(url, id);
        if nod.effectiveState == state {
            return nod;
        }
        assert!(
            Instant::now() < deadline,
            "Nod {id} stayed in state {} instead of reaching {state}",
            nod.effectiveState
        );
        sleep(Duration::from_secs(1));
    }
}
