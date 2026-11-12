//! Outbe consensus integration with Commonware Simplex.
//!
//! Bridges Commonware's Simplex BFT consensus engine with Reth's execution layer.
//!
//! - Phase 1: Static validator set, RoundRobin leader election.
//! - Phase 2: Dynamic validator reading from EVM state.
//! - Phase 3: BLS12-381 threshold VRF scheme, Random leader election.
//! - Phase 4: Hybrid scheme (BLS individual attribution + BLS threshold VRF).

pub mod cli;
pub mod dkg;
pub mod dkg_actor;
pub mod dkg_manager;
pub mod errors;
pub mod metrics;

pub mod application_shutdown;
pub mod args;
pub mod execution_port;
pub mod follow_transport;
pub mod follower_shutdown;
pub mod marshal_update_reporter;
pub mod peer_manager;
pub mod protocol_state;
pub mod stack;
pub mod tee_bootstrap;
pub mod validators;
pub use follow_transport::read_upstream_tribute_offer_public_key;
pub use outbe_primitives::{digest, proof};

#[cfg(test)]
mod marshal_resolver_p2p_tests;
#[cfg(test)]
mod marshal_tests;
#[cfg(test)]
mod shutdown_tests;
#[cfg(test)]
mod telemetry_label_tests;

#[cfg(any(test, feature = "test-utils"))]
pub use outbe_simplex::finalized_admission_test_utils;
#[cfg(any(test, feature = "test-utils"))]
#[path = "test_harness.rs"]
pub mod test_harness;

pub mod ancestry_readiness;
pub mod application;
pub mod executor;
pub mod finalization;
pub mod follow;
pub mod reporter;
pub(crate) mod test_faults;
#[cfg(test)]
mod test_fixtures;

pub use outbe_simplex::{
    block, bls, committee_chain, committee_provider, config, epoch_registry, epoch_subchannels,
    forfeit, hybrid, marshal_types, storage_identity, timing, util, vrf_safety,
};
