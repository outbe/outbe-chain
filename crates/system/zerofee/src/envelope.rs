//! Registry hooks that waive the fee of one protocol call made by a validator.

use core::marker::PhantomData;

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::{
    contract::ValidatorSet, delegation::ValidatorDelegateRole, ValidatorLifecycle,
};

use crate::hooks::{
    ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeeHook, ZeroFeeHookId, ZeroFeePolicyError,
    ZeroFeeTransaction,
};

/// Finds the validator that a signer represents, or rejects the signer.
pub(crate) type ValidatorResolver =
    fn(StorageHandle<'_>, Address) -> Result<Address, ZeroFeePolicyError>;

/// Registry hook for the protocol call `C` made by a validator.
///
/// The stateless envelope selects the transaction. The resolver then finds
/// the validator that the signer represents. That validator is the subject
/// of the fee waiver.
pub(crate) struct ValidatorCallHook<C> {
    id: ZeroFeeHookId,
    envelope: ZeroFeeEnvelope,
    resolve_validator: ValidatorResolver,
    call: PhantomData<fn() -> C>,
}

impl<C> ValidatorCallHook<C> {
    pub(crate) const fn new(
        id: ZeroFeeHookId,
        envelope: ZeroFeeEnvelope,
        resolve_validator: ValidatorResolver,
    ) -> Self {
        Self {
            id,
            envelope,
            resolve_validator,
            call: PhantomData,
        }
    }
}

impl<C: SolCall> ZeroFeeHook for ValidatorCallHook<C> {
    fn id(&self) -> ZeroFeeHookId {
        self.id
    }

    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        self.envelope.classify::<C>(self.id, tx)
    }

    fn authorize_fee_waiver(
        &self,
        storage: StorageHandle,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        let subject = (self.resolve_validator)(storage, candidate.signer)?;
        Ok(ZeroFeeAuthorization {
            hook: self.id,
            subject,
        })
    }
}

/// Stateless zero-fee envelope of one protocol call.
///
/// The call type supplies the selector and the decoder. So a hook cannot
/// accept one selector and decode another call.
pub(crate) struct ZeroFeeEnvelope {
    /// The only call target that the hook claims.
    pub(crate) target: Address,
    /// Minimum EIP-1559 fee cap of a claimed transaction.
    pub(crate) min_max_fee_per_gas: u128,
    /// Maximum calldata size of a claimed transaction.
    pub(crate) max_calldata_bytes: usize,
    /// Maximum gas limit of a claimed transaction.
    pub(crate) max_gas_limit: u64,
    /// Reason in `MalformedCalldata` when the calldata does not decode.
    pub(crate) malformed_reason: &'static str,
}

impl ZeroFeeEnvelope {
    /// Classifies `tx` as a zero-fee candidate for `hook`.
    ///
    /// The hook claims only a transaction to `target` with the call selector
    /// and a zero priority fee. All other transactions return `Ok(None)` and
    /// use the normal fee path. A claimed transaction must then pass the fee
    /// cap, value, calldata size, gas limit and decode checks in that order.
    pub(crate) fn classify<C: SolCall>(
        &self,
        hook: ZeroFeeHookId,
        tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        if !self.claims(tx, &C::SELECTOR) {
            return Ok(None);
        }
        self.check_limits(tx)?;
        if C::abi_decode(tx.call.input).is_err() {
            return Err(ZeroFeePolicyError::MalformedCalldata(
                self.malformed_reason.to_string(),
            ));
        }
        Ok(Some(ZeroFeeCandidate::new(hook, tx.signer)))
    }

    fn claims(&self, tx: &ZeroFeeTransaction<'_>, selector: &[u8; 4]) -> bool {
        tx.call.to == Some(self.target)
            && tx.call.input.starts_with(selector)
            && tx.call.max_priority_fee_per_gas == Some(0)
    }

    fn check_limits(&self, tx: &ZeroFeeTransaction<'_>) -> Result<(), ZeroFeePolicyError> {
        if tx.call.max_fee_per_gas < self.min_max_fee_per_gas {
            return Err(ZeroFeePolicyError::FeeCapTooLow {
                max_fee_per_gas: tx.call.max_fee_per_gas,
                minimum: self.min_max_fee_per_gas,
            });
        }
        if tx.call.value != U256::ZERO {
            return Err(ZeroFeePolicyError::NonZeroValue);
        }
        self.check_size(tx)
    }

    fn check_size(&self, tx: &ZeroFeeTransaction<'_>) -> Result<(), ZeroFeePolicyError> {
        if tx.call.input.len() > self.max_calldata_bytes {
            return Err(ZeroFeePolicyError::CalldataTooLarge {
                size: tx.call.input.len(),
                limit: self.max_calldata_bytes,
            });
        }
        if tx.call.gas_limit > self.max_gas_limit {
            return Err(ZeroFeePolicyError::GasLimitTooHigh {
                gas_limit: tx.call.gas_limit,
                limit: self.max_gas_limit,
            });
        }
        Ok(())
    }
}

/// Returns the validator that `signer` represents for `role`.
pub(crate) fn validator_for_role(
    storage: StorageHandle<'_>,
    signer: Address,
    role: ValidatorDelegateRole,
) -> Result<Address, ZeroFeePolicyError> {
    ValidatorSet::new(storage)
        .resolve_validator_for_role(signer, role)?
        .ok_or(ZeroFeePolicyError::UnauthorizedSigner)
}

/// Returns the validator that `signer` represents for `role`, if that
/// validator lifecycle is `Active`.
pub(crate) fn active_validator_for_role(
    storage: StorageHandle<'_>,
    signer: Address,
    role: ValidatorDelegateRole,
) -> Result<Address, ZeroFeePolicyError> {
    let validators = ValidatorSet::new(storage);
    let validator = validators
        .resolve_validator_for_role(signer, role)?
        .ok_or(ZeroFeePolicyError::UnauthorizedSigner)?;
    match validators.validator_lifecycle(validator)? {
        ValidatorLifecycle::Active(_) => Ok(validator),
        _ => Err(ZeroFeePolicyError::UnauthorizedSigner),
    }
}
