//! The phases every lifecycle shares, one step each with the entity as a parameter:
//! qualification, payment, the call, the forfeit, and mining into native COEN.

use std::time::Duration;

use alloy_primitives::U256;
use cucumber::{then, when};

use super::chain::poll_until;
use super::entity::{Currency, Entity, Phase, Rail};
use super::markets::{coen_rate, MYR_ISO};
use super::{guards, payment, redeem};
use crate::world::forge::DEPLOYER_KEY;
use crate::world::settlement_currency::USD_ISO;
use crate::world::{test_issuance, World};

/// Seeded closed days above the call price: Nod's production threshold, which covers
/// the DEV two-of-three that gems and Intex run under.
pub(crate) const CALL_WINDOW_SEED_DAYS: u32 = 21;
const QUALIFY_TIMEOUT: Duration = Duration::from_secs(180);
const CALL_TIMEOUT: Duration = Duration::from_secs(300);

#[then(expr = "every {entity} reads Issued and carries its terms")]
fn every_entity_issued(world: &mut World, entity: Entity) {
    entity.lifecycle().assert_issued(world);
}

#[then(expr = "no {entity} can be paid before it qualifies")]
fn unqualified_refused(world: &mut World, entity: Entity) {
    let [first, _] = entity.lifecycle().targets(world, Phase::Qualified);
    guards::assert_unqualified_refused(world, &first);
}

#[then(expr = "no {entity} can be transferred")]
fn not_transferable(world: &mut World, entity: Entity) {
    let [first, _] = entity.lifecycle().targets(world, Phase::Qualified);
    guards::assert_soulbound(world, &first);
}

/// One closed day between floor and call price qualifies without calling; a close a
/// target chain already holds is kept, since it records a day's price once.
#[when(expr = "the reference rate stands above the {entity} floor")]
fn rate_above_floor(world: &mut World, entity: Entity) {
    let lifecycle = entity.lifecycle();
    let floor = lifecycle.floor(world);
    let rate = lifecycle.recorded_close(world).unwrap_or_else(|| {
        let call_price = lifecycle.call_price(world);
        assert!(
            floor < call_price,
            "the {entity:?} floor must sit below its call price"
        );
        (floor + call_price) / U256::from(2)
    });
    assert!(
        rate > floor,
        "the reference rate {rate} must clear the {entity:?} floor"
    );
    seed_closed_days(world, 1, rate);
}

#[then(expr = "every {entity} qualifies")]
fn every_entity_qualifies(world: &mut World, entity: Entity) {
    poll_until(
        QUALIFY_TIMEOUT,
        || format!("not every {entity:?} qualified on the seeded day"),
        || entity.lifecycle().qualified(world),
    );
}

#[then(
    expr = "a/an {entity} payment is refused for a stale snapshot, a foreign currency or a note bound to another holding"
)]
fn payment_guards(world: &mut World, entity: Entity) {
    // The holding the scenario next pays in MYR by PayNote, so that payment differs from
    // the refused note only in the holding it is bound to.
    let [other, target] = entity.lifecycle().targets(world, Phase::Qualified);
    assert_eq!(
        target.issuance_currency, MYR_ISO,
        "the guarded holding is issued in MYR"
    );
    guards::assert_payment_guards(world, &target, &other);
}

#[then(expr = "an unpaid {entity} cannot be mined")]
fn unpaid_unminable(world: &mut World, entity: Entity) {
    let [first, _] = entity.lifecycle().targets(world, Phase::Qualified);
    guards::assert_unpaid_unminable(world, &first);
}

#[when(
    expr = "a {phase} {entity} is paid in {currency} by {rail} and another in {currency} by {rail}"
)]
fn pay_two(
    world: &mut World,
    phase: Phase,
    entity: Entity,
    first: Currency,
    first_rail: Rail,
    second: Currency,
    second_rail: Rail,
) {
    let lifecycle = entity.lifecycle();
    let [first_target, second_target] = lifecycle.targets(world, phase);
    for (target, currency, rail) in [
        (first_target, first, first_rail),
        (second_target, second, second_rail),
    ] {
        let terms = lifecycle.settlement_terms(world, &target, phase);
        let paid = payment::pay(world, &target, rail, currency.0, terms);
        world.state.entity_lifecycle.payments.push(paid);
    }
}

#[then("each payment settles exactly its quote into its currency's vault")]
fn payments_settle(world: &mut World) {
    let ledger = &world.state.entity_lifecycle;
    payment::assert_payments_settled(world, &ledger.payments[ledger.verified..]);
    world.state.entity_lifecycle.verified = world.state.entity_lifecycle.payments.len();
}

#[when(expr = "the reference rate holds above the {entity} call price across the call window")]
fn rate_above_call_price(world: &mut World, entity: Entity) {
    let lifecycle = entity.lifecycle();
    let rate = lifecycle
        .recorded_close(world)
        .unwrap_or_else(|| coen_rate(USD_ISO));
    assert!(
        rate > lifecycle.call_price(world),
        "the reference rate {rate} must clear the {entity:?} call price"
    );
    seed_closed_days(world, CALL_WINDOW_SEED_DAYS, rate);
}

#[then(expr = "every unpaid {entity} becomes Called while what was paid stays Settled")]
fn unpaid_called(world: &mut World, entity: Entity) {
    let lifecycle = entity.lifecycle();
    poll_until(
        CALL_TIMEOUT,
        || format!("not every unpaid {entity:?} was called"),
        || lifecycle.called(world),
    );
    lifecycle.assert_paid_settled(world);
}

#[when(expr = "the call notice lapses on the unpaid {entity}")]
fn notice_lapses(world: &mut World, entity: Entity) {
    entity.lifecycle().lapse_notice(world);
}

#[then(
    expr = "the unpaid {entity} is forfeited and its unpaid load returns to the unallocated pool"
)]
fn unpaid_forfeited(world: &mut World, entity: Entity) {
    entity.lifecycle().assert_forfeited(world);
}

#[then(expr = "every paid {entity} stays Settled")]
fn paid_stay_settled(world: &mut World, entity: Entity) {
    entity.lifecycle().assert_paid_settled(world);
}

#[when(expr = "the owners mine every paid {entity}")]
fn mine_paid(world: &mut World, entity: Entity) {
    let mined = entity.lifecycle().mine_paid(world);
    world.state.entity_lifecycle.mined = mined;
}

#[then("each paid load lands in its owner's balance")]
fn mined_loads_land(world: &mut World) {
    redeem::assert_mined(world, &world.state.entity_lifecycle.mined);
}

#[then(expr = "a mined {entity} cannot be mined again")]
fn mined_again(world: &mut World, entity: Entity) {
    let [first, _] = entity.lifecycle().targets(world, Phase::Qualified);
    guards::assert_mined_unminable(world, &first);
}

#[when("the owners redeem what they mined into COEN")]
fn redeem_into_coen(world: &mut World) {
    let redeemed = redeem::redeem(world, &world.state.entity_lifecycle.mined);
    world.state.entity_lifecycle.redeemed = redeemed;
}

#[then("each owner's native COEN grows by exactly that load")]
fn native_coen_grows(world: &mut World) {
    let ledger = &world.state.entity_lifecycle;
    redeem::assert_redeemed(world, &ledger.mined, &ledger.redeemed);
}

/// Seed the last `days` closed UTC days of COEN/USD, the reference every entity prices by.
fn seed_closed_days(world: &World, days: u32, rate: U256) {
    let url = world.rpc.url(world.validators.primary_port());
    test_issuance::seed_day_vwaps(&url, DEPLOYER_KEY, USD_ISO, days, rate)
        .expect("seed the closed days' VWAP");
}
