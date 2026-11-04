//! Block `>= 2` (`SteadyState`) parent finalization accounting and late-finalize credits.
//!
//! Unifies the two halves of steady-state consensus finalization:
//! - [`parent_accounting`]: Phase 1 (`CertifiedParentAccounting`) pre-execution
//!   certificate verification, parent execution summary resolution, pre-commit,
//!   and `Rewards` / `SlashIndicator` / `Cycle` settlement.
//! - [`late_credits`]: `LateFinalizeCredits` inclusion-window and BLS aggregate
//!   verification (performed once in pre-execution and reused in the precompile),
//!   escrow authentication, matured-window settlement, and absentee slashing.

pub mod late_credits;
pub mod parent_accounting;

pub use late_credits::{authenticate_late_credit, run_late_finalize_credits};
pub use parent_accounting::{
    run_finalization_and_slashing, validate_finalized_metadata, AccountedParentArtifact,
    AccountedParentArtifactProvider,
};
