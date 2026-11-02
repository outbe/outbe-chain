//! Owner operations on the ISMs and the domain table. Every write to an ISM
//! goes through [`Executor`].
//!
//! `sync` and begin-block liveness call `set_validators_and_threshold`. The
//! crosschain vote target calls `add_domain` and `remove_domain`.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::errors::HyperlaneControllerError;
use crate::executor::{Executor, RemoteCall};
use crate::precompile::IHyperlaneController;
use crate::runtime::validate_validators;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::IStorageMultisigIsm;

impl HyperlaneControllerContract<'_> {
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
            self.emit(IHyperlaneController::ValidatorsAndThresholdApplied {
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
            self.emit(IHyperlaneController::DomainRemoved { domain })
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
        self.emit(IHyperlaneController::DomainAdded { domain, ism })
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
