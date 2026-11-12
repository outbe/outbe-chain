//! Consensus startup and the epoch application lifecycle.
//!
//! `run` orders startup adapters and hands ownership to the supervisor. The
//! supervisor keeps event selection and shutdown policy visible. Completion,
//! activation, pending recovery and ceremony scheduling own DKG operations.
//! Epoch authority, DKG progress, routed channels and persistent actors have
//! separate state in `runtime`. These are private seams within the stack;
//! callers continue to use `run_consensus_stack`.

pub mod activation;
pub mod ceremony;
pub(super) mod completion;
pub mod continuity;
pub(super) mod execution_monitor;
mod expiry;
mod helpers;
pub mod marshal_recovery;
pub(super) mod pending_recovery;
pub mod run;
pub(super) mod runtime;
pub mod signer;
pub mod simplex;
pub mod supervisor;
pub(super) mod tee_bootstrap;
pub(super) mod threshold_recovery;
pub mod transport;
pub mod watchdog;
