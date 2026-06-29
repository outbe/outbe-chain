use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::FIDELITY_ADDRESS;

/// Domain tags for per-owner cohort slot keys, keeping the active and sold index
/// spaces disjoint (defence in depth — the two maps already have distinct base
/// slots).
const DOMAIN_ACTIVE: u8 = 1;
const DOMAIN_SOLD: u8 = 2;

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
/// RCFI is computed on demand from a per-owner cohort ledger: an active LIFO
/// stack and an append-only sold log, each a `count` + a
/// `keccak(domain ++ owner ++ index)` keyed record map (the `nod` enumeration
/// pattern).
#[storage_schema]
#[contract(addr = FIDELITY_ADDRESS)]
pub struct FidelityContract {
    // slot 0: first qualified acquisition time (seconds); 0 = no history.
    #[attribute(order = 0)]
    pub qualified_start: outbe_primitives::storage::dsl::Map<Address, u64>,

    // --- active cohort LIFO stack ---
    // slot 1: per-owner active stack depth.
    #[attribute(order = 1)]
    pub active_count: outbe_primitives::storage::dsl::Map<Address, u32>,
    // slots 2-3: active cohort record keyed by cohort_key(ACTIVE, owner, index).
    #[attribute(order = 2)]
    pub active_cohorts: outbe_primitives::storage::dsl::Map<B256, ActiveCohort>,

    // --- sold cohort append-only log ---
    // slot 4: per-owner sold log length.
    #[attribute(order = 3)]
    pub sold_count: outbe_primitives::storage::dsl::Map<Address, u32>,
    // slots 5-7: sold cohort record keyed by cohort_key(SOLD, owner, index).
    #[attribute(order = 4)]
    pub sold_cohorts: outbe_primitives::storage::dsl::Map<B256, SoldCohort>,

    // slot 8: earliest qualified_start across all accounts; anchors the global
    // synthetic-max RCFI ceiling for leagues. 0 = no account has qualified yet.
    // Timestamps are monotonic, so the first write is the chain-wide minimum.
    #[attribute(order = 5)]
    pub first_qualified_start: outbe_primitives::storage::dsl::Value<u64>,
}

/// Domain-separated per-owner cohort slot key: `keccak(domain ++ owner ++ index)`.
fn cohort_key(domain: u8, owner: Address, index: u32) -> B256 {
    let mut buf = [0u8; 1 + 20 + 4];
    buf[0] = domain;
    buf[1..21].copy_from_slice(owner.as_slice());
    buf[21..25].copy_from_slice(&index.to_be_bytes());
    keccak256(buf)
}

pub(crate) fn sold_cohort_key(owner: Address, index: u32) -> B256 {
    cohort_key(DOMAIN_SOLD, owner, index)
}

pub(crate) fn active_cohort_key(owner: Address, index: u32) -> B256 {
    cohort_key(DOMAIN_ACTIVE, owner, index)
}
