//! The ICA router, the ISM and the hook of each domain, and the guards that
//! protect them.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::abi::ICrosschainGov;
use crate::errors::HyperlaneControllerError;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IHyperlaneRouter, IOwnable, IStorageMultisigIsm};
use crate::{Owner, RemoteCall};

impl HyperlaneControllerContract<'_> {
    /// Hyperlane domain of this chain (= chain id).
    pub fn local_domain(&self) -> Result<u32> {
        u32::try_from(self.storage.chain_id()?)
            .map_err(|_| HyperlaneControllerError::InvalidDomain.into())
    }

    pub fn is_initialized(&self) -> Result<bool> {
        Ok(self.ica_router.read()? != Address::ZERO)
    }

    pub fn require_initialized(&self) -> Result<()> {
        if !self.is_initialized()? {
            return Err(HyperlaneControllerError::NotInitialized.into());
        }
        Ok(())
    }

    /// Requires a non-zero domain other than the Outbe domain.
    pub fn require_remote_domain(&self, domain: u32) -> Result<()> {
        if domain == 0 {
            return Err(HyperlaneControllerError::InvalidDomain.into());
        }
        if domain == self.local_domain()? {
            return Err(HyperlaneControllerError::LocalDomainNotAllowed { domain }.into());
        }
        Ok(())
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

    /// Requires that this frame (`0x…EE14`) owns `contract`.
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

    /// Connects a remote chain (or replaces its ISM). The local domain is
    /// fixed at `initialize`.
    pub fn add_domain(
        &mut self,
        domain: u32,
        (ism, hook): (Address, Address),
        owner: &mut impl Owner,
    ) -> Result<()> {
        self.require_initialized()?;
        self.require_remote_domain(domain)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.write_domain(domain, ism, hook)?;
            accept_remote_ism(owner, domain, ism)
        })
    }

    /// Disconnects a remote chain from validator synchronisation.
    pub fn remove_domain(&mut self, domain: u32) -> Result<()> {
        self.require_initialized()?;
        self.require_remote_domain(domain)?;
        if self.ism_by_domain.read(&domain)? == Address::ZERO {
            return Err(HyperlaneControllerError::UnknownDomain { domain }.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.ism_by_domain.clear(&domain)?;
            let remaining: Vec<u32> = self
                .domains
                .read_all()?
                .into_iter()
                .filter(|entry| *entry != domain)
                .collect();
            self.domains.clear()?;
            for entry in remaining {
                self.domains.push(entry)?;
            }
            self.emit(ICrosschainGov::DomainRemoved { domain })
        })
    }

    pub(crate) fn write_domain(&mut self, domain: u32, ism: Address, hook: Address) -> Result<()> {
        if domain == 0 {
            return Err(HyperlaneControllerError::InvalidDomain.into());
        }
        if ism == Address::ZERO || hook == Address::ZERO {
            return Err(HyperlaneControllerError::InvalidAddress.into());
        }
        if self.ism_by_domain.read(&domain)? == Address::ZERO {
            self.domains.push(domain)?;
        }
        self.ism_by_domain.write(&domain, ism)?;
        self.hook_by_domain.write(&domain, hook)?;
        self.emit(ICrosschainGov::DomainAdded { domain, ism })
    }

    /// Enrolls the ICA router of `domain` on the Outbe ICA router, so that
    /// `call_remote` can reach that chain. The router keeps the first
    /// enrollment forever, so the same router is a no-op and another router
    /// reverts.
    pub(crate) fn enroll_ica_router(
        &mut self,
        domain: u32,
        router: Address,
        owner: &mut impl Owner,
    ) -> Result<()> {
        let ica_router = self.ica_router.read()?;
        let ret = self.storage.staticcall(
            ica_router,
            IHyperlaneRouter::routersCall { _domain: domain }
                .abi_encode()
                .into(),
        )?;
        let enrolled = IHyperlaneRouter::routersCall::abi_decode_returns(&ret).map_err(|_| {
            HyperlaneControllerError::UndecodableReturn("InterchainAccountRouter routers")
        })?;
        if enrolled == router.into_word() {
            return Ok(());
        }
        if enrolled != B256::ZERO {
            return Err(HyperlaneControllerError::RemoteRouterMismatch { domain, enrolled }.into());
        }
        let data: Bytes = IHyperlaneRouter::enrollRemoteRouterCall {
            _domain: domain,
            _router: router.into_word(),
        }
        .abi_encode()
        .into();
        owner.call_local(ica_router, U256::ZERO, data).map(|_| ())
    }
}

/// Accepts the pending ownership of a remote ISM from the Interchain
/// Account: `callRemote(domain, [ism.acceptOwnership()])`.
pub(crate) fn accept_remote_ism(owner: &mut impl Owner, domain: u32, ism: Address) -> Result<()> {
    let accept = IStorageMultisigIsm::acceptOwnershipCall {}.abi_encode();
    owner.call_remote(domain, &[RemoteCall::new(ism, accept)])?;
    Ok(())
}
