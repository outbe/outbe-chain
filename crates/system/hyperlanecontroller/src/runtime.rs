use alloy_primitives::{eip191_hash_message, keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;
use outbe_primitives::tee_signatures::recover_signer;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::delegation::ValidatorDelegateRole;

use crate::errors::HyperlaneControllerError;
use crate::precompile::IHyperlaneController;
use crate::schema::{validator_domain_key, HyperlaneControllerContract};
use crate::sol_ext::{IInterchainAccountRouter, IOwnable, IStorageMultisigIsm};

/// Hyperlane's multisig threshold is a `uint8`.
const MAX_VALIDATORS: usize = u8::MAX as usize;

/// Submissions younger than this many blocks do not count towards the
/// per-domain reference index: time for the agent to sign, upload, and the
/// feeder to submit before a lagging validator is considered behind.
pub const GRACE_BLOCKS: u64 = 30;
/// The liveness verdict runs at every block number that is a multiple of
/// this, the same cadence as the oracle slash window.
pub const LIVENESS_WINDOW_BLOCKS: u64 = 150;
/// Consecutive window misses before a validator is jailed.
pub const MAX_MISSES: u32 = 3;

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
        Ok(self.ica_router.read()? != Address::ZERO)
    }

    // ----------------------------------------------------------------------
    // Direct selectors
    // ----------------------------------------------------------------------

    /// One-shot bootstrap: accepts the pending ownership of the local ISM,
    /// verifies the ICA router is already owned by the controller, and stores
    /// it plus the whole `domain -> ISM` table.
    ///
    /// Only the current owner of the local ISM (the deployer that staged
    /// `transferOwnership` to this precompile) may call it.
    pub fn initialize(
        &mut self,
        caller: Address,
        ica_router: Address,
        domains: &[u32],
        isms: &[Address],
        hooks: &[Address],
    ) -> Result<()> {
        if self.is_initialized()? {
            return Err(HyperlaneControllerError::AlreadyInitialized.into());
        }
        if domains.len() != isms.len() {
            return Err(HyperlaneControllerError::IsmLengthMismatch.into());
        }
        if domains.len() != hooks.len() {
            return Err(HyperlaneControllerError::HookLengthMismatch.into());
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
        self.require_owned(ica_router)?;

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.storage.call(
                local_ism,
                U256::ZERO,
                IStorageMultisigIsm::acceptOwnershipCall {}
                    .abi_encode()
                    .into(),
            )?;
            self.ica_router.write(ica_router)?;
            for ((domain, ism), hook) in domains.iter().zip(isms).zip(hooks) {
                self.write_domain(*domain, *ism, *hook)?;
            }
            self.emit(IHyperlaneController::Initialized {
                icaRouter: ica_router,
            })
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
    pub fn add_domain(&mut self, domain: u32, ism: Address, hook: Address) -> Result<()> {
        self.require_initialized()?;
        self.require_remote_domain(domain)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.write_domain(domain, ism, hook))
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
    // Liveness: validators prove their Hyperlane agent keeps signing
    // ----------------------------------------------------------------------

    /// Registers the key `caller`'s validator signs Hyperlane checkpoints
    /// with. Zero resets to the validator address.
    pub fn set_hyperlane_signer(&mut self, caller: Address, signer: Address) -> Result<()> {
        let validator = self.validator_of_sender(caller)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.signer_of.write(&validator, signer)?;
            self.emit(IHyperlaneController::HyperlaneSignerSet { validator, signer })
        })
    }

    /// Liveness proof for one domain. The caller (validator or its oracle
    /// delegate, i.e. the feeder key) submits its latest signed checkpoint;
    /// the signature is verified against the validator's Hyperlane signer and
    /// only the index is recorded.
    pub fn submit_checkpoint(
        &mut self,
        caller: Address,
        domain: u32,
        root: B256,
        index: u32,
        message_id: B256,
        signature: &[u8],
    ) -> Result<()> {
        self.require_initialized()?;
        let validator = self.validator_of_sender(caller)?;
        let hook = self.hook_by_domain.read(&domain)?;
        if hook == Address::ZERO {
            return Err(HyperlaneControllerError::UnknownHook { domain }.into());
        }
        let signature: &[u8; 65] =
            signature
                .try_into()
                .map_err(|_| HyperlaneControllerError::InvalidSignatureLength {
                    length: signature.len(),
                })?;
        let recovered = recover_signer(
            &checkpoint_digest(domain, hook, root, index, message_id),
            signature,
        )?;
        let expected = self.hyperlane_signer(validator)?;
        if recovered != expected {
            return Err(HyperlaneControllerError::SignerMismatch {
                validator,
                expected,
                recovered,
            }
            .into());
        }
        let key = validator_domain_key(validator, domain);
        let submitted = self.submitted_index.read(&key)?;
        if index < submitted {
            return Err(HyperlaneControllerError::StaleIndex { index, submitted }.into());
        }
        let block = self.storage.block_number()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.submitted_index.write(&key, index)?;
            self.submitted_block.write(&key, block)?;
            self.emit(IHyperlaneController::CheckpointSubmitted {
                validator,
                domain,
                index,
            })
        })
    }

    /// Liveness-window verdict (every [`LIVENESS_WINDOW_BLOCKS`]). Per domain
    /// the reference index is the `threshold`-th highest submission among
    /// active validators, counting only submissions older than
    /// [`GRACE_BLOCKS`]: one validator cannot inflate it, and a checkpoint
    /// everyone is still catching up on is ignored. A validator below the
    /// reference on any domain gets a miss; [`MAX_MISSES`] consecutive misses
    /// jail it (no slash). A validator with no submission yet is stamped and
    /// evaluated from the next window. Returns the validators jailed.
    pub fn check_liveness(&mut self) -> Result<Vec<Address>> {
        if !self.is_initialized()? {
            return Ok(Vec::new());
        }
        let now = self.storage.block_number()?;
        let mut validator_set = ValidatorSet::new(self.storage.clone());
        let active: Vec<Address> = validator_set
            .get_active_validators()?
            .into_iter()
            .map(|record| record.validator_address)
            .collect();
        if active.is_empty() {
            return Ok(Vec::new());
        }
        let threshold = usize::from(consensus_threshold(active.len())?);
        let domains = self.domains.read_all()?;

        let mut references: Vec<(u32, Option<u32>)> = Vec::with_capacity(domains.len());
        for domain in &domains {
            let mut settled = Vec::with_capacity(active.len());
            for validator in &active {
                let key = validator_domain_key(*validator, *domain);
                let block = self.submitted_block.read(&key)?;
                if block != 0 && block.saturating_add(GRACE_BLOCKS) <= now {
                    settled.push(self.submitted_index.read(&key)?);
                }
            }
            settled.sort_unstable_by(|a, b| b.cmp(a));
            references.push((*domain, settled.get(threshold - 1).copied()));
        }

        let mut jailed = Vec::new();
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            for validator in &active {
                let mut newcomer = false;
                let mut behind = false;
                for (domain, reference) in &references {
                    let key = validator_domain_key(*validator, *domain);
                    if self.submitted_block.read(&key)? == 0 {
                        self.submitted_block.write(&key, now)?;
                        newcomer = true;
                        continue;
                    }
                    if let Some(reference) = reference {
                        if self.submitted_index.read(&key)? < *reference {
                            behind = true;
                        }
                    }
                }
                if newcomer {
                    continue;
                }
                if !behind {
                    if self.miss_count.read(validator)? != 0 {
                        self.miss_count.write(validator, 0)?;
                    }
                    continue;
                }
                let misses = self.miss_count.read(validator)?.saturating_add(1);
                self.emit(IHyperlaneController::LivenessMiss {
                    validator: *validator,
                    misses,
                })?;
                if misses < MAX_MISSES {
                    self.miss_count.write(validator, misses)?;
                    continue;
                }
                validator_set.jail_validator(*validator)?;
                self.miss_count.write(validator, 0)?;
                self.emit(IHyperlaneController::LivenessJailed {
                    validator: *validator,
                })?;
                jailed.push(*validator);
            }
            Ok(())
        })?;
        Ok(jailed)
    }

    /// Hyperlane signer of `validator`: the registered key, else the
    /// validator address itself.
    pub fn hyperlane_signer(&self, validator: Address) -> Result<Address> {
        let signer = self.signer_of.read(&validator)?;
        Ok(if signer == Address::ZERO {
            validator
        } else {
            signer
        })
    }

    /// The active validator a transaction sender acts for: the validator
    /// itself or its oracle delegate (the feeder key).
    fn validator_of_sender(&self, sender: Address) -> Result<Address> {
        let validator_set = ValidatorSet::new(self.storage.clone());
        let validator = validator_set
            .resolve_validator_for_role(sender, ValidatorDelegateRole::Oracle)?
            .ok_or(HyperlaneControllerError::NotActiveValidator { caller: sender })?;
        if !validator_set
            .validator_lifecycle(validator)?
            .is_active_status()
        {
            return Err(HyperlaneControllerError::NotActiveValidator { caller: sender }.into());
        }
        Ok(validator)
    }

    // ----------------------------------------------------------------------
    // Internals
    // ----------------------------------------------------------------------

    fn dispatch_remote(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256> {
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

    fn write_domain(&mut self, domain: u32, ism: Address, hook: Address) -> Result<()> {
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

/// The EIP-191 digest a Hyperlane validator signs for a checkpoint
/// (`hyperlane-core`'s `CheckpointWithMessageId::signing_hash`):
/// `domain_hash = keccak(domain_be32 || hook_bytes32 || "HYPERLANE")`,
/// `signing_hash = keccak(domain_hash || root || index_be32 || message_id)`,
/// then `personal_sign` over the 32-byte signing hash.
pub fn checkpoint_digest(
    domain: u32,
    hook: Address,
    root: B256,
    index: u32,
    message_id: B256,
) -> B256 {
    let mut domain_input = Vec::with_capacity(4 + 32 + 9);
    domain_input.extend_from_slice(&domain.to_be_bytes());
    domain_input.extend_from_slice(hook.into_word().as_slice());
    domain_input.extend_from_slice(b"HYPERLANE");
    let domain_hash = keccak256(&domain_input);

    let mut signing_input = Vec::with_capacity(32 + 32 + 4 + 32);
    signing_input.extend_from_slice(domain_hash.as_slice());
    signing_input.extend_from_slice(root.as_slice());
    signing_input.extend_from_slice(&index.to_be_bytes());
    signing_input.extend_from_slice(message_id.as_slice());
    eip191_hash_message(keccak256(&signing_input))
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
