//! Oracle feeder delegation through the ValidatorSet delegate registry.

use alloy_primitives::Address;
use outbe_primitives::error::Result;

use crate::errors::OracleError;
use crate::schema::OracleContract;

impl OracleContract<'_> {
    /// Returns the feeder address for a validator. Address::ZERO means self-delegation.
    pub fn get_feeder(&self, validator: &Address) -> Result<Address> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(self.storage.clone());
        vs.get_delegate(
            *validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
        )
    }

    /// Delegates feeder consent from validator to feeder.
    pub fn delegate_feeder(&mut self, validator: Address, feeder: Address) -> Result<()> {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(self.storage.clone());
        if feeder.is_zero() {
            return vs.revoke_delegate(
                validator,
                outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
            );
        }
        vs.set_delegate(
            validator,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
            feeder,
        )
    }

    /// Resolves which validator a feeder is acting for.
    /// Returns the validator address if the caller is a valid feeder.
    pub fn resolve_validator_for_feeder(&self, caller: Address) -> Result<Address> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(self.storage.clone());
        vs.resolve_validator_for_role(
            caller,
            outbe_validatorset::delegation::ValidatorDelegateRole::Oracle,
        )?
        .ok_or_else(|| OracleError::NotActiveOracleSigner.into())
    }
}
