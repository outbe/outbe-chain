//! The ISMs and the domain table: reads of the ISMs, the validator rotation,
//! and adding or removing a chain. Every write to an ISM goes through
//! [`Executor`].
//!
//! `sync` and begin-block liveness call `set_validators_and_threshold`. The
//! vote actions in `crosschaingov` call `enroll_ica_router`, `add_domain` and
//! `remove_domain`.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::errors::HyperlaneControllerError;
use crate::executor::{Executor, RemoteCall};
use crate::precompile::ICrosschainGov;
use crate::runtime::validate_validators;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IHyperlaneRouter, IStorageMultisigIsm};

impl HyperlaneControllerContract<'_> {
    /// Enrolls the ICA router of `domain` on the Outbe ICA router, so that
    /// `call_remote` can reach that chain. The router keeps the first
    /// enrollment forever, so the same router is a no-op and another router
    /// reverts.
    pub fn enroll_ica_router(
        &mut self,
        executor: &mut impl Executor,
        domain: u32,
        router: Address,
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
        let data = IHyperlaneRouter::enrollRemoteRouterCall {
            _domain: domain,
            _router: router.into_word(),
        }
        .abi_encode();
        executor
            .call_local(ica_router, U256::ZERO, data.into())
            .map(|_| ())
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

    /// Full rotation: `setValidatorsAndThreshold` on every remote ISM through
    /// the Interchain Account, then on the local ISM. All calls run under one
    /// checkpoint. Any failure leaves every ISM untouched.
    pub fn set_validators_and_threshold(
        &mut self,
        executor: &mut impl Executor,
        validators: &[Address],
        threshold: u8,
    ) -> Result<()> {
        validate_validators(validators, threshold)?;
        let local = self.local_domain()?;
        let local_ism = self.local_ism()?;
        let data: Bytes = IStorageMultisigIsm::setValidatorsAndThresholdCall {
            _validators: validators.to_vec(),
            _threshold: threshold,
        }
        .abi_encode()
        .into();

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            for domain in self.domains.read_all()? {
                if domain == local {
                    continue;
                }
                let ism = self.ism_by_domain.read(&domain)?;
                executor.call_remote(
                    domain,
                    &[RemoteCall {
                        to: ism,
                        value: U256::ZERO,
                        data: data.clone(),
                    }],
                )?;
            }
            executor.call_local(local_ism, U256::ZERO, data)?;
            self.emit(ICrosschainGov::ValidatorsAndThresholdApplied {
                threshold,
                validatorCount: U256::from(validators.len()),
            })
        })
    }

    /// Connects a remote chain (or replaces its ISM). The local domain is
    /// fixed at `initialize`.
    pub fn add_domain(
        &mut self,
        executor: &mut impl Executor,
        domain: u32,
        ism: Address,
        hook: Address,
    ) -> Result<()> {
        self.require_initialized()?;
        self.require_remote_domain(domain)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.write_domain(domain, ism, hook)?;
            accept_remote_ism(executor, domain, ism)
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

    pub fn require_remote_domain(&self, domain: u32) -> Result<()> {
        if domain == 0 {
            return Err(HyperlaneControllerError::InvalidDomain.into());
        }
        if domain == self.local_domain()? {
            return Err(HyperlaneControllerError::LocalDomainNotAllowed { domain }.into());
        }
        Ok(())
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
}

/// Accepts the pending ownership of a remote ISM from the Interchain Account:
/// `callRemote(domain, [ism.acceptOwnership()])`.
pub(crate) fn accept_remote_ism(
    executor: &mut impl Executor,
    domain: u32,
    ism: Address,
) -> Result<()> {
    executor.call_remote(
        domain,
        &[RemoteCall {
            to: ism,
            value: U256::ZERO,
            data: IStorageMultisigIsm::acceptOwnershipCall {}
                .abi_encode()
                .into(),
        }],
    )?;
    Ok(())
}
