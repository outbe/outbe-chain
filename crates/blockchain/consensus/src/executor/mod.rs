//! Executor actor - maintains forkchoice state and forwards finalized blocks.
//!
//! Handles chain-head canonicalization and finalization updates from the consensus engine
//! to Reth's execution layer.

pub mod actor;
pub mod ingress;

pub use ingress::Mailbox;
