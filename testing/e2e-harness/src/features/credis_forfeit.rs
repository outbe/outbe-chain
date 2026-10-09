//! A called Credis left unpaid past its notice: it reads Forfeited at once, and the
//! forfeit sweep burns the rest of its pledge into the Promis Limit pool.

use std::time::Duration;

use alloy_primitives::{Address, U256};
use alloy_sol_types::{sol, SolEvent};
use cucumber::{then, when};
use outbe_primitives::addresses::{CREDIS_ADDRESS, CREDIS_FACTORY_ADDRESS};

use crate::features::entity_lifecycle::chain::{
    assert_single_event, finalized_checkpoint, head_time, poll_until,
};
use crate::features::entity_lifecycle::phases::CALL_WINDOW_SEED_DAYS;
use crate::internal::{addresses, eth};
use crate::world::credis::{
    event, execute, send, snapshot, CredisFixture, ICredis, ICredisFactory, IFixtureToken, DAY,
    INITIAL_GRATIS, LIQUIDITY, PRINCIPAL, USD,
};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::{test_issuance, World};

const CALLED: u8 = 1;
const FORFEITED: u8 = 3;
const BREACH_RATE: U256 = U256::from_limbs([2_000_000, 0, 0, 0]);
const PART: U256 = U256::from_limbs([100_000_000, 0, 0, 0]);
const CALL_TIMEOUT_SECS: u64 = 300;
const FORFEIT_TIMEOUT: Duration = Duration::from_secs(120);
const EXPIRY_BUCKET_SECS: u64 = 3_600;

sol! {
    interface ICredisTestArming {
        function backdateCredisForTest(uint256 credisId, uint64 issuedAt) external;
        function closeCallNoticeForTest(uint256 credisId, uint64 deadline) external;
    }
}

#[when("the USD rate holds above the Credis call price across its call window")]
fn rate_above_call_price(world: &mut World) {
    let url = url(world);
    let f = fixture(world);
    assert!(BREACH_RATE > latest(world).call.callPriceMinor);
    // The call counts only days the Credis was open: stamp it behind every seeded day.
    send(
        &url,
        CREDIS_ADDRESS,
        DEPLOYER_KEY,
        &ICredisTestArming::backdateCredisForTestCall {
            credisId: f.credis_id,
            issuedAt: head_time(world) - (u64::from(CALL_WINDOW_SEED_DAYS) + 1) * DAY,
        },
        None,
    );
    test_issuance::seed_day_vwaps(&url, DEPLOYER_KEY, USD, CALL_WINDOW_SEED_DAYS, BREACH_RATE)
        .expect("seed the breaching closed days");
}

#[then("the Credis is called with a settlement notice")]
fn called(world: &mut World) {
    poll_until(
        Duration::from_secs(CALL_TIMEOUT_SECS),
        || "the call sweep never called the breached Credis".into(),
        || latest(world).state == CALLED,
    );
    let height = finalized_checkpoint(world).height;
    let record = snapshot(world).record.expect("called Credis");
    let terms = &record.call;
    assert!(
        u64::from(terms.callNoticePeriod) <= EXPIRY_BUCKET_SECS,
        "call notice is {}s: the DEV parameter profile is not active",
        terms.callNoticePeriod
    );
    assert_eq!(
        terms.settlementDeadline,
        terms.calledAt + u64::from(terms.callNoticePeriod)
    );
    assert_single_event(
        &url(world),
        CREDIS_ADDRESS,
        0,
        height,
        ICredis::CredisCalled {
            credisId: record.credisId,
            calledAt: terms.calledAt,
            settlementDeadline: terms.settlementDeadline,
        },
    );
}

#[when("the user pays part of the called Credis before its deadline")]
fn pay_part(world: &mut World) {
    let url = url(world);
    let f = fixture(world);
    let before = snapshot(world);
    let record = before.record.as_ref().expect("called Credis");
    assert!(head_time(world) < record.call.settlementDeadline);
    let interest = eth::read_call(
        &url,
        CREDIS_ADDRESS,
        &ICredis::interestAccruedMinorCall {
            credisId: f.credis_id,
        },
    )
    .expect("accrued interest");
    let amount = PART + interest;
    execute(
        &url,
        f.account,
        DEPLOYER_KEY,
        f.currency.asset,
        &IFixtureToken::approveCall {
            spender: CREDIS_FACTORY_ADDRESS,
            amount,
        },
    );
    let receipt = execute(
        &url,
        f.account,
        DEPLOYER_KEY,
        CREDIS_FACTORY_ADDRESS,
        &ICredisFactory::settleCredisCall {
            credisId: f.credis_id,
            amountMinor: amount,
        },
    );
    let applied = event::<ICredis::SettlementApplied>(&receipt, CREDIS_ADDRESS);
    assert_eq!(
        (applied.interestPaidMinor, applied.principalPaidMinor),
        (interest, PART)
    );
    let after = snapshot(world);
    let paid = after.record.as_ref().expect("partly paid Credis");
    assert_eq!(paid.state, CALLED);
    assert_eq!(paid.outstandingPrincipalMinor, PRINCIPAL - PART);
    assert_eq!(
        paid.outstandingGratisMinor,
        record.outstandingGratisMinor - applied.gratisReturnedMinor
    );
    assert_eq!(
        paid.outcome,
        ICredis::Outcome {
            principalPaidMinor: PART,
            principalWrittenOffMinor: U256::ZERO,
            gratisReturnedMinor: applied.gratisReturnedMinor,
            gratisBurnedMinor: U256::ZERO,
        }
    );
    assert_eq!(after.pledged, before.pledged - applied.gratisReturnedMinor);
    assert_eq!(after.liquid, before.liquid + applied.gratisReturnedMinor);
    // Read before the deadline can lapse, so no forfeit has credited the pool yet.
    let height = finalized_checkpoint(world).height;
    let pool = eth::read_call_at(
        &url,
        addresses::PROMIS_LIMIT_ADDR,
        &eth::IPromisLimit::totalUnallocatedCall {},
        height,
    )
    .expect("unallocated pool before the forfeit");
    let f = world.state.credis.as_mut().expect("fixture");
    f.interest_paid += interest;
    f.pool_before_forfeit = Some((height, pool));
}

#[when("the settlement deadline passes unpaid")]
fn deadline_passes(world: &mut World) {
    let deadline = latest(world).call.settlementDeadline;
    poll_until(
        Duration::from_secs(EXPIRY_BUCKET_SECS + CALL_TIMEOUT_SECS),
        || format!("the chain never passed the settlement deadline {deadline}"),
        || head_time(world) > deadline,
    );
}

#[then("the Credis reads Forfeited with its remainders written off")]
fn reads_forfeited(world: &mut World) {
    let f = fixture(world);
    let lapsed = latest(world);
    assert_eq!(lapsed.state, FORFEITED);
    assert_eq!(
        (
            lapsed.outstandingPrincipalMinor,
            lapsed.outstandingGratisMinor
        ),
        (U256::ZERO, U256::ZERO)
    );
    let outcome = &lapsed.outcome;
    assert_eq!(outcome.principalPaidMinor, PART);
    assert_eq!(outcome.principalWrittenOffMinor, PRINCIPAL - PART);
    assert_eq!(
        outcome.gratisReturnedMinor + outcome.gratisBurnedMinor,
        f.gratis_minor
    );
    assert!(outcome.gratisBurnedMinor > U256::ZERO);
    assert_eq!(
        eth::read_call(
            &url(world),
            CREDIS_ADDRESS,
            &ICredis::interestAccruedMinorCall {
                credisId: f.credis_id
            }
        ),
        Some(U256::ZERO)
    );
    world.state.credis.as_mut().expect("fixture").lapsed = Some(lapsed);
}

/// The sweep opens an hour only once it has closed: move the deadline into a closed one.
#[when("the forfeit sweep reaches the lapsed Credis")]
fn sweep_reaches(world: &mut World) {
    let outcome = eth::send_call_outcome(
        &url(world),
        CREDIS_ADDRESS,
        DEPLOYER_KEY,
        &ICredisTestArming::closeCallNoticeForTestCall {
            credisId: fixture(world).credis_id,
            deadline: head_time(world) - EXPIRY_BUCKET_SECS,
        },
        None,
    )
    .expect("submit closeCallNoticeForTest");
    if !outcome.success {
        eprintln!("the deadline's hour closed first: the sweep already dequeued the Credis");
    }
}

#[then("the remaining pledge is burned into the Promis Limit pool once")]
fn burned(world: &mut World) {
    let url = url(world);
    let f = fixture(world);
    let (from, pool) = f.pool_before_forfeit.expect("pool before the forfeit");
    let lapsed = f.lapsed.clone().expect("lapsed Credis");
    let burned = lapsed.outcome.gratisBurnedMinor;
    let unallocated = |height: Option<u64>| {
        let call = eth::IPromisLimit::totalUnallocatedCall {};
        match height {
            Some(height) => eth::read_call_at(&url, addresses::PROMIS_LIMIT_ADDR, &call, height),
            None => eth::read_call(&url, addresses::PROMIS_LIMIT_ADDR, &call),
        }
        .expect("unallocated pool")
    };
    poll_until(
        FORFEIT_TIMEOUT,
        || "the forfeit sweep never burned the lapsed Credis's pledge".into(),
        || unallocated(None) == pool + burned,
    );
    let height = finalized_checkpoint(world).height;
    assert_eq!(unallocated(Some(height)), pool + burned);
    assert_single_event(
        &url,
        CREDIS_ADDRESS,
        from,
        height,
        ICredis::CredisForfeited {
            credisId: f.credis_id,
            cca: f.cca,
            gratisBurnedMinor: burned,
            principalWrittenOffMinor: lapsed.outcome.principalWrittenOffMinor,
        },
    );
    assert_no_event::<ICredisFactory::ExpiryDeferred>(&url, CREDIS_FACTORY_ADDRESS, from, height);
    let state = snapshot(world);
    assert_eq!(
        state.record.as_ref(),
        Some(&lapsed),
        "the sweep moved the projection"
    );
    assert_eq!(state.pledged, U256::ZERO);
    assert_eq!(
        state.liquid,
        INITIAL_GRATIS - f.gratis_minor + lapsed.outcome.gratisReturnedMinor
    );
    assert_eq!(
        state.vault_stables,
        LIQUIDITY - PRINCIPAL + PART + f.interest_paid
    );
    assert_eq!(state.cca_stables, PRINCIPAL);
    assert_settlement_rejected(world, f);
}

fn assert_settlement_rejected(world: &World, f: &CredisFixture) {
    let url = url(world);
    execute(
        &url,
        f.account,
        DEPLOYER_KEY,
        f.currency.asset,
        &IFixtureToken::approveCall {
            spender: CREDIS_FACTORY_ADDRESS,
            amount: PART,
        },
    );
    let settle = ICredisFactory::settleCredisCall {
        credisId: f.credis_id,
        amountMinor: PART,
    };
    let outcome = eth::send_call_outcome(
        &url,
        f.account,
        DEPLOYER_KEY,
        &crate::world::credis::IMockAccount::executeCall {
            target: CREDIS_FACTORY_ADDRESS,
            value: U256::ZERO,
            data: alloy_sol_types::SolCall::abi_encode(&settle).into(),
        },
        None,
    )
    .expect("submit a settlement of the forfeited Credis");
    assert!(!outcome.success, "a forfeited Credis accepted a settlement");
}

fn assert_no_event<E: SolEvent>(url: &str, address: Address, from: u64, to: u64) {
    let logs = eth::raw_json_result(
        url,
        "eth_getLogs",
        serde_json::json!([{
            "address": address, "fromBlock": format!("0x{from:x}"), "toBlock": format!("0x{to:x}"),
            "topics": [E::SIGNATURE_HASH],
        }]),
    )
    .expect("finalized forfeit events");
    assert_eq!(logs, serde_json::json!([]), "unexpected {}", E::SIGNATURE);
}

fn latest(world: &World) -> ICredis::Credis {
    eth::read_call(
        &url(world),
        CREDIS_ADDRESS,
        &ICredis::getCredisCall {
            credisId: fixture(world).credis_id,
        },
    )
    .expect("latest Credis")
}

fn fixture(world: &World) -> &CredisFixture {
    world.state.credis.as_ref().expect("Credis fixture")
}

fn url(world: &World) -> String {
    world.rpc.url(world.validators.primary_port())
}
