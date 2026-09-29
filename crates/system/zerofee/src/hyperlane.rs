use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::precompile::IHyperlaneController;
use outbe_primitives::{addresses::HYPERLANE_CONTROLLER_ADDRESS, storage::StorageHandle};
use outbe_validatorset::ValidatorLifecycle;

use crate::hooks::{
    ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeeHook, ZeroFeeHookId, ZeroFeePolicyError,
    ZeroFeeTransaction,
};

/// Maximum calldata bytes accepted for a zero-fee checkpoint submission.
pub const MAX_ZERO_FEE_CHECKPOINT_CALLDATA_BYTES: usize = 1024;

/// Maximum gas limit accepted for a zero-fee checkpoint submission.
pub const MAX_ZERO_FEE_CHECKPOINT_GAS_LIMIT: u64 = 1_500_000;

/// Minimum EIP-1559 fee cap accepted by Reth's public txpool.
pub const MIN_ZERO_FEE_CHECKPOINT_MAX_FEE_PER_GAS: u128 = MIN_PROTOCOL_BASE_FEE as u128;

/// Zero-fee hook for validator Hyperlane checkpoint submissions.
#[derive(Debug, Clone, Copy)]
pub struct HyperlaneSubmitCheckpointHook;

impl ZeroFeeHook for HyperlaneSubmitCheckpointHook {
    fn id(&self) -> ZeroFeeHookId {
        ZeroFeeHookId::HyperlaneSubmitCheckpoint
    }

    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        if tx.to != Some(HYPERLANE_CONTROLLER_ADDRESS) {
            return Ok(None);
        }

        if !tx
            .input
            .starts_with(&IHyperlaneController::submitCheckpointCall::SELECTOR)
        {
            return Ok(None);
        }

        if tx.max_priority_fee_per_gas != Some(0) {
            return Ok(None);
        }

        if tx.max_fee_per_gas < MIN_ZERO_FEE_CHECKPOINT_MAX_FEE_PER_GAS {
            return Err(ZeroFeePolicyError::FeeCapTooLow {
                max_fee_per_gas: tx.max_fee_per_gas,
                minimum: MIN_ZERO_FEE_CHECKPOINT_MAX_FEE_PER_GAS,
            });
        }

        if tx.value != U256::ZERO {
            return Err(ZeroFeePolicyError::NonZeroValue);
        }

        if tx.input.len() > MAX_ZERO_FEE_CHECKPOINT_CALLDATA_BYTES {
            return Err(ZeroFeePolicyError::CalldataTooLarge {
                size: tx.input.len(),
                limit: MAX_ZERO_FEE_CHECKPOINT_CALLDATA_BYTES,
            });
        }

        if tx.gas_limit > MAX_ZERO_FEE_CHECKPOINT_GAS_LIMIT {
            return Err(ZeroFeePolicyError::GasLimitTooHigh {
                gas_limit: tx.gas_limit,
                limit: MAX_ZERO_FEE_CHECKPOINT_GAS_LIMIT,
            });
        }

        if IHyperlaneController::submitCheckpointCall::abi_decode(tx.input).is_err() {
            return Err(ZeroFeePolicyError::MalformedCalldata(
                "submitCheckpoint(uint32,bytes32,uint32,bytes32,bytes) decode failed".to_string(),
            ));
        }

        Ok(Some(ZeroFeeCandidate::new(self.id(), tx.signer)))
    }

    fn authorize_fee_waiver(
        &self,
        storage: StorageHandle,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let validator = vs
            .resolve_validator_for_role(
                candidate.signer,
                outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
            )?
            .ok_or(ZeroFeePolicyError::UnauthorizedSigner)?;
        if !matches!(
            vs.validator_lifecycle(validator)?,
            ValidatorLifecycle::Active(_)
        ) {
            return Err(ZeroFeePolicyError::UnauthorizedSigner);
        }
        Ok(ZeroFeeAuthorization {
            hook: self.id(),
            subject: validator,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address, Bytes, B256};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    const VALIDATOR: Address = address!("0x1111111111111111111111111111111111111111");
    const FEEDER: Address = address!("0x2222222222222222222222222222222222222222");

    fn checkpoint_calldata() -> Bytes {
        IHyperlaneController::submitCheckpointCall {
            domain: 54_322_345,
            root: B256::repeat_byte(1),
            index: 7,
            messageId: B256::repeat_byte(2),
            signature: Bytes::from(vec![3u8; 65]),
        }
        .abi_encode()
        .into()
    }

    fn checkpoint_tx(
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: Option<u128>,
        input: &[u8],
    ) -> ZeroFeeTransaction<'_> {
        ZeroFeeTransaction {
            signer: FEEDER,
            to: Some(HYPERLANE_CONTROLLER_ADDRESS),
            value: U256::ZERO,
            input,
            gas_limit: 1_000_000,
            max_fee_per_gas,
            max_priority_fee_per_gas,
        }
    }

    #[test]
    fn registry_classifies_submit_checkpoint_shape() {
        let input = checkpoint_calldata();
        let tx = checkpoint_tx(
            MIN_ZERO_FEE_CHECKPOINT_MAX_FEE_PER_GAS,
            Some(0),
            input.as_ref(),
        );
        let candidate = crate::registry().classify(&tx).unwrap().unwrap();
        assert_eq!(candidate.hook, ZeroFeeHookId::HyperlaneSubmitCheckpoint);
        assert_eq!(candidate.signer, FEEDER);

        let paid = checkpoint_tx(1_000_000_000, Some(1), input.as_ref());
        assert_eq!(crate::registry().classify(&paid).unwrap(), None);

        let malformed = checkpoint_tx(
            MIN_ZERO_FEE_CHECKPOINT_MAX_FEE_PER_GAS,
            Some(0),
            &IHyperlaneController::submitCheckpointCall::SELECTOR,
        );
        assert!(matches!(
            crate::registry().classify(&malformed).unwrap_err(),
            ZeroFeePolicyError::MalformedCalldata(_)
        ));
    }

    #[test]
    fn only_an_active_validator_or_its_oracle_delegate_is_waived() {
        let mut storage = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut storage, |storage| {
            let input = checkpoint_calldata();
            let tx = checkpoint_tx(
                MIN_ZERO_FEE_CHECKPOINT_MAX_FEE_PER_GAS,
                Some(0),
                input.as_ref(),
            );
            let candidate = crate::registry().classify(&tx).unwrap().unwrap();
            let err = crate::registry()
                .authorize_fee_waiver(storage.clone(), candidate)
                .unwrap_err();
            assert_eq!(err, ZeroFeePolicyError::UnauthorizedSigner);

            let owner = address!("0xffffffffffffffffffffffffffffffffffffffff");
            let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            vs.config_owner.write(owner).unwrap();
            vs.config_max_validators.write(1).unwrap();
            vs.test_register_validator_without_pop(VALIDATOR, &[1; 48])
                .unwrap();
            vs.test_activate_validator_canonically(
                VALIDATOR,
                outbe_validatorset::StakeProjection::new(U256::from(1), None),
                U256::from(1),
            )
            .unwrap();
            vs.set_delegate(
                VALIDATOR,
                outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
                FEEDER,
            )
            .unwrap();

            let auth = crate::registry()
                .authorize_fee_waiver(storage, candidate)
                .unwrap();
            assert_eq!(auth.subject, VALIDATOR);
        });
    }
}
