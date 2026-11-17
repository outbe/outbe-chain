//! Timely zero-fee renewal of a resident identity, independent of validator role.
pub(crate) mod prepare;
mod schedule;
pub use prepare::*;
pub use schedule::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenewalStep {
    NotDue {
        opens_at_timestamp: u64,
    },
    LeaseAlreadyExpiredNeedsRejoin,
    BlockedByActiveUpgrade,
    AwaitingFinality,
    Submitted {
        transaction_hash: alloy_primitives::B256,
        replayed: bool,
    },
}

#[cfg(test)]
pub(crate) mod tests;

/// Supervisor-owned RAM upgrade phase; no legacy sidecar journal is consulted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenewalUpgradePhaseV1 {
    Idle,
    CandidatePrepared,
    KeyProvisioned,
    CandidateKeyReady,
    SubmissionPrepared,
    Submitted,
    Finalized,
    TerminalMissedCutoff,
    Promoted {
        candidate_manifest_hash: alloy_primitives::B256,
        successor_policy_hash: alloy_primitives::B256,
    },
}
