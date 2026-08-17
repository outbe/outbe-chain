use alloy_primitives::{address, Address, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::constants::{
    MAX_DECAY_HALVINGS, MIN_PRINCIPAL_FOR_UNIT, MULTIPLIER_ONE, ORIGINATION_FLOOR, RECOVERY_STEP,
    RECOVERY_VALUE_UNIT,
};
use crate::schema::{CcaContract, CcaState};

const CHAIN_ID: u64 = 1;
const REGISTERED_AT: u64 = 1_700_000_000;

fn agent() -> Address {
    address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC")
}

fn other_agent() -> Address {
    address!("0xDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD")
}

fn owner() -> Address {
    address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
}

fn other_owner() -> Address {
    address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB")
}

fn day(n: u32) -> WorldwideDay {
    WorldwideDay::new(20_260_101 + n)
}

/// A principal comfortably over the dust guard.
fn principal() -> U256 {
    U256::from(1_000_000_000u64) // $1,000
}

fn with_cca<R>(f: impl FnOnce(CcaContract<'_>) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_timestamp(U256::from(REGISTERED_AT));
    StorageHandle::enter(&mut storage, |storage| f(CcaContract::new(storage)))
}

/// Registers `agent()` and hands back the contract.
fn with_registered<R>(f: impl FnOnce(CcaContract<'_>) -> R) -> R {
    with_cca(|mut cca| {
        cca.register(agent(), REGISTERED_AT).unwrap();
        f(cca)
    })
}

// ---------------------------------------------------------------------------
// Registration (§8.1)
// ---------------------------------------------------------------------------

#[test]
fn registration_is_permissionless_and_starts_unpenalized() {
    with_cca(|mut cca| {
        assert!(
            !cca.can_originate(agent()).unwrap(),
            "an unregistered address cannot originate"
        );

        cca.register(agent(), REGISTERED_AT).unwrap();
        let record = cca.get_cca(agent()).unwrap();
        assert_eq!(record.multiplier, MULTIPLIER_ONE);
        assert_eq!(record.registered_at, REGISTERED_AT);
        assert_eq!(record.registration_state().unwrap(), CcaState::Active);
        assert!(cca.can_originate(agent()).unwrap());

        // Registering twice is an error, not a silent reset of the multiplier.
        assert!(cca.register(agent(), REGISTERED_AT + 1).is_err());
        assert!(cca.register(Address::ZERO, REGISTERED_AT).is_err());
    });
}

#[test]
fn deregistering_releases_the_identity_for_re_registration() {
    with_registered(|mut cca| {
        cca.deregister(agent()).unwrap();
        assert!(!cca.can_originate(agent()).unwrap());
        assert!(cca.get_cca(agent()).is_err(), "the record stops existing");

        assert!(cca.deregister(agent()).is_err(), "already deregistered");
        assert!(cca.deregister(other_agent()).is_err(), "never registered");

        // The address is not burned: the same agent can come back.
        cca.register(agent(), REGISTERED_AT + 100).unwrap();
        assert!(cca.can_originate(agent()).unwrap());
        let record = cca.get_cca(agent()).unwrap();
        assert_eq!(record.registration_state().unwrap(), CcaState::Active);
        assert_eq!(record.registered_at, REGISTERED_AT + 100);
    });
}

/// Exit is total, and that cuts both ways: leaving discards the penalty the
/// agent was carrying, so re-registration is a clean slate. §8.2's exit haircut
/// on the bond is what is supposed to price this; the bond does not exist yet,
/// so today the round trip is free. Pinned as a test because it is a decision,
/// not an accident — if the bond lands and this stays free, that is the bug.
#[test]
fn leaving_and_returning_discards_a_penalty() {
    with_registered(|mut cca| {
        cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        assert_eq!(
            cca.multiplier_of(agent()).unwrap(),
            U256::from(900_000_000_000_000_000u64)
        );

        cca.deregister(agent()).unwrap();
        // A void that lands after the exit has no record to bite.
        cca.record_void(agent(), MULTIPLIER_ONE).unwrap();

        cca.register(agent(), REGISTERED_AT + 100).unwrap();
        assert_eq!(
            cca.multiplier_of(agent()).unwrap(),
            MULTIPLIER_ONE,
            "a returning agent starts over, exactly like a fresh address would"
        );
        assert_eq!(cca.get_cca(agent()).unwrap().recovery_progress, U256::ZERO);
    });
}

// ---------------------------------------------------------------------------
// Accountability (§8.4)
// ---------------------------------------------------------------------------

#[test]
fn a_fully_unpaid_void_costs_the_whole_penalty_step() {
    with_registered(|mut cca| {
        cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        assert_eq!(
            cca.multiplier_of(agent()).unwrap(),
            U256::from(900_000_000_000_000_000u64),
            "m = 1 × (1 - 0.10)"
        );
    });
}

#[test]
fn a_partial_void_costs_a_proportional_share_of_the_step() {
    with_registered(|mut cca| {
        // A 25% remainder costs a quarter of the 10% step: m = 1 × 0.975.
        let quarter = MULTIPLIER_ONE / U256::from(4u64);
        cca.record_void(agent(), quarter).unwrap();
        assert_eq!(
            cca.multiplier_of(agent()).unwrap(),
            U256::from(975_000_000_000_000_000u64)
        );
    });
}

#[test]
fn voids_compound_downward_and_never_go_negative() {
    with_registered(|mut cca| {
        for _ in 0..3 {
            cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        }
        // 0.9³ = 0.729
        assert_eq!(
            cca.multiplier_of(agent()).unwrap(),
            U256::from(729_000_000_000_000_000u64)
        );

        // Many more voids drive it toward zero without underflowing.
        for _ in 0..200 {
            cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        }
        assert!(cca.multiplier_of(agent()).unwrap() < ORIGINATION_FLOOR);
    });
}

#[test]
fn quarantine_blocks_origination_below_half() {
    with_registered(|mut cca| {
        // Six full voids: 0.9^6 = 0.531441, still above the floor.
        for _ in 0..6 {
            cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        }
        assert!(cca.multiplier_of(agent()).unwrap() >= ORIGINATION_FLOOR);
        assert!(cca.can_originate(agent()).unwrap());

        // The seventh drops it to 0.4782969 and closes origination.
        cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        assert!(cca.multiplier_of(agent()).unwrap() < ORIGINATION_FLOOR);
        assert!(!cca.can_originate(agent()).unwrap());
        assert!(
            cca.record_origination(agent(), owner(), principal(), day(0))
                .is_ok(),
            "recording still works — the gate is on the caller's side"
        );
    });
}

#[test]
fn a_void_by_an_unregistered_agent_is_ignored() {
    with_cca(|mut cca| {
        // Positions outlive both registration and exit, so a void can arrive
        // for an agent with no record. It must not revert the void itself,
        // which is a collateral operation.
        cca.record_void(other_agent(), MULTIPLIER_ONE).unwrap();
        assert_eq!(cca.multiplier_of(other_agent()).unwrap(), MULTIPLIER_ONE);
    });
}

#[test]
fn only_repayment_restores_the_multiplier() {
    with_registered(|mut cca| {
        cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        let penalized = cca.multiplier_of(agent()).unwrap();

        // Under one recovery unit banks progress but earns no step yet.
        cca.record_settlement(agent(), RECOVERY_VALUE_UNIT - U256::from(1u64))
            .unwrap();
        assert_eq!(cca.multiplier_of(agent()).unwrap(), penalized);

        // The remainder is carried, so the next wei tips it over a whole unit.
        cca.record_settlement(agent(), U256::from(1u64)).unwrap();
        assert_eq!(
            cca.multiplier_of(agent()).unwrap(),
            penalized + RECOVERY_STEP
        );
    });
}

#[test]
fn recovery_is_capped_at_one_and_cannot_be_banked_in_advance() {
    with_registered(|mut cca| {
        // An unpenalized agent banks nothing, however much it settles.
        cca.record_settlement(agent(), RECOVERY_VALUE_UNIT * U256::from(100u64))
            .unwrap();
        assert_eq!(cca.multiplier_of(agent()).unwrap(), MULTIPLIER_ONE);

        // So a later void takes the full step rather than being pre-paid for.
        cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        assert_eq!(
            cca.multiplier_of(agent()).unwrap(),
            U256::from(900_000_000_000_000_000u64)
        );

        // Overshooting recovery clamps at 1 rather than exceeding it.
        cca.record_settlement(agent(), RECOVERY_VALUE_UNIT * U256::from(1_000u64))
            .unwrap();
        assert_eq!(cca.multiplier_of(agent()).unwrap(), MULTIPLIER_ONE);
    });
}

#[test]
fn a_new_void_discards_progress_banked_toward_the_previous_one() {
    with_registered(|mut cca| {
        cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        // Bank a partial step.
        cca.record_settlement(agent(), RECOVERY_VALUE_UNIT / U256::from(2u64))
            .unwrap();
        assert_eq!(
            cca.get_cca(agent()).unwrap().recovery_progress,
            RECOVERY_VALUE_UNIT / U256::from(2u64)
        );

        cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        assert!(
            cca.get_cca(agent()).unwrap().recovery_progress.is_zero(),
            "recovery is measured from where the agent now stands"
        );
    });
}

// ---------------------------------------------------------------------------
// Origination units (§8.3)
// ---------------------------------------------------------------------------

#[test]
fn the_first_origination_for_an_owner_earns_a_whole_unit() {
    with_registered(|mut cca| {
        let units = cca
            .record_origination(agent(), owner(), principal(), day(0))
            .unwrap();
        assert_eq!(units, MULTIPLIER_ONE);
        assert_eq!(
            cca.origination_units(day(0), agent()).unwrap(),
            MULTIPLIER_ONE
        );
        assert_eq!(
            cca.day_originators(day(0)).unwrap(),
            vec![(agent(), MULTIPLIER_ONE)]
        );
    });
}

#[test]
fn repeat_owner_originations_halve_the_unit() {
    with_registered(|mut cca| {
        // One per day, so the day cap does not mask the decay.
        let mut expected = MULTIPLIER_ONE;
        let mut total = U256::ZERO;
        for d in 0..4 {
            let units = cca
                .record_origination(agent(), owner(), principal(), day(d))
                .unwrap();
            assert_eq!(units, expected, "day {d}");
            total += units;
            expected /= U256::from(2u64);
        }
        // 1 + ½ + ¼ + ⅛
        assert_eq!(total, MULTIPLIER_ONE * U256::from(15u64) / U256::from(8u64));

        // A different owner starts fresh at a whole unit.
        assert_eq!(
            cca.record_origination(agent(), other_owner(), principal(), day(9))
                .unwrap(),
            MULTIPLIER_ONE
        );
    });
}

#[test]
fn the_decay_stops_shrinking_at_the_halving_cap() {
    with_registered(|mut cca| {
        for d in 0..MAX_DECAY_HALVINGS + 3 {
            cca.record_origination(agent(), owner(), principal(), day(d))
                .unwrap();
        }
        let floor_unit = MULTIPLIER_ONE >> MAX_DECAY_HALVINGS;
        assert!(
            !floor_unit.is_zero(),
            "a decayed unit is still distinguishable from none"
        );
        assert_eq!(
            cca.origination_units(day(MAX_DECAY_HALVINGS + 2), agent())
                .unwrap(),
            floor_unit
        );
    });
}

#[test]
fn only_one_unit_per_owner_per_day() {
    with_registered(|mut cca| {
        assert_eq!(
            cca.record_origination(agent(), owner(), principal(), day(0))
                .unwrap(),
            MULTIPLIER_ONE
        );
        // A second position for the same owner on the same day earns nothing —
        // and does not advance the decay, so tomorrow is still worth ½.
        assert_eq!(
            cca.record_origination(agent(), owner(), principal(), day(0))
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(
            cca.origination_units(day(0), agent()).unwrap(),
            MULTIPLIER_ONE
        );

        assert_eq!(
            cca.record_origination(agent(), owner(), principal(), day(1))
                .unwrap(),
            MULTIPLIER_ONE / U256::from(2u64)
        );
    });
}

#[test]
fn a_dust_principal_earns_nothing_and_burns_neither_the_day_slot_nor_the_decay() {
    with_registered(|mut cca| {
        let dust = MIN_PRINCIPAL_FOR_UNIT - U256::from(1u64);
        assert_eq!(
            cca.record_origination(agent(), owner(), dust, day(0))
                .unwrap(),
            U256::ZERO
        );
        assert_eq!(cca.origination_units(day(0), agent()).unwrap(), U256::ZERO);
        assert!(
            cca.day_originators(day(0)).unwrap().is_empty(),
            "a day nobody earned on has no originators"
        );

        // The owner's slot for the day is untouched, and so is the decay.
        assert_eq!(
            cca.record_origination(agent(), owner(), principal(), day(0))
                .unwrap(),
            MULTIPLIER_ONE
        );
    });
}

#[test]
fn exactly_the_minimum_principal_counts() {
    with_registered(|mut cca| {
        assert_eq!(
            cca.record_origination(agent(), owner(), MIN_PRINCIPAL_FOR_UNIT, day(0))
                .unwrap(),
            MULTIPLIER_ONE
        );
    });
}

#[test]
fn units_are_recorded_per_agent_and_the_day_index_lists_each_once() {
    with_cca(|mut cca| {
        cca.register(agent(), REGISTERED_AT).unwrap();
        cca.register(other_agent(), REGISTERED_AT).unwrap();

        cca.record_origination(agent(), owner(), principal(), day(0))
            .unwrap();
        cca.record_origination(agent(), other_owner(), principal(), day(0))
            .unwrap();
        cca.record_origination(other_agent(), owner(), principal(), day(0))
            .unwrap();

        let originators = cca.day_originators(day(0)).unwrap();
        assert_eq!(originators.len(), 2, "each agent appears once");
        assert_eq!(
            originators.iter().find(|(a, _)| *a == agent()).unwrap().1,
            MULTIPLIER_ONE * U256::from(2u64),
            "two distinct owners, both first-time"
        );
        assert_eq!(
            originators
                .iter()
                .find(|(a, _)| *a == other_agent())
                .unwrap()
                .1,
            MULTIPLIER_ONE
        );

        // Days are independent.
        assert!(cca.day_originators(day(1)).unwrap().is_empty());
    });
}

#[test]
fn recording_an_origination_for_an_unregistered_agent_is_rejected() {
    with_cca(|mut cca| {
        assert!(cca
            .record_origination(other_agent(), owner(), principal(), day(0))
            .is_err());
    });
}

#[test]
fn the_multiplier_is_not_folded_into_the_recorded_units() {
    with_registered(|mut cca| {
        cca.record_void(agent(), MULTIPLIER_ONE).unwrap();
        // Units are the agent's raw activity; the multiplier is applied at
        // distribution time, so a later recovery is not lost to a frozen weight.
        assert_eq!(
            cca.record_origination(agent(), owner(), principal(), day(0))
                .unwrap(),
            MULTIPLIER_ONE
        );
        assert_eq!(
            cca.multiplier_of(agent()).unwrap(),
            U256::from(900_000_000_000_000_000u64)
        );
    });
}

#[test]
fn an_unregistered_address_weighs_neutrally() {
    with_cca(|cca| {
        assert_eq!(cca.multiplier_of(other_agent()).unwrap(), MULTIPLIER_ONE);
        assert!(cca.get_cca(other_agent()).is_err());
    });
}
