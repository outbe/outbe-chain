//! Consensus startup and the epoch application lifecycle.
//!
//! `run` orders startup adapters and hands ownership to the supervisor. The
//! supervisor keeps event selection and shutdown policy visible; completion,
//! activation, pending recovery and ceremony scheduling own DKG operations.
//! Epoch authority, DKG progress, routed channels and persistent actors have
//! separate state in `runtime`. These are private seams within the stack;
//! callers continue to use `run_consensus_stack`.

pub(super) mod activation;
pub(super) mod ceremony;
pub(super) mod completion;
pub(super) mod continuity;
pub(super) mod execution_monitor;
pub(super) mod marshal_recovery;
pub(super) mod pending_recovery;
pub(super) mod run;
pub(super) mod runtime;
pub(super) mod signer;
pub(super) mod simplex;
pub(super) mod supervisor;
pub(super) mod tee_bootstrap;
pub(super) mod threshold_recovery;
pub(super) mod transport;
pub(super) mod watchdog;
