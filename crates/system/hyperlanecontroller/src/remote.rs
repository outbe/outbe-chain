//! Sub-calls from the controller to Hyperlane contracts, and the guards that
//! protect them.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::errors::HyperlaneControllerError;
use crate::precompile::IHyperlaneController;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IInterchainAccountRouter, IOwnable, IStorageMultisigIsm};

/// One call executed by the controller's Interchain Account on a remote chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCall {
    pub to: Address,
    pub value: U256,
    pub data: Bytes,
}

impl HyperlaneControllerContract<'_> {
    pub(crate) fn dispatch_remote(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256> {
        let router = self.ica_router.read()?;
        let fee = self.quote(router, domain)?;
        self.require_balance(fee)?;
        let calls = calls
            .iter()
            .map(|call| IInterchainAccountRouter::Call {
                to: call.to.into_word(),
                value: call.value,
                data: call.data.clone(),
            })
            .collect();
        let ret = self.storage.call(
            router,
            fee,
            IInterchainAccountRouter::callRemoteCall {
                _destination: domain,
                _calls: calls,
            }
            .abi_encode()
            .into(),
        )?;
        let message_id = IInterchainAccountRouter::callRemoteCall::abi_decode_returns(&ret)
            .map_err(|_| {
                HyperlaneControllerError::UndecodableReturn("InterchainAccountRouter callRemote")
            })?;
        self.emit(IHyperlaneController::RemoteCallDispatched {
            domain,
            messageId: message_id,
            fee,
        })?;
        Ok(message_id)
    }

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

    pub(crate) fn quote(&self, router: Address, domain: u32) -> Result<U256> {
        let ret = self.storage.staticcall(
            router,
            IInterchainAccountRouter::quoteGasPaymentCall {
                _destination: domain,
            }
            .abi_encode()
            .into(),
        )?;
        IInterchainAccountRouter::quoteGasPaymentCall::abi_decode_returns(&ret).map_err(|_| {
            HyperlaneControllerError::UndecodableReturn("InterchainAccountRouter quoteGasPayment")
                .into()
        })
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

    pub(crate) fn require_balance(&self, required: U256) -> Result<()> {
        let available = self.storage.balance(self.address)?;
        if available < required {
            return Err(HyperlaneControllerError::InsufficientBalance {
                required,
                available,
            }
            .into());
        }
        Ok(())
    }
}
