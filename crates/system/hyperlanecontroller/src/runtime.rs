use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::errors::HyperlaneControllerError;
use crate::precompile::IHyperlaneController;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IInterchainAccountRouter, IOwnable, IStorageMultisigIsm};
use outbe_validatorset::contract::ValidatorSet;

/// Hyperlane's multisig threshold is a `uint8`.
const MAX_VALIDATORS: usize = u8::MAX as usize;

/// One call executed by the controller's Interchain Account on a remote chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteCall {
    pub to: Address,
    pub value: U256,
    pub data: Bytes,
}

impl HyperlaneControllerContract<'_> {
    /// Hyperlane domain of this chain (= chain id).
    pub fn local_domain(&self) -> Result<u32> {
        u32::try_from(self.storage.chain_id()?)
            .map_err(|_| HyperlaneControllerError::InvalidDomain.into())
    }

    pub fn is_initialized(&self) -> Result<bool> {
        Ok(self.router.read()? != Address::ZERO)
    }

    // ----------------------------------------------------------------------
    // Direct selectors
    // ----------------------------------------------------------------------

    /// One-shot bootstrap: accepts the pending ownership of the local ISM,
    /// verifies the router is already owned by the controller, and stores the
    /// router plus the whole `domain -> ISM` table.
    ///
    /// Only the current owner of the local ISM (the deployer that staged
    /// `transferOwnership` to this precompile) may call it.
    pub fn initialize(
        &mut self,
        caller: Address,
        router: Address,
        domains: &[u32],
        isms: &[Address],
    ) -> Result<()> {
        if self.is_initialized()? {
            return Err(HyperlaneControllerError::AlreadyInitialized.into());
        }
        if domains.len() != isms.len() {
            return Err(HyperlaneControllerError::IsmLengthMismatch.into());
        }
        let local = self.local_domain()?;
        let local_ism = domains
            .iter()
            .zip(isms)
            .find(|(domain, _)| **domain == local)
            .map(|(_, ism)| *ism)
            .ok_or(HyperlaneControllerError::LocalDomainMissing { domain: local })?;

        let owner = self.owner_of(local_ism)?;
        if owner != caller {
            return Err(HyperlaneControllerError::NotIsmOwner { caller, owner }.into());
        }
        let pending = self.pending_owner_of(local_ism)?;
        if pending != self.address {
            return Err(HyperlaneControllerError::IsmNotPendingToController { pending }.into());
        }
        self.require_owned(router)?;

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.storage.call(
                local_ism,
                U256::ZERO,
                IStorageMultisigIsm::acceptOwnershipCall {}
                    .abi_encode()
                    .into(),
            )?;
            self.router.write(router)?;
            for (domain, ism) in domains.iter().zip(isms) {
                self.write_domain(*domain, *ism)?;
            }
            self.emit(IHyperlaneController::Initialized { router })
        })
    }

    /// Records a top-up; the value itself is credited by the payable route.
    pub fn fund(&mut self, from: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(HyperlaneControllerError::ZeroFund.into());
        }
        self.emit(IHyperlaneController::Funded { from, amount })
    }

    /// Mirrors the active Outbe validator set into every ISM (validators =
    /// active validators, threshold = [`consensus_threshold`]). Returns `false`
    /// when the local ISM already matches, so a keeper can call it every epoch.
    pub fn sync(&mut self) -> Result<bool> {
        let validator_set = ValidatorSet::new(self.storage.clone());
        let active: Vec<Address> = validator_set
            .get_active_validators()?
            .into_iter()
            .map(|record| record.validator_address)
            .collect();
        let threshold = consensus_threshold(active.len())?;
        let (current, current_threshold) = self.current_validators()?;
        if current_threshold == threshold && same_set(&current, &active) {
            return Ok(false);
        }
        self.set_validators_and_threshold(&active, threshold)?;
        Ok(true)
    }

    // ----------------------------------------------------------------------
    // Owner operations (trigger not wired yet)
    // ----------------------------------------------------------------------

    /// Adds `validator` to the current set. `threshold` replaces the current
    /// one when given; otherwise the current threshold is kept.
    pub fn add_validator(&mut self, validator: Address, threshold: Option<u8>) -> Result<()> {
        let (mut validators, current) = self.current_validators()?;
        if validators.contains(&validator) {
            return Err(HyperlaneControllerError::InvalidValidator { validator }.into());
        }
        validators.push(validator);
        self.set_validators_and_threshold(&validators, threshold.unwrap_or(current))
    }

    /// Removes `validator` from the current set. `threshold` replaces the
    /// current one when given; otherwise the current threshold is kept and
    /// must still fit the smaller set.
    pub fn remove_validator(&mut self, validator: Address, threshold: Option<u8>) -> Result<()> {
        let (mut validators, current) = self.current_validators()?;
        let before = validators.len();
        validators.retain(|entry| *entry != validator);
        if validators.len() == before {
            return Err(HyperlaneControllerError::InvalidValidator { validator }.into());
        }
        self.set_validators_and_threshold(&validators, threshold.unwrap_or(current))
    }

    /// Changes only the threshold, keeping the current set.
    pub fn set_threshold(&mut self, threshold: u8) -> Result<()> {
        let (validators, _) = self.current_validators()?;
        self.set_validators_and_threshold(&validators, threshold)
    }

    /// Full rotation: `setValidatorsAndThreshold` on every remote ISM through
    /// the Interchain Account, then on the local ISM. One checkpoint - any
    /// failure leaves every ISM untouched.
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
    pub fn add_domain(&mut self, domain: u32, ism: Address) -> Result<()> {
        self.require_initialized()?;
        self.require_remote_domain(domain)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.write_domain(domain, ism))
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

    // ----------------------------------------------------------------------
    // Internals
    // ----------------------------------------------------------------------

    fn dispatch_remote(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256> {
        let router = self.router.read()?;
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
    fn current_validators(&self) -> Result<(Vec<Address>, u8)> {
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

    fn local_ism(&self) -> Result<Address> {
        self.require_initialized()?;
        let local = self.local_domain()?;
        let ism = self.ism_by_domain.read(&local)?;
        if ism == Address::ZERO {
            return Err(HyperlaneControllerError::LocalDomainMissing { domain: local }.into());
        }
        Ok(ism)
    }

    fn quote(&self, router: Address, domain: u32) -> Result<U256> {
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

    fn owner_of(&self, target: Address) -> Result<Address> {
        let ret = self
            .storage
            .staticcall(target, IOwnable::ownerCall {}.abi_encode().into())?;
        IOwnable::ownerCall::abi_decode_returns(&ret)
            .map_err(|_| HyperlaneControllerError::UndecodableReturn("owner").into())
    }

    fn pending_owner_of(&self, target: Address) -> Result<Address> {
        let ret = self.storage.staticcall(
            target,
            IStorageMultisigIsm::pendingOwnerCall {}.abi_encode().into(),
        )?;
        IStorageMultisigIsm::pendingOwnerCall::abi_decode_returns(&ret)
            .map_err(|_| HyperlaneControllerError::UndecodableReturn("pendingOwner").into())
    }

    fn require_owned(&self, contract: Address) -> Result<()> {
        if contract == Address::ZERO {
            return Err(HyperlaneControllerError::InvalidAddress.into());
        }
        let owner = self.owner_of(contract)?;
        if owner != self.address {
            return Err(HyperlaneControllerError::NotOwnedByController { contract, owner }.into());
        }
        Ok(())
    }

    fn require_initialized(&self) -> Result<()> {
        if !self.is_initialized()? {
            return Err(HyperlaneControllerError::NotInitialized.into());
        }
        Ok(())
    }

    fn require_remote_domain(&self, domain: u32) -> Result<()> {
        if domain == 0 {
            return Err(HyperlaneControllerError::InvalidDomain.into());
        }
        if domain == self.local_domain()? {
            return Err(HyperlaneControllerError::LocalDomainNotAllowed { domain }.into());
        }
        Ok(())
    }

    fn require_balance(&self, required: U256) -> Result<()> {
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

    fn write_domain(&mut self, domain: u32, ism: Address) -> Result<()> {
        if domain == 0 {
            return Err(HyperlaneControllerError::InvalidDomain.into());
        }
        if ism == Address::ZERO {
            return Err(HyperlaneControllerError::InvalidAddress.into());
        }
        if self.ism_by_domain.read(&domain)? == Address::ZERO {
            self.domains.push(domain)?;
        }
        self.ism_by_domain.write(&domain, ism)?;
        self.emit(IHyperlaneController::DomainAdded { domain, ism })
    }
}

/// Bridge threshold for `n` active validators: the same 2/3 rule as the
/// validator vote quorum, rounded up (`ceil(2n / 3)`).
pub fn consensus_threshold(active: usize) -> Result<u8> {
    if active == 0 || active > MAX_VALIDATORS {
        return Err(HyperlaneControllerError::InvalidValidatorCount { count: active }.into());
    }
    u8::try_from(active.saturating_mul(2).div_ceil(3))
        .map_err(|_| HyperlaneControllerError::InvalidValidatorCount { count: active }.into())
}

fn same_set(left: &[Address], right: &[Address]) -> bool {
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort_unstable();
    right.sort_unstable();
    left == right
}

/// Shape checks Hyperlane's `setValidatorsAndThreshold` would reject on-chain.
pub fn validate_validators(
    validators: &[Address],
    threshold: u8,
) -> std::result::Result<(), HyperlaneControllerError> {
    if validators.is_empty() || validators.len() > MAX_VALIDATORS {
        return Err(HyperlaneControllerError::InvalidValidatorCount {
            count: validators.len(),
        });
    }
    if threshold == 0 || usize::from(threshold) > validators.len() {
        return Err(HyperlaneControllerError::InvalidThreshold {
            threshold,
            validators: validators.len(),
        });
    }
    for (index, validator) in validators.iter().enumerate() {
        if *validator == Address::ZERO || validators[..index].contains(validator) {
            return Err(HyperlaneControllerError::InvalidValidator {
                validator: *validator,
            });
        }
    }
    Ok(())
}
