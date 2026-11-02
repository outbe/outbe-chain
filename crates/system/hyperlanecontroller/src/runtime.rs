use alloy_primitives::Address;
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::errors::HyperlaneControllerError;
use crate::calls::{Calls, RemoteCall};
use crate::ism::accept_remote_ism;
use crate::precompile::ICrosschainGov;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IOwnable, IStorageMultisigIsm};

/// Hyperlane's multisig threshold is a `uint8`.
const MAX_VALIDATORS: usize = u8::MAX as usize;

/// The one-shot `initialize` input: the ICA router, the validator announce
/// contract, and the `domain -> (ISM, hook)` table as parallel lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerBootstrap<'a> {
    /// Interchain Account router that the controller must already own.
    pub ica_router: Address,
    /// Hyperlane validator announce contract.
    pub validator_announce: Address,
    /// Domains of the table. The local domain must be one of them.
    pub domains: &'a [u32],
    /// ISM of each domain, in the order of `domains`.
    pub isms: &'a [Address],
    /// Post-dispatch hook of each domain, in the order of `domains`.
    pub hooks: &'a [Address],
}

impl ControllerBootstrap<'_> {
    /// Checks the table shape and the announce address.
    fn check_shape(&self) -> Result<()> {
        if self.domains.len() != self.isms.len() {
            return Err(HyperlaneControllerError::IsmLengthMismatch.into());
        }
        if self.domains.len() != self.hooks.len() {
            return Err(HyperlaneControllerError::HookLengthMismatch.into());
        }
        if self.validator_announce == Address::ZERO {
            return Err(HyperlaneControllerError::InvalidAddress.into());
        }
        Ok(())
    }

    /// The ISM of the `local` domain.
    fn ism_of(&self, local: u32) -> Result<Address> {
        self.domains
            .iter()
            .zip(self.isms)
            .find(|(domain, _)| **domain == local)
            .map(|(_, ism)| *ism)
            .ok_or_else(|| HyperlaneControllerError::LocalDomainMissing { domain: local }.into())
    }
}

impl HyperlaneControllerContract<'_> {
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

    pub fn require_initialized(&self) -> Result<()> {
        if !self.is_initialized()? {
            return Err(HyperlaneControllerError::NotInitialized.into());
        }
        Ok(())
    }

    /// Hyperlane domain of this chain (= chain id).
    pub fn local_domain(&self) -> Result<u32> {
        u32::try_from(self.storage.chain_id()?)
            .map_err(|_| HyperlaneControllerError::InvalidDomain.into())
    }

    pub fn is_initialized(&self) -> Result<bool> {
        Ok(self.ica_router.read()? != Address::ZERO)
    }

    /// One-shot bootstrap:
    /// 1. Accepts the pending ownership of the local ISM.
    /// 2. Verifies that the controller already owns the ICA router.
    /// 3. Stores the router plus the whole `domain -> ISM` table.
    /// 4. Dispatches `acceptOwnership()` to every remote ISM through the
    ///    Interchain Account.
    ///
    /// The controller must have funds first, because each dispatch pays the
    /// IGP quote.
    ///
    /// Only the genesis-seeded bootstrap admin may call it. The admin must
    /// also be the current owner of the local ISM (the deployer that staged
    /// `transferOwnership` to this precompile). The ISM address comes from
    /// calldata, so the ISM owner check alone does not restrict the caller.
    pub fn initialize(
        &mut self,
        caller: Address,
        bootstrap: &ControllerBootstrap<'_>,
    ) -> Result<Calls> {
        if self.is_initialized()? {
            return Err(HyperlaneControllerError::AlreadyInitialized.into());
        }
        // Check the caller before any sub-call to a calldata address.
        let admin = self.bootstrap_admin.read()?;
        if admin == Address::ZERO || caller != admin {
            return Err(HyperlaneControllerError::NotBootstrapAdmin { caller, admin }.into());
        }
        bootstrap.check_shape()?;
        let local = self.local_domain()?;
        let local_ism = bootstrap.ism_of(local)?;
        self.require_ism_handover(local_ism, caller)?;
        self.require_owned(bootstrap.ica_router)?;

        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.install(bootstrap, local, local_ism))
    }

    /// Requires that `caller` owns the local ISM and that the ISM ownership
    /// is pending to this controller.
    fn require_ism_handover(&self, local_ism: Address, caller: Address) -> Result<()> {
        let owner = self.owner_of(local_ism)?;
        if owner != caller {
            return Err(HyperlaneControllerError::NotIsmOwner { caller, owner }.into());
        }
        let pending = self.pending_owner_of(local_ism)?;
        if pending != self.address {
            return Err(HyperlaneControllerError::IsmNotPendingToController { pending }.into());
        }
        Ok(())
    }

    /// Stores the bootstrap table. Returns the calls that take the ownership
    /// of the local ISM and of every remote ISM.
    fn install(
        &mut self,
        bootstrap: &ControllerBootstrap<'_>,
        local: u32,
        local_ism: Address,
    ) -> Result<Calls> {
        let mut calls = Calls {
            local: vec![RemoteCall::new(
                local_ism,
                IStorageMultisigIsm::acceptOwnershipCall {}.abi_encode(),
            )],
            remote: Vec::new(),
        };
        self.ica_router.write(bootstrap.ica_router)?;
        self.validator_announce
            .write(bootstrap.validator_announce)?;
        let table = bootstrap
            .domains
            .iter()
            .zip(bootstrap.isms)
            .zip(bootstrap.hooks);
        for ((domain, ism), hook) in table {
            self.write_domain(*domain, *ism, *hook)?;
            if *domain != local {
                calls.remote.push(accept_remote_ism(*domain, *ism));
            }
        }
        self.emit(ICrosschainGov::Initialized {
            icaRouter: bootstrap.ica_router,
        })?;
        Ok(calls)
    }

    /// Mirrors the active Outbe validator set into every ISM (validators =
    /// the Hyperlane signer of every active validator, threshold =
    /// [`consensus_threshold`]). Returns the rotation calls, or `None` when
    /// the local ISM already matches, so a keeper can call it every epoch.
    pub fn sync(&mut self) -> Result<Option<Calls>> {
        let active = self.active_signers()?;
        let threshold = consensus_threshold(active.len())?;
        let (current, current_threshold) = self.current_validators()?;
        if current_threshold == threshold && same_set(&current, &active) {
            return Ok(None);
        }
        self.set_validators_and_threshold(&active, threshold)
            .map(Some)
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

/// Shape checks for the inputs that Hyperlane's `setValidatorsAndThreshold`
/// would reject on-chain.
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
