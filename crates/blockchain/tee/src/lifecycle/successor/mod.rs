//! Resident successor preparation, cancellation and proof-verified transition.
mod candidate;
mod recovery;
mod transition;
pub use candidate::*;
pub use recovery::*;
pub use transition::*;

use super::renewal::LifecycleSignerV1;
use super::{renewal::RenewalError, LifecycleChainPort};

#[derive(Debug, thiserror::Error)]
pub enum SuccessorError<E: std::error::Error + 'static, S: std::error::Error + 'static> {
    #[error(transparent)]
    Lifecycle(#[from] RenewalError<E, S>),
    #[error("successor policy is absent or not a direct successor")]
    PolicyMismatch,
    #[error("candidate is not the same NodeHost with fresh enclave and binding identities")]
    CandidateMismatch,
    #[error("candidate lease window is invalid")]
    LeaseWindow,
    #[error("source lease expired before a strict active upgrade; paid rejoin required")]
    SourceLeaseExpiredNeedsRejoin,
    #[error("candidate context or source binding changed")]
    CandidateChanged,
    #[error("pending operation must be reconciled before another submission")]
    PendingOperation,
    #[error("verified candidate network key differs from finalized Registry")]
    NetworkKeyMismatch,
}

pub(crate) type SuccessorResult<T, P, S> =
    Result<T, SuccessorError<<P as LifecycleChainPort>::Error, <S as LifecycleSignerV1>::Error>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuccessorStep {
    AwaitingFinality,
    CancelRequired {
        context_hash: alloy_primitives::B256,
    },
    Prepared,
    RebuildRequired {
        nonce: u64,
    },
    KeyReady,
    Submitted {
        transaction_hash: alloy_primitives::B256,
        replayed: bool,
    },
    Finalized,
    LateRecoveryRequired,
}

#[cfg(test)]
mod tests;

/// Exact requested candidate binding and lease, shared by prepare and retry.
#[derive(Clone, Copy, Debug)]
pub struct SuccessorLeaseV1 {
    pub binding_id: alloy_primitives::B256,
    pub valid_until: u64,
}

#[cfg(test)]
mod cold_tests;
