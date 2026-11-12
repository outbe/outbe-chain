//! Lightweight follower: cold-sync finalized blocks from an upstream node and
//! verify them against the chain's committee, WITHOUT running consensus.
//!
//! **Trust model - committee-chaining.** outbe's finalize certificate is an
//! atomic aggregate of individual MinPk votes and a mandatory MinSig threshold
//! VRF proof over a *committee-bound* namespace. Both are verified with the
//! epoch-scoped committee material, which changes on every reshare. A follower
//! therefore:
//!
//! 1. anchors the START epoch on the genesis validator set for a fresh node,
//!    or an already accepted local finalized boundary on snapshot/restart.
//! 2. reads each later epoch's committee from a `CommitteePreAnnounce` in the
//!    previous epoch's last finalized block, verifies that block with the already
//!    trusted previous committee, and only then installs the next verifier.
//!
//! All inputs are public on-chain data carried in the boundary block
//! `extra_data` (the full DKG [`Output`] - players + polynomial). The follower
//! never holds any DKG secret. [`CommitteeChain`] implements this chaining.
//! `phase0_spike_*` (the de-risk gate) and the tests below exercise it.

mod driver;
pub mod engine;
mod resolver;
mod stubs;
pub mod upstream;

pub use engine::{run_follow_engine, FollowEngineConfig};
pub use outbe_simplex::committee_chain::*;
pub use outbe_simplex::epoch_registry::{BoundaryObservationError, FollowerEpocher};
pub use upstream::{FinalizedSource, LocalBlockSource, TipSource};

#[cfg(test)]
use crate::{
    digest::Digest,
    hybrid::{HybridScheme, VrfMaterialProvider},
};
#[cfg(test)]
use alloy_primitives::B256;
#[cfg(test)]
use commonware_consensus::types::Epoch;
#[cfg(test)]
use commonware_cryptography::bls12381;
#[cfg(test)]
use commonware_cryptography::bls12381::primitives::variant::MinSig;

#[cfg(test)]
use commonware_parallel::Sequential;

#[cfg(test)]
mod tests;
