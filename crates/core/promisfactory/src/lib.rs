//! Promisfactory precompile (`0x2337`). Thin orchestration layer on top of the
//! Promis token (`outbe_promis`, `0x1337`).
//!
//! Owns the promis mint/burn orchestration. `mine` wraps `Promis::mine` and
//! records the Fidelity acquisition cohort (`cohort_in`). `mine_coen` is the
//! symmetric sale path: it wraps `Promis::burn`, records the Fidelity sale cohort
//! (`cohort_out`), mints native COEN 1:1, and emits `CoenMined`. Keeping both here
//! puts the token movement and Fidelity bookkeeping in one place. The
//! `PromisMinted`/`PromisBurned` events are emitted by the Promis token itself.

pub mod api;
pub mod precompile;
pub mod runtime;

#[cfg(test)]
mod tests;
