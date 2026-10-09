//! A called Credis left unpaid past its notice: it reads Forfeited at once, and the
//! forfeit sweep burns the rest of its pledge into the Promis Limit pool.

use std::time::Duration;

use alloy_primitives::{Address, B256, U256};
use alloy_sol_types::{sol, SolCall, SolEvent};
use cucumber::{then, when};
use outbe_primitives::addresses::{CREDIS_ADDRESS, CREDIS_FACTORY_ADDRESS};

use crate::features::entity_lifecycle::chain::{
    assert_single_event, finalized_checkpoint, head_time, poll_until,
};
use crate::features::entity_lifecycle::phases::CALL_WINDOW_SEED_DAYS;
use crate::features::settlement::assert_receipt_event;
use crate::internal::{addresses, eth};
use crate::world::credis::{
    execute, receipt_height, send, snapshot, CredisFixture, ICredis, ICredisFactory, IFixtureToken,
    IMockAccount, DAY, INITIAL_GRATIS, LIQUIDITY, PRINCIPAL, USD,
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
/// Window, threshold and notice of the DEV profile in an e2e build, in seconds.
const DEV_TERMS: (u32, u32, u32) = (3 * 86_400, 2 * 86_400, 600);

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
    assert_eq!(
        (
            terms.callWindow,
            terms.callThreshold,
            terms.callNoticePeriod
        ),
        DEV_TERMS,
        "the Credis did not seal the DEV parameter profile"
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
    let returned = (record.gratisMinor * PART)
        .div_ceil(record.principalMinor)
        .min(record.outstandingGratisMinor);
    assert_receipt_event(
        &receipt,
        CREDIS_ADDRESS,
        &ICredis::SettlementApplied {
            credisId: f.credis_id,
            interestPaidMinor: interest,
            principalPaidMinor: PART,
            gratisReturnedMinor: returned,
            outstandingPrincipalMinor: PRINCIPAL - PART,
        },
    );
    let after = snapshot(world);
    let paid = after.record.as_ref().expect("partly paid Credis");
    assert_eq!(paid.state, CALLED);
    assert_eq!(paid.outstandingPrincipalMinor, PRINCIPAL - PART);
    assert_eq!(
        paid.outstandingGratisMinor,
        record.outstandingGratisMinor - returned
    );
    assert_eq!(
        paid.outcome,
        ICredis::Outcome {
            principalPaidMinor: PART,
            principalWrittenOffMinor: U256::ZERO,
            gratisReturnedMinor: returned,
            gratisBurnedMinor: U256::ZERO,
        }
    );
    assert_eq!(after.pledged, before.pledged - returned);
    assert_eq!(after.liquid, before.liquid + returned);
    let f = world.state.credis.as_mut().expect("fixture");
    f.interest_paid += interest;
    f.partly_paid_at = Some(receipt_height(&receipt));
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
    // The sweep may already have reached it, if the deadline's hour closed at once.
    let (reason, height) = settlement_revert(world, f);
    let swept = forfeit_block(&url(world), f.credis_id).is_some_and(|block| block <= height);
    let expected = if swept {
        "Credis is closed"
    } else {
        "settlement deadline has passed"
    };
    assert_eq!(reason, expected);
    world.state.credis.as_mut().expect("fixture").lapsed = Some(lapsed);
}

/// The sweep opens an hour only once it has closed: move the deadline into a closed one.
#[when("the forfeit sweep reaches the lapsed Credis")]
fn sweep_reaches(world: &mut World) {
    let url = url(world);
    let credis_id = fixture(world).credis_id;
    // The deadline's hour may have closed already, and the sweep forfeited it.
    if forfeit_block(&url, credis_id).is_some() {
        return;
    }
    send(
        &url,
        CREDIS_ADDRESS,
        DEPLOYER_KEY,
        &ICredisTestArming::closeCallNoticeForTestCall {
            credisId: credis_id,
            deadline: head_time(world) - EXPIRY_BUCKET_SECS,
        },
        None,
    );
}

#[then("the remaining pledge is burned into the Promis Limit pool once")]
fn burned(world: &mut World) {
    let url = url(world);
    let f = fixture(world);
    let from = f.partly_paid_at.expect("partial payment height");
    let lapsed = f.lapsed.clone().expect("lapsed Credis");
    let burned = lapsed.outcome.gratisBurnedMinor;
    poll_until(
        FORFEIT_TIMEOUT,
        || "the forfeit sweep never burned the lapsed Credis's pledge".into(),
        || forfeit_block(&url, f.credis_id).is_some(),
    );
    let height = finalized_checkpoint(world).height;
    let forfeited_at = forfeit_block(&url, f.credis_id).expect("forfeit block");
    assert!(forfeited_at <= height);
    let unallocated = |height| {
        eth::read_call_at(
            &url,
            addresses::PROMIS_LIMIT_ADDR,
            &eth::IPromisLimit::totalUnallocatedCall {},
            height,
        )
        .expect("unallocated pool")
    };
    // Other modules credit the pool too: only the forfeit's own block is attributable.
    assert_eq!(
        unallocated(forfeited_at) - unallocated(forfeited_at - 1),
        burned,
        "the forfeit did not credit exactly the burned Gratis to the pool"
    );
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
    assert_eq!(settlement_revert(world, f).0, "Credis is closed");
}

/// Why a settlement through the Credis's smart account reverts, and the block read.
fn settlement_revert(world: &World, f: &CredisFixture) -> (String, u64) {
    let settle = ICredisFactory::settleCredisCall {
        credisId: f.credis_id,
        amountMinor: PART,
    };
    let height = world
        .rpc
        .head(world.validators.primary_port())
        .expect("primary head");
    let reason = eth::read_call_revert_reason_at(
        &url(world),
        f.account,
        f.user,
        &IMockAccount::executeCall {
            target: CREDIS_FACTORY_ADDRESS,
            value: U256::ZERO,
            data: settle.abi_encode().into(),
        },
        height,
    )
    .expect("a settlement of a lapsed Credis reverts");
    (reason, height)
}

/// The block of the Credis's `CredisForfeited` log, once there is one.
fn forfeit_block(url: &str, credis_id: U256) -> Option<u64> {
    let logs = eth::raw_json_result(
        url,
        "eth_getLogs",
        serde_json::json!([{
            "address": CREDIS_ADDRESS, "fromBlock": "0x0", "toBlock": "latest",
            "topics": [ICredis::CredisForfeited::SIGNATURE_HASH, B256::from(credis_id)],
        }]),
    )
    .expect("Credis forfeit logs");
    let block = logs.as_array()?.first()?["blockNumber"].as_str()?;
    u64::from_str_radix(block.trim_start_matches("0x"), 16).ok()
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
