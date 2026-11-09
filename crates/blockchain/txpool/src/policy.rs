//! Pool-facing zero-fee policy.
//!
//! The pool asks this trait whether a transaction is a fee waiver. The node
//! passes the policy into [`crate::OutbePoolBuilder`]. Without one, the pool
//! uses [`NormalFeePolicy`] and every transaction pays the normal fee.

use std::sync::Arc;

use outbe_primitives::{
    storage::StorageHandle,
    zero_fee_types::{
        BootstrapAccountView, BootstrapCandidate, BootstrapTransactionView, ZeroFeeAuthorization,
        ZeroFeeCandidate, ZeroFeeHookId, ZeroFeePolicyError, ZeroFeeTransaction,
    },
};
/// Classifies and authorizes zero-fee transactions for the pool.
///
/// [`crate::OutbePoolBuilder`] copies this policy into the validator and the ordering.
pub trait TxPoolZeroFeePolicy: Send + Sync + std::fmt::Debug {
    /// `Ok(Some(_))` means the transaction asks this hook for a waiver.
    /// `Ok(None)` means normal fee rules apply. `Err` rejects the transaction.
    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError>;

    /// Reserved ordering class for a hook that must outrank the tip market.
    ///
    /// The match stays exhaustive so a new hook makes an explicit ordering
    /// decision. `None` keeps the normal tip priority.
    fn priority_class(&self, hook: ZeroFeeHookId) -> Option<u8> {
        match hook {
            ZeroFeeHookId::OracleSubmitVote => Some(1),
            ZeroFeeHookId::HyperlaneSubmitCheckpoint => Some(1),
            ZeroFeeHookId::RenewEnclave => Some(1),
        }
    }

    /// Stateful authorization for a candidate returned by [`Self::classify`].
    fn authorize(
        &self,
        storage: StorageHandle<'_>,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError>;

    /// `Some` means the signed envelope is the one-time bootstrap.
    /// `None` means the caller continues with normal fee validation.
    fn classify_bootstrap(&self, tx: &BootstrapTransactionView<'_>) -> Option<BootstrapCandidate>;

    /// Rechecks the pre-state account for a bootstrap candidate.
    fn authorize_bootstrap(
        &self,
        candidate: BootstrapCandidate,
        account: BootstrapAccountView,
    ) -> bool;

    /// Stateless sponsorship envelope check.
    ///
    /// `Err` means the transaction is not a sponsorship request. The pool
    /// then uses the normal fee path. It does not reject the transaction.
    fn classify_sponsorship(&self, tx: &ZeroFeeTransaction<'_>) -> Result<(), ZeroFeePolicyError>;

    /// Pool admission check for a sponsorship request.
    ///
    /// This check does not read the daily quota. Quota exhaustion is a
    /// block-time soft failure, so the pool still admits the transaction.
    fn precheck_sponsorship(
        &self,
        signer: alloy_primitives::Address,
    ) -> Result<(), ZeroFeePolicyError>;
}

pub(crate) fn normal_policy() -> Arc<dyn TxPoolZeroFeePolicy> {
    Arc::new(NormalFeePolicy)
}

/// Admits nothing as a waiver. Every transaction uses the normal fee path.
#[derive(Debug, Default)]
struct NormalFeePolicy;

impl TxPoolZeroFeePolicy for NormalFeePolicy {
    fn classify(
        &self,
        _tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        Ok(None)
    }

    fn authorize(
        &self,
        _storage: StorageHandle<'_>,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        Err(ZeroFeePolicyError::UnknownHook(candidate.hook))
    }

    fn classify_bootstrap(&self, _tx: &BootstrapTransactionView<'_>) -> Option<BootstrapCandidate> {
        None
    }

    fn authorize_bootstrap(
        &self,
        _candidate: BootstrapCandidate,
        _account: BootstrapAccountView,
    ) -> bool {
        false
    }

    fn classify_sponsorship(&self, tx: &ZeroFeeTransaction<'_>) -> Result<(), ZeroFeePolicyError> {
        Err(ZeroFeePolicyError::FeeCapTooLow {
            max_fee_per_gas: tx.call.max_fee_per_gas,
            minimum: tx.call.max_fee_per_gas,
        })
    }

    fn precheck_sponsorship(
        &self,
        _signer: alloy_primitives::Address,
    ) -> Result<(), ZeroFeePolicyError> {
        Ok(())
    }
}
