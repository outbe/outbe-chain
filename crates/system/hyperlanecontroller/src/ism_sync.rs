//! Owner operations on the ISMs and the domain table.
//!
//! `sync` and begin-block liveness call `set_validators_and_threshold`.
//! `call_remote`, `call_local`, `add_domain`, and `remove_domain` have no
//! production caller.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::errors::HyperlaneControllerError;
use crate::precompile::IHyperlaneController;
use crate::remote::RemoteCall;
use crate::runtime::validate_validators;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::IStorageMultisigIsm;

impl HyperlaneControllerContract<'_> {
    /// Full rotation: `setValidatorsAndThreshold` on every remote ISM through
    /// the Interchain Account, then on the local ISM. All calls run under one
    /// checkpoint. Any failure leaves every ISM untouched.
    pub fn set_validators_and_threshold(
        &mut self,
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
                self.dispatch_remote(
                    domain,
                    &[RemoteCall {
                        to: ism,
                        value: U256::ZERO,
                        data: data.clone(),
                    }],
                )?;
            }
            self.storage.call(local_ism, U256::ZERO, data)?;
            self.emit(IHyperlaneController::ValidatorsAndThresholdApplied {
                threshold,
                validatorCount: U256::from(validators.len()),
            })
        })
    }

    /// Generic Interchain Account call on a remote `domain`.
    pub fn call_remote(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256> {
        self.require_initialized()?;
        if calls.is_empty() {
            return Err(HyperlaneControllerError::EmptyCalls.into());
        }
        self.require_remote_domain(domain)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.dispatch_remote(domain, calls))
    }

    /// Generic owner call on this chain, paid from the controller's balance
    /// when `value` is non-zero.
    pub fn call_local(&mut self, to: Address, value: U256, data: Bytes) -> Result<Bytes> {
        if to == Address::ZERO {
            return Err(HyperlaneControllerError::InvalidAddress.into());
        }
        self.require_balance(value)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let ret = self.storage.call(to, value, data)?;
            self.emit(IHyperlaneController::LocalCallExecuted { target: to, value })?;
            Ok(ret)
        })
    }

    /// Connects a remote chain (or replaces its ISM). The local domain is
    /// fixed at `initialize`.
    pub fn add_domain(&mut self, domain: u32, ism: Address, hook: Address) -> Result<()> {
        self.require_initialized()?;
        self.require_remote_domain(domain)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.write_domain(domain, ism, hook)?;
            self.accept_remote_ism(domain, ism)?;
            Ok(())
        })
    }

    /// Accepts the pending ownership of a remote ISM from the Interchain
    /// Account: `callRemote(domain, [ism.acceptOwnership()])`.
    pub(crate) fn accept_remote_ism(&mut self, domain: u32, ism: Address) -> Result<()> {
        self.dispatch_remote(
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

    pub(crate) fn require_remote_domain(&self, domain: u32) -> Result<()> {
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
