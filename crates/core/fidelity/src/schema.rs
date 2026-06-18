use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::FIDELITY_ADDRESS;

/// Domain tags for per-owner cohort slot keys, keeping the active and sold index
/// spaces disjoint (defence in depth — the two maps already have distinct base
/// slots).
pub(crate) const DOMAIN_ACTIVE: u8 = 1;
pub(crate) const DOMAIN_SOLD: u8 = 2;

/// A live (unsold) holding. Contributes `size · T_dec(now − acquired_at)` to both
/// the efficiency numerator and denominator. Exists while `size > 0`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = size)]
pub struct ActiveCohort {
    #[key]
    pub slot_key: B256,
    #[attribute(order = 0)]
    pub size: U256,
    #[attribute(order = 1)]
    pub acquired_at: u64,
}

/// A closed (sold) holding slice. Contributes `size · (T_dec(now − acquired_at) −
/// T_dec(now − sold_at))` to the denominator only — the sale penalty that fades
/// as both ages saturate. `acquired_at` is the ORIGINAL acquisition time,
/// preserved when a boundary cohort is split on a partial sale.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = size)]
pub struct SoldCohort {
    #[key]
    pub slot_key: B256,
    #[attribute(order = 0)]
    pub size: U256,
    #[attribute(order = 1)]
    pub acquired_at: u64,
    #[attribute(order = 2)]
    pub sold_at: u64,
}

/// EVM storage layout for the Fidelity (RCFI) module.
///
/// `fidelity_indices` (slot 0) is the legacy per-address index still consumed by
/// `lysis`/`credisfactory` (mock `league_id`); it is preserved unchanged. The
/// RCFI engine adds a per-owner cohort ledger: an active LIFO stack and an
/// append-only sold log, each a `count` + a `keccak(domain ++ owner ++ index)`
/// keyed record map (the `nod` enumeration pattern).
#[storage_schema]
#[contract(addr = FIDELITY_ADDRESS)]
pub struct FidelityContract {
    // slot 0: legacy mock index (default 1), consumed by lysis/credisfactory.
    #[attribute(order = 0)]
    pub fidelity_indices: outbe_primitives::storage::dsl::Map<Address, u64>,

    // slot 1: first qualified acquisition time (seconds); 0 = no history.
    #[attribute(order = 1)]
    pub qualified_start: outbe_primitives::storage::dsl::Map<Address, u64>,

    // --- active cohort LIFO stack ---
    // slot 2: per-owner active stack depth.
    #[attribute(order = 2)]
    pub active_count: outbe_primitives::storage::dsl::Map<Address, u32>,
    // slots 3-4: active cohort record keyed by cohort_key(ACTIVE, owner, index).
    #[attribute(order = 3)]
    pub active_cohorts: outbe_primitives::storage::dsl::Map<B256, ActiveCohort>,

    // --- sold cohort append-only log ---
    // slot 5: per-owner sold log length.
    #[attribute(order = 4)]
    pub sold_count: outbe_primitives::storage::dsl::Map<Address, u32>,
    // slots 6-8: sold cohort record keyed by cohort_key(SOLD, owner, index).
    #[attribute(order = 5)]
    pub sold_cohorts: outbe_primitives::storage::dsl::Map<B256, SoldCohort>,
}

/// Domain-separated per-owner cohort slot key: `keccak(domain ++ owner ++ index)`.
pub(crate) fn cohort_key(domain: u8, owner: Address, index: u32) -> B256 {
    let mut buf = [0u8; 1 + 20 + 4];
    buf[0] = domain;
    buf[1..21].copy_from_slice(owner.as_slice());
    buf[21..25].copy_from_slice(&index.to_be_bytes());
    keccak256(buf)
}
