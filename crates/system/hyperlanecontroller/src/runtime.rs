use alloy_primitives::{Address, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::abi::ICrosschainGov;
use crate::domains::accept_remote_ism;
use crate::errors::HyperlaneControllerError;
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::IStorageMultisigIsm;
use crate::Owner;

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
    /// One-shot bootstrap:
    /// 1. Accepts the pending ownership of the local ISM.
    /// 2. Verifies that `0x…EE14` already owns the ICA router.
    /// 3. Stores the router plus the whole `domain -> ISM` table.
    /// 4. Dispatches `acceptOwnership()` to every remote ISM through the
    ///    Interchain Account.
    ///
    /// The owner must have funds first, because each dispatch pays the IGP
    /// quote. Every ISM call goes through `owner`.
    ///
    /// Only the genesis-seeded bootstrap admin may call it. The admin must
    /// also be the current owner of the local ISM (the deployer that staged
    /// `transferOwnership` to this precompile). The ISM address comes from
    /// calldata, so the ISM owner check alone does not restrict the caller.
    pub fn initialize(
        &mut self,
        caller: Address,
        bootstrap: &ControllerBootstrap<'_>,
        owner: &mut impl Owner,
    ) -> Result<()> {
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
        storage.with_checkpoint(|| self.install(bootstrap, (local, local_ism), owner))
    }

    /// Requires that `caller` owns the local ISM and that the ISM ownership
    /// is pending to `0x…EE14`.
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

    /// Takes ownership of the local ISM, then stores the bootstrap table and
    /// takes ownership of every remote ISM.
    fn install(
        &mut self,
        bootstrap: &ControllerBootstrap<'_>,
        (local, local_ism): (u32, Address),
        owner: &mut impl Owner,
    ) -> Result<()> {
        owner.call_local(
            local_ism,
            U256::ZERO,
            IStorageMultisigIsm::acceptOwnershipCall {}
                .abi_encode()
                .into(),
        )?;
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
                accept_remote_ism(owner, *domain, *ism)?;
            }
        }
        self.emit(ICrosschainGov::Initialized {
            icaRouter: bootstrap.ica_router,
        })
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
