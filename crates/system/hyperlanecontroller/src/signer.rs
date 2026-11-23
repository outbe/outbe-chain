//! Hyperlane signer of a validator, and the validator that a transaction
//! sender acts for.

use alloy_primitives::Address;
use outbe_primitives::error::Result;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::delegation::ValidatorDelegateRole;

use crate::errors::HyperlaneControllerError;
use crate::precompile::IHyperlaneController;
use crate::schema::HyperlaneControllerContract;

impl HyperlaneControllerContract<'_> {
    /// Registers the key that the validator of `caller` uses to sign Hyperlane
    /// checkpoints. Zero resets to the validator address.
    pub fn set_hyperlane_signer(&mut self, caller: Address, signer: Address) -> Result<()> {
        let validator = self.validator_of_sender(caller)?;
        let effective = if signer == Address::ZERO {
            validator
        } else {
            signer
        };
        let validator_set = ValidatorSet::new(self.storage.clone());
        for record in validator_set.get_active_validators()? {
            let other = record.validator_address;
            if other == validator {
                continue;
            }
            if other == effective || self.hyperlane_signer(other)? == effective {
                return Err(HyperlaneControllerError::SignerTaken {
                    signer: effective,
                    validator: other,
                }
                .into());
            }
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.signer_of.write(&validator, signer)?;
            self.emit(IHyperlaneController::HyperlaneSignerSet { validator, signer })
        })
    }

    /// Hyperlane signers of the active validators, in validator-set order.
    pub(crate) fn active_signers(&self) -> Result<Vec<Address>> {
        let validator_set = ValidatorSet::new(self.storage.clone());
        validator_set
            .get_active_validators()?
            .into_iter()
            .map(|record| self.hyperlane_signer(record.validator_address))
            .collect()
    }

    /// Hyperlane signer of `validator`: the registered key, else the
    /// validator address itself.
    pub fn hyperlane_signer(&self, validator: Address) -> Result<Address> {
        let signer = self.signer_of.read(&validator)?;
        Ok(if signer == Address::ZERO {
            validator
        } else {
            signer
        })
    }

    /// The active validator a transaction sender acts for: the validator
    /// itself or its Hyperlane delegate.
    pub(crate) fn validator_of_sender(&self, sender: Address) -> Result<Address> {
        let validator_set = ValidatorSet::new(self.storage.clone());
        let validator = validator_set
            .resolve_validator_for_role(sender, ValidatorDelegateRole::Hyperlane)?
            .ok_or(HyperlaneControllerError::NotActiveValidator { caller: sender })?;
        if !validator_set
            .validator_lifecycle(validator)?
            .is_active_status()
        {
            return Err(HyperlaneControllerError::NotActiveValidator { caller: sender }.into());
        }
        Ok(validator)
    }
}
