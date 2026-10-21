//! A called Credis left unpaid past its notice: it reads Forfeited at once, its group is
//! forfeited, and the cleanup burns the rest of its pledge into the Promis Limit pool
//! and deletes the Credis. An owner may forfeit their Credis themselves, with
//! the same burn.

use std::time::Duration;

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{sol, SolError, SolEvent};
use cucumber::{then, when};
use outbe_primitives::addresses::{CREDIS_ADDRESS, CREDIS_FACTORY_ADDRESS};
use outbe_primitives::time::SECONDS_PER_DAY_U32;

use crate::features::entity_lifecycle::chain::{
    assert_single_event, finalized_checkpoint, head_time, poll_until,
};
use crate::features::entity_lifecycle::phases::CALL_WINDOW_SEED_DAYS;
use crate::features::settlement::assert_receipt_event;
use crate::internal::{addresses, eth};
use crate::world::credis::{
    receipt_height, send, snapshot, CredisFixture, ICredis, ICredisFactory, IFixtureToken, DAY,
    INITIAL_GRATIS, LIQUIDITY, PRINCIPAL, USD,
};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::{test_issuance, World};

const ISSUED: u8 = 0;
const CALLED: u8 = 1;
const FORFEITED: u8 = 3;
const BREACH_RATE: U256 = U256::from_limbs([2_000_000, 0, 0, 0]);
const PART: U256 = U256::from_limbs([100_000_000, 0, 0, 0]);
const CALL_TIMEOUT_SECS: u64 = 300;
const FORFEIT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_DEV_NOTICE_SECS: u64 = 3_600;
const EXPIRY_DAY_SECS: u64 = 86_400;
/// Window, threshold and notice of the DEV profile in an e2e build, in seconds.
const DEV_TERMS: (u32, u32, u32) = (3 * SECONDS_PER_DAY_U32, 2 * SECONDS_PER_DAY_U32, 600);

sol! {
    interface ICredisFactoryTestArming {
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
        CREDIS_FACTORY_ADDRESS,
        DEPLOYER_KEY,
        &ICredisFactoryTestArming::backdateCredisForTestCall {
            credisId: f.credis_id,
            issuedAt: head_time(world) - (u64::from(CALL_WINDOW_SEED_DAYS) + 1) * DAY,
        },
        None,
    );
    test_issuance::seed_day_vwaps(&url, DEPLOYER_KEY, USD, CALL_WINDOW_SEED_DAYS, BREACH_RATE)
        .expect("seed the breaching closed days");
    test_issuance::rescan_closed_day(&url, DEPLOYER_KEY, CREDIS_FACTORY_ADDRESS)
        .expect("arm the Credis call pass after seeding prices");
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
        ICredis::CredisGroupCalled {
            groupKey: record.groupKey,
            calledAt: terms.calledAt,
            settlementDeadline: terms.settlementDeadline,
        },
    );
}

#[when("the owner pays part of the called Credis before its deadline")]
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
    send(
        &url,
        f.currency.asset,
        &f.owner_key,
        &IFixtureToken::approveCall {
            spender: CREDIS_FACTORY_ADDRESS,
            amount,
        },
        None,
    );
    let receipt = send(
        &url,
        CREDIS_FACTORY_ADDRESS,
        &f.owner_key,
        &ICredisFactory::settleCredisCall {
            credisId: f.credis_id,
            amountMinor: amount,
        },
        None,
    );
    let returned = (record.gratisMinor * PART)
        .div_ceil(record.principalMinor)
        .min(record.outstandingGratisMinor);
    assert_receipt_event(
        &receipt,
        CREDIS_FACTORY_ADDRESS,
        &ICredisFactory::SettlementApplied {
            credisId: f.credis_id,
            interestPaidMinor: interest,
            principalPaidMinor: PART,
            gratisReturnedMinor: returned,
            outstandingPrincipalMinor: PRINCIPAL - PART,
        },
    );
    let after = snapshot(world);
    let paid = after.record.clone().expect("partly paid Credis");
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
    // What the forfeit writes off, if the Credis is deleted before it can be read lapsed.
    f.lapsed = Some(paid);
}

#[when("the settlement deadline passes unpaid")]
fn deadline_passes(world: &mut World) {
    let deadline = latest(world).call.settlementDeadline;
    poll_until(
        Duration::from_secs(MAX_DEV_NOTICE_SECS + CALL_TIMEOUT_SECS),
        || format!("the chain never passed the settlement deadline {deadline}"),
        || head_time(world) > deadline,
    );
    // Cleanup follows the closed deadline day's pass: read it before that.
    let height = world
        .rpc
        .head(world.validators.primary_port())
        .expect("primary head");
    // It may already be deleted if the deadline day closed at this boundary.
    if let Some(lapsed) = eth::read_call_at(
        &url(world),
        CREDIS_ADDRESS,
        &ICredis::getCredisCall {
            credisId: fixture(world).credis_id,
        },
        height,
    ) {
        world.state.credis.as_mut().expect("fixture").lapsed = Some(lapsed);
    }
}

/// The Gratis and the principal a forfeit writes off, from a Called or a lapsed read.
fn written_off(credis: &ICredis::Credis) -> (U256, U256) {
    (
        credis.outcome.gratisBurnedMinor + credis.outstandingGratisMinor,
        credis.outcome.principalWrittenOffMinor + credis.outstandingPrincipalMinor,
    )
}

#[then("the Credis reads Forfeited with its remainders written off")]
fn reads_forfeited(world: &mut World) {
    let f = fixture(world);
    let lapsed = f.lapsed.clone().expect("lapsed Credis");
    if lapsed.state != FORFEITED {
        assert_eq!(
            settlement_revert(world, f).0.as_ref(),
            ICredis::CredisNotFound {}.abi_encode()
        );
        return;
    }
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
    // Cleanup may already have deleted it if its deadline day closed at once.
    let (reason, height) = settlement_revert(world, f);
    let swept = forfeit_block(&url(world), f.credis_id).is_some_and(|block| block <= height);
    let expected = if swept {
        ICredis::CredisNotFound {}.abi_encode()
    } else {
        ICredisFactory::SettlementDeadlinePassed {}.abi_encode()
    };
    assert_eq!(reason.as_ref(), expected.as_slice());
}

/// Arm a closed deadline day after the exact settlement notice has elapsed.
#[when("the forfeit sweep reaches the lapsed Credis")]
fn sweep_reaches(world: &mut World) {
    let url = url(world);
    let credis_id = fixture(world).credis_id;
    // Its deadline day may have closed already, and cleanup deleted it.
    if forfeit_block(&url, credis_id).is_some() {
        return;
    }
    send(
        &url,
        CREDIS_FACTORY_ADDRESS,
        DEPLOYER_KEY,
        &ICredisFactoryTestArming::closeCallNoticeForTestCall {
            credisId: credis_id,
            deadline: head_time(world).saturating_sub(EXPIRY_DAY_SECS),
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
    let (burned, principal_written_off) = written_off(&lapsed);
    poll_until(
        FORFEIT_TIMEOUT,
        || "the cleanup never burned the lapsed Credis's pledge".into(),
        || forfeit_block(&url, f.credis_id).is_some(),
    );
    let height = finalized_checkpoint(world).height;
    let forfeited_at = forfeit_block(&url, f.credis_id).expect("forfeit block");
    assert!(forfeited_at <= height);
    assert_pool_credited(&url, forfeited_at, burned);
    assert_single_event(
        &url,
        CREDIS_ADDRESS,
        from,
        height,
        ICredis::CredisForfeited {
            credisId: f.credis_id,
            cca: f.cca,
            gratisBurnedMinor: burned,
            principalWrittenOffMinor: principal_written_off,
        },
    );
    assert_single_event(
        &url,
        CREDIS_ADDRESS,
        from,
        height,
        ICredis::CredisGroupForfeited {
            groupKey: lapsed.groupKey,
            credis: 1,
        },
    );
    assert_no_event::<ICredisFactory::ExpiryDeferred>(&url, CREDIS_FACTORY_ADDRESS, from, height);
    let state = snapshot(world);
    assert!(state.record.is_none(), "the cleanup deletes the Credis");
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
    assert_eq!(
        settlement_revert(world, f).0.as_ref(),
        ICredis::CredisNotFound {}.abi_encode()
    );
}

#[when("the owner forfeits the Credis")]
fn owner_forfeits(world: &mut World) {
    let url = url(world);
    let f = fixture(world);
    assert_eq!(latest(world).state, ISSUED);
    let receipt = send(
        &url,
        CREDIS_FACTORY_ADDRESS,
        &f.owner_key,
        &ICredisFactory::forfeitCredisCall {
            credisId: f.credis_id,
        },
        None,
    );
    assert_receipt_event(
        &receipt,
        CREDIS_ADDRESS,
        &ICredis::CredisForfeited {
            credisId: f.credis_id,
            cca: f.cca,
            gratisBurnedMinor: f.gratis_minor,
            principalWrittenOffMinor: PRINCIPAL,
        },
    );
}

#[then("the whole pledge is burned into the Promis Limit pool and the principal written off")]
fn owner_forfeit_burned(world: &mut World) {
    let url = url(world);
    let f = fixture(world);
    let forfeited_at = forfeit_block(&url, f.credis_id).expect("forfeit block");
    assert_pool_credited(&url, forfeited_at, f.gratis_minor);
    let state = snapshot(world);
    assert!(
        state.record.is_none(),
        "the owner's forfeit deletes the Credis"
    );
    assert_eq!(state.pledged, U256::ZERO);
    assert_eq!(state.liquid, INITIAL_GRATIS - f.gratis_minor);
    assert_eq!(state.vault_stables, LIQUIDITY - PRINCIPAL);
    assert_eq!(state.cca_stables, PRINCIPAL);
    assert_eq!(
        settlement_revert(world, f).0.as_ref(),
        ICredis::CredisNotFound {}.abi_encode()
    );
}

/// Other modules credit the pool too: only the forfeit's own block is attributable.
fn assert_pool_credited(url: &str, forfeited_at: u64, burned: U256) {
    let unallocated = |height| {
        eth::read_call_at(
            url,
            addresses::PROMIS_LIMIT_ADDR,
            &eth::IPromisLimit::promisLimitCall {},
            height,
        )
        .expect("unallocated pool")
    };
    assert_eq!(
        unallocated(forfeited_at) - unallocated(forfeited_at - 1),
        burned,
        "the forfeit did not credit exactly the burned Gratis to the pool"
    );
}

/// Why a settlement by the Credis owner reverts, and the block read.
fn settlement_revert(world: &World, f: &CredisFixture) -> (Bytes, u64) {
    let settle = ICredisFactory::settleCredisCall {
        credisId: f.credis_id,
        amountMinor: PART,
    };
    let height = world
        .rpc
        .head(world.validators.primary_port())
        .expect("primary head");
    let reason = eth::read_call_revert_data_at(
        &url(world),
        CREDIS_FACTORY_ADDRESS,
        f.owner,
        &settle,
        eth::RevertAt {
            value: U256::ZERO,
            height,
        },
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
