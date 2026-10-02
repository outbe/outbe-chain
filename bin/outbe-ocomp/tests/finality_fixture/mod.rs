//! Real q=3/4 Commonware finality and Ethereum-MPT fixture for OCOMP process tests.
//!
//! This module does not provide an accepting verifier or inject a trusted
//! result. It constructs the same canonical proof bytes consumed by the
//! production verifier, allowing process-boundary tests to derive `JobIntent`
//! exclusively from authenticated node responses.

#![allow(dead_code)]

mod build;
mod proof;
mod provider;

pub(crate) const FINALIZED_EPOCH: u64 = 2;
pub(crate) const FINALIZED_VIEW: u64 = 3;
pub(crate) const PARENT_VIEW: u64 = 2;
pub(crate) const VRF_MATERIAL_VERSION: u64 = 5;
pub(crate) const SIGNER_INDICES: [u32; 3] = [0, 1, 2];

pub use build::finalized_intent_proof_fixture;
pub use proof::fixture_league;
