//! Pool-facing zero-fee policy.
//!
//! The pool asks this trait whether a transaction is a fee waiver. The
//! protocol crate that owns the hooks implements the trait and installs it
//! before the node builds the pool. Until that install, the pool uses
//! [`NormalFeePolicy`] and every transaction pays the normal fee.
//! The process-wide `OnceLock` is a temporary bridge until constructor injection.

use std::sync::{Arc, OnceLock};

use outbe_primitives::{
    storage::StorageHandle,
    zero_fee_types::{
        BootstrapAccountView, BootstrapCandidate, BootstrapTransactionView, ZeroFeeAuthorization,
        ZeroFeeCandidate, ZeroFeeHookId, ZeroFeePolicyError, ZeroFeeTransaction,
    },
};
use thiserror::Error;

static INSTALLED_POLICY: OnceLock<Arc<dyn TxPoolZeroFeePolicy>> = OnceLock::new();

/// Classifies and authorizes zero-fee transactions for the pool.
///
/// Install the production implementation with [`install_txpool_zero_fee_policy`]
/// before [`crate::OutbePoolBuilder`] builds the pool. The builder copies the
/// installed policy into the validator and the ordering.
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

/// The production policy was already installed.
#[derive(Debug, Error)]
#[error("txpool zero-fee policy is already installed")]
pub struct TxPoolPolicyAlreadyInstalled;

/// Install the policy that [`crate::OutbePoolBuilder`] copies into the pool.
///
/// Call this before the node builds the pool. A later install does not change
/// a pool that was already built.
pub fn install_txpool_zero_fee_policy(
    policy: Arc<dyn TxPoolZeroFeePolicy>,
) -> Result<(), TxPoolPolicyAlreadyInstalled> {
    INSTALLED_POLICY
        .set(policy)
        .map_err(|_| TxPoolPolicyAlreadyInstalled)
}

pub(crate) fn active_policy() -> Arc<dyn TxPoolZeroFeePolicy> {
    INSTALLED_POLICY
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(NormalFeePolicy))
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
