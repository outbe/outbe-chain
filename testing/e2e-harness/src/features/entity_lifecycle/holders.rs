//! The owners a lifecycle issues its gems or Nods to: one holding each, on real keys,
//! so every owner pays its own note, mines and redeems for itself.

use alloy_primitives::Address;

use super::entity::Phase;
use crate::internal::eth;
use crate::world::World;

pub(crate) const HOLDERS: usize = 5;
/// Two paid while qualified, two inside the notice, and the last one left to forfeit.
pub(crate) const FORFEITED: usize = 4;
/// The holdings left unpaid once the qualified ones are paid.
pub(crate) const UNPAID_AT_CALL: [usize; 3] = [2, 3, FORFEITED];
const FUNDING_COEN: u64 = 10;

/// The two holdings paid in `phase`, in the order the scenario names their payments.
pub(crate) fn paid_in(phase: Phase) -> [usize; 2] {
    match phase {
        Phase::Qualified => [0, 1],
        Phase::Called => [2, 3],
    }
}

/// The owners of every holding but the forfeited one.
pub(crate) fn paid() -> std::ops::Range<usize> {
    0..FORFEITED
}

pub(crate) fn key(seed: u64, index: usize) -> String {
    format!("0x{:064x}", seed + index as u64 + 1)
}

pub(crate) fn address(seed: u64, index: usize) -> Address {
    eth::address_of(&key(seed, index)).expect("holder address")
}

/// Gas for the notes, mining and redemptions each owner sends itself.
pub(crate) fn fund(world: &World, seed: u64) {
    let url = world.rpc.url(world.validators.primary_port());
    let funder = world
        .validators
        .get(0)
        .evm_key()
        .expect("validator-0 funding key");
    for index in 0..HOLDERS {
        eth::send_value(&url, address(seed, index), &funder, eth::coen(FUNDING_COEN))
            .expect("fund the holder");
    }
}
