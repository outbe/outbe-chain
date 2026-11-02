//! Reads of Hyperlane contracts, and the guards that protect the owner calls.

use alloy_primitives::{Address, Bytes};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::errors::HyperlaneControllerError;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IOwnable, IStorageMultisigIsm};

impl HyperlaneControllerContract<'_> {
    /// Current set as stored by the local ISM (the source of truth).
    pub(crate) fn current_validators(&self) -> Result<(Vec<Address>, u8)> {
        let local_ism = self.local_ism()?;
        let ret = self.storage.staticcall(
            local_ism,
            IStorageMultisigIsm::validatorsAndThresholdCall {
                _message: Bytes::new(),
            }
            .abi_encode()
            .into(),
        )?;
        let decoded = IStorageMultisigIsm::validatorsAndThresholdCall::abi_decode_returns(&ret)
            .map_err(|_| HyperlaneControllerError::UndecodableReturn("validatorsAndThreshold"))?;
        Ok((decoded._0, decoded._1))
    }

    pub(crate) fn local_ism(&self) -> Result<Address> {
        self.require_initialized()?;
        let local = self.local_domain()?;
        let ism = self.ism_by_domain.read(&local)?;
        if ism == Address::ZERO {
            return Err(HyperlaneControllerError::LocalDomainMissing { domain: local }.into());
        }
        Ok(ism)
    }

    pub(crate) fn owner_of(&self, target: Address) -> Result<Address> {
        let ret = self
            .storage
            .staticcall(target, IOwnable::ownerCall {}.abi_encode().into())?;
        IOwnable::ownerCall::abi_decode_returns(&ret)
            .map_err(|_| HyperlaneControllerError::UndecodableReturn("owner").into())
    }

    pub(crate) fn pending_owner_of(&self, target: Address) -> Result<Address> {
        let ret = self.storage.staticcall(
            target,
            IStorageMultisigIsm::pendingOwnerCall {}.abi_encode().into(),
        )?;
        IStorageMultisigIsm::pendingOwnerCall::abi_decode_returns(&ret)
            .map_err(|_| HyperlaneControllerError::UndecodableReturn("pendingOwner").into())
    }

    pub(crate) fn require_owned(&self, contract: Address) -> Result<()> {
        if contract == Address::ZERO {
            return Err(HyperlaneControllerError::InvalidAddress.into());
        }
        let owner = self.owner_of(contract)?;
        if owner != self.address {
            return Err(HyperlaneControllerError::NotOwnedByController { contract, owner }.into());
        }
        Ok(())
    }

    pub(crate) fn require_initialized(&self) -> Result<()> {
        if !self.is_initialized()? {
            return Err(HyperlaneControllerError::NotInitialized.into());
        }
        Ok(())
    }
}
