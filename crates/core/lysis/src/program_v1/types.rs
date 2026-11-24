//! Typed storage-independent inputs, actions, observations and failures.

use alloy_primitives::{Address, U256};
use outbe_core_common::wwd_entity_id::WwdEntityId;
use outbe_system_common::time::WorldwideDay;

pub use crate::errors::{LysisArithmeticErrorV1, ProgramErrorV1};

/// One canonical Offer body required by Lysis V1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfferInputV1 {
    pub offer_id: WwdEntityId,
    pub owner: Address,
    pub worldwide_day: WorldwideDay,
    pub issuance_currency: u16,
    pub nominal_amount_minor: U256,
    pub reference_currency: u16,
    pub offer_price_minor: U256,
}

/// A logical snapshot observation, including a deterministic unavailable value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservationValueV1<T> {
    Value(T),
    Unavailable,
}

impl<T: Copy> ObservationValueV1<T> {
    pub(crate) fn copied(&self) -> Option<T> {
        match self {
            Self::Value(value) => Some(*value),
            Self::Unavailable => None,
        }
    }
}

/// One Offer plus the two Fidelity observations, entry price
/// and reserved-target fact consumed at its semantic ordinal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedOfferV1 {
    pub offer: OfferInputV1,
    pub first_league: ObservationValueV1<u16>,
    pub second_league: ObservationValueV1<u16>,
    /// Snapshot price for this Offer's reference currency.
    pub entry_price_minor: ObservationValueV1<U256>,
    pub nod_target_available: bool,
}

/// Complete logical input for the pure semantic entrypoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramInputV1 {
    pub worldwide_day: WorldwideDay,
    pub logical_evaluation_time: u64,
    pub lysis_limit_minor: U256,
    pub offers: Vec<ObservedOfferV1>,
}

/// Which repeated Fidelity observation was made.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FidelityPhaseV1 {
    First,
    Second,
}

/// Ordered semantic observation transcript.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticObservationV1 {
    Fidelity {
        ordinal: usize,
        phase: FidelityPhaseV1,
        owner: Address,
        league: u16,
    },
    Oracle {
        ordinal: usize,
        currency: u16,
        entry_price_minor: U256,
    },
}

/// One storage-free Nod creation action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodActionV1 {
    pub source_offer_id: WwdEntityId,
    pub owner: Address,
    pub league_id: u16,
    pub entry_price_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
}

/// One league's fixed-point Gratis fraction, sorted by ascending league.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LeagueFractionV1 {
    pub league: u16,
    pub fraction: U256,
}

/// Successful storage-independent Lysis result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramResultV1 {
    pub offer_ids: Vec<WwdEntityId>,
    pub total_nominal: U256,
    pub lysis_limit_minor: U256,
    pub remaining_lysis_limit_minor: U256,
    pub league_fractions: Vec<LeagueFractionV1>,
    pub nod_actions: Vec<NodActionV1>,
    pub observations: Vec<SemanticObservationV1>,
}
