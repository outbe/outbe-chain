//! `outbe-engine` — bridge layer between pure `outbe-consensus`
//! (Simplex/hybrid/DKG/proof) and the EVM/node side (`outbe-evm`,
//! `outbe-validatorset`, `outbe-node`).
//!
//! Owns:
//! * `stack.rs` — engine startup, epoch loop, reshare monitoring.
//! * `validators.rs` — ValidatorSet storage reader (Reth state → Commonware
//!   participant set).
//! * `peer_manager/` — P2P peer registration against `outbe-node`.
//! * `args.rs` — `ConsensusArgs` CLI bundle for engine startup.
//! * `bridge.rs` — re-exports for `ConsensusExecutionBridge` wiring.

pub mod args;
pub mod bridge;
pub(crate) mod follow_transport;
pub(crate) mod marshal_update_reporter;
pub(crate) mod peer_manager;
pub mod stack;
pub mod tee_bootstrap;
pub mod validators;

pub use args::ConsensusArgs;
pub use stack::run_consensus_stack;
