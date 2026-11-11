use outbe_primitives::storage::StorageHandle;

use outbe_hyperlanecontroller::precompile::IHyperlaneController;
use outbe_oracle::precompile::IOracle;
use outbe_validatorset::delegation::ValidatorDelegateRole;

use crate::envelope::{active_validator_for_role, ValidatorCallHook};
use crate::renewal::RenewEnclaveHook;
use crate::{hyperlane, oracle};

pub use outbe_primitives::zero_fee_types::{
    ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeeHookId, ZeroFeePolicyError, ZeroFeeTransaction,
};

#[cfg(test)]
mod failure_code_tests {
    use super::*;
    use alloy_primitives::Address;

    fn all_variants() -> Vec<ZeroFeePolicyError> {
        vec![
            ZeroFeePolicyError::UnknownHook(ZeroFeeHookId::OracleSubmitVote),
            ZeroFeePolicyError::NonZeroValue,
            ZeroFeePolicyError::CalldataTooLarge { size: 0, limit: 0 },
            ZeroFeePolicyError::GasLimitTooHigh {
                gas_limit: 0,
                limit: 0,
            },
            ZeroFeePolicyError::FeeCapTooLow {
                max_fee_per_gas: 0,
                minimum: 0,
            },
            ZeroFeePolicyError::MalformedCalldata(""),
            ZeroFeePolicyError::UnauthorizedSigner,
            ZeroFeePolicyError::AlreadyVoted,
            ZeroFeePolicyError::from(outbe_primitives::error::PrecompileError::Storage(
                String::new(),
            )),
            ZeroFeePolicyError::FreeTxDailyExhausted { used: 0, limit: 0 },
            ZeroFeePolicyError::FreeTxDailyContractCreationForbidden,
            ZeroFeePolicyError::FreeTxDailyValueNotZero,
            ZeroFeePolicyError::FreeTxDailyGasLimitExceeded {
                gas_limit: 0,
                limit: 0,
            },
            ZeroFeePolicyError::FreeTxDailyCalldataTooLarge { size: 0, limit: 0 },
            ZeroFeePolicyError::FreeTxDailyTargetNotWhitelisted { to: Address::ZERO },
        ]
    }

    #[test]
    fn codes_are_in_zero_fee_band() {
        for err in all_variants() {
            let code = err.code();
            assert!(
                (100..=199).contains(&code),
                "code {code} for {err:?} outside the zero-fee band 100..=199"
            );
        }
    }

    #[test]
    fn codes_are_pairwise_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for err in all_variants() {
            let code = err.code();
            assert!(
                seen.insert(code),
                "duplicate ZeroFeePolicyError code {code}"
            );
        }
    }

    #[test]
    fn unauthorized_signer_reason_is_hook_neutral() {
        assert_eq!(
            ZeroFeePolicyError::UnauthorizedSigner.to_string(),
            "zero-fee signer is not authorized for this transaction hook"
        );
    }

    #[test]
    fn sponsored_gas_error_preserves_code_and_display_for_both_ceilings() {
        for limit in [
            crate::constants::FREE_TX_DAILY_GAS_LIMIT,
            crate::constants::FREE_TX_TRIBUTE_FACTORY_GAS_LIMIT,
        ] {
            let gas_limit = limit + 1;
            let err = ZeroFeePolicyError::FreeTxDailyGasLimitExceeded { gas_limit, limit };
            assert_eq!(err.code(), 114);
            assert_eq!(
                err.to_string(),
                format!("free-tx gas limit {gas_limit} exceeds free-tx limit {limit}")
            );
        }
    }
}

/// A deterministic hook that can waive native fee debit for a transaction class.
pub trait ZeroFeeHook: Sync {
    /// Stable hook identifier.
    fn id(&self) -> ZeroFeeHookId;

    /// Matches the stateless transaction envelope.
    ///
    /// `Ok(None)` means this hook does not own the transaction and normal fee
    /// rules should be used. `Ok(Some(_))` means the transaction explicitly asks
    /// this hook for a zero-fee waiver and must pass stateful authorization.
    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError>;

    /// Performs stateful authorization for a candidate matched by `classify`.
    ///
    /// Returning `Ok(_)` means native fee debit may be waived for this exact
    /// transaction class. Returning `Err(_)` rejects the zero-fee attempt.
    fn authorize_fee_waiver(
        &self,
        storage: StorageHandle,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError>;
}

/// Registered zero-fee hook registry.
#[derive(Clone, Copy)]
pub struct ZeroFeeRegistry {
    hooks: &'static [&'static dyn ZeroFeeHook],
}

impl ZeroFeeRegistry {
    /// Creates a registry over a static hook list.
    pub const fn new(hooks: &'static [&'static dyn ZeroFeeHook]) -> Self {
        Self { hooks }
    }

    /// Classifies a transaction against registered hooks.
    pub fn classify(
        self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        for hook in self.hooks {
            if let Some(candidate) = hook.classify(tx)? {
                return Ok(Some(candidate));
            }
        }

        Ok(None)
    }

    /// Runs the stateful hook for a zero-fee candidate.
    pub fn authorize_fee_waiver(
        self,
        storage: StorageHandle,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        for hook in self.hooks {
            if hook.id() == candidate.hook {
                return hook.authorize_fee_waiver(storage, candidate);
            }
        }

        Err(ZeroFeePolicyError::UnknownHook(candidate.hook))
    }
}

/// `Oracle.submitVote`: an active validator or its oracle delegate that has
/// not voted in this period.
static ORACLE_SUBMIT_VOTE_HOOK: ValidatorCallHook<IOracle::submitVoteCall> = ValidatorCallHook::new(
    ZeroFeeHookId::OracleSubmitVote,
    oracle::VOTE_ENVELOPE,
    oracle::validate_oracle_submit_vote_state,
);

/// `HyperlaneController.submitCheckpoint`: an active validator or its Hyperlane
/// delegate.
static HYPERLANE_SUBMIT_CHECKPOINT_HOOK: ValidatorCallHook<
    IHyperlaneController::submitCheckpointCall,
> = ValidatorCallHook::new(
    ZeroFeeHookId::HyperlaneSubmitCheckpoint,
    hyperlane::CHECKPOINT_ENVELOPE,
    |storage, signer| active_validator_for_role(storage, signer, ValidatorDelegateRole::Hyperlane),
);

static RENEW_ENCLAVE_HOOK: RenewEnclaveHook = RenewEnclaveHook;

static ZERO_FEE_HOOKS: &[&dyn ZeroFeeHook] = &[
    &ORACLE_SUBMIT_VOTE_HOOK,
    &HYPERLANE_SUBMIT_CHECKPOINT_HOOK,
    &RENEW_ENCLAVE_HOOK,
];

/// Returns the Outbe system zero-fee hook registry.
pub const fn registry() -> ZeroFeeRegistry {
    ZeroFeeRegistry::new(ZERO_FEE_HOOKS)
}
