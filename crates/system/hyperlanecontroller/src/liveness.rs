//! Liveness: validators prove that their Hyperlane agent keeps signing.

use alloy_primitives::{eip191_hash_message, keccak256, Address, B256};
use outbe_primitives::error::Result;
use outbe_primitives::tee_signatures::recover_signer;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::delegation::ValidatorDelegateRole;

use crate::errors::HyperlaneControllerError;
use crate::precompile::IHyperlaneController;
use crate::runtime::consensus_threshold;
use crate::schema::{validator_domain_key, HyperlaneControllerContract};

/// Submissions younger than this many blocks do not count towards the
/// per-domain reference index. This gives the agent time to sign and upload,
/// and the feeder time to submit, before the controller considers a lagging
/// validator behind.
pub const GRACE_BLOCKS: u64 = 30;
/// The liveness verdict runs when the block number is a multiple of this
/// value. The oracle slash window is a separate genesis parameter.
/// Both checks run in the begin zone. They do not share this cadence.
pub const LIVENESS_WINDOW_BLOCKS: u64 = 150;
/// Consecutive window misses before a validator is jailed.
pub const MAX_MISSES: u32 = 3;

/// A Hyperlane checkpoint and the validator signature over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedCheckpoint<'a> {
    /// Origin domain of the checkpoint.
    pub domain: u32,
    /// Merkle root of the origin mailbox tree.
    pub root: B256,
    /// Index of the latest message in the tree.
    pub index: u32,
    /// Identifier of the latest message.
    pub message_id: B256,
    /// Signature over [`checkpoint_digest`]. It must be 65 bytes.
    pub signature: &'a [u8],
}

/// Reference index of one domain in a liveness window. It is `None` when too
/// few settled submissions exist for that domain.
type DomainReference = (u32, Option<u32>);

/// Standing of one active validator in a liveness window.
enum WindowStanding {
    /// At least one domain had no submission. The window stamps that domain
    /// and evaluates the validator from the next window.
    Newcomer,
    /// The validator is at or above the reference index on every domain.
    OnTime,
    /// The validator is below the reference index on some domain.
    Behind,
}

impl HyperlaneControllerContract<'_> {
    /// Registers the key that the validator of `caller` uses to sign Hyperlane
    /// checkpoints. Zero resets to the validator address.
    pub fn set_hyperlane_signer(&mut self, caller: Address, signer: Address) -> Result<()> {
        let validator = self.validator_of_sender(caller)?;
        let effective = if signer == Address::ZERO {
            validator
        } else {
            signer
        };
        let validator_set = ValidatorSet::new(self.storage.clone());
        for record in validator_set.get_active_validators()? {
            let other = record.validator_address;
            if other == validator {
                continue;
            }
            if other == effective || self.hyperlane_signer(other)? == effective {
                return Err(HyperlaneControllerError::SignerTaken {
                    signer: effective,
                    validator: other,
                }
                .into());
            }
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.signer_of.write(&validator, signer)?;
            self.emit(IHyperlaneController::HyperlaneSignerSet { validator, signer })
        })
    }

    /// Liveness proof for one domain. The caller (validator or its oracle
    /// delegate, i.e. the feeder key) submits its latest signed checkpoint.
    /// The controller verifies the signature against the validator's
    /// Hyperlane signer and records only the index. The controller never
    /// accepts index 0: the first submission must be strictly newer than the
    /// stored zero.
    pub fn submit_checkpoint(
        &mut self,
        caller: Address,
        checkpoint: &SignedCheckpoint<'_>,
    ) -> Result<()> {
        self.require_initialized()?;
        let validator = self.validator_of_sender(caller)?;
        let domain = checkpoint.domain;
        let hook = self.hook_by_domain.read(&domain)?;
        if hook == Address::ZERO {
            return Err(HyperlaneControllerError::UnknownHook { domain }.into());
        }
        let signature: &[u8; 65] = checkpoint.signature.try_into().map_err(|_| {
            HyperlaneControllerError::InvalidSignatureLength {
                length: checkpoint.signature.len(),
            }
        })?;
        let digest = checkpoint_digest(
            domain,
            hook,
            checkpoint.root,
            checkpoint.index,
            checkpoint.message_id,
        );
        let recovered = recover_signer(&digest, signature)?;
        let expected = self.hyperlane_signer(validator)?;
        if recovered != expected {
            return Err(HyperlaneControllerError::SignerMismatch {
                validator,
                expected,
                recovered,
            }
            .into());
        }
        self.record_submission(validator, domain, checkpoint.index)
    }

    /// Records a verified submission. The index must be strictly newer than
    /// the stored index.
    fn record_submission(&mut self, validator: Address, domain: u32, index: u32) -> Result<()> {
        let key = validator_domain_key(validator, domain);
        let submitted = self.submitted_index.read(&key)?;
        if index <= submitted {
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

    /// Liveness-window verdict (every [`LIVENESS_WINDOW_BLOCKS`]). For each
    /// domain, the reference index is the `threshold`-th highest submission
    /// among active validators. Only submissions older than [`GRACE_BLOCKS`]
    /// count. This way, one validator cannot inflate the reference, and the
    /// verdict ignores a checkpoint that everyone still trails. A validator
    /// below the reference on any domain gets a miss. [`MAX_MISSES`]
    /// consecutive misses jail it (no slash). A validator with no submission
    /// yet gets a stamp, and the verdict evaluates it from the next window.
    /// Returns the validators jailed.
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
        let references = self.reference_indexes(&active, threshold, now)?;

        let mut jailed = Vec::new();
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            for validator in &active {
                let standing = self.window_standing(*validator, &references, now)?;
                if self.apply_standing(&mut validator_set, *validator, standing)? {
                    jailed.push(*validator);
                }
            }
            Ok(())
        })?;
        Ok(jailed)
    }

    /// The reference index of every domain: the `threshold`-th highest
    /// settled submission among `active`.
    fn reference_indexes(
        &self,
        active: &[Address],
        threshold: usize,
        now: u64,
    ) -> Result<Vec<DomainReference>> {
        let domains = self.domains.read_all()?;
        let mut references = Vec::with_capacity(domains.len());
        for domain in domains {
            let mut settled = self.settled_indexes(active, domain, now)?;
            settled.sort_unstable_by(|a, b| b.cmp(a));
            references.push((domain, settled.get(threshold - 1).copied()));
        }
        Ok(references)
    }

    /// Submitted indexes on `domain` that are at least [`GRACE_BLOCKS`] old.
    fn settled_indexes(&self, active: &[Address], domain: u32, now: u64) -> Result<Vec<u32>> {
        let mut settled = Vec::with_capacity(active.len());
        for validator in active {
            let key = validator_domain_key(*validator, domain);
            let block = self.submitted_block.read(&key)?;
            if block != 0 && block.saturating_add(GRACE_BLOCKS) <= now {
                settled.push(self.submitted_index.read(&key)?);
            }
        }
        Ok(settled)
    }

    /// Compares `validator` with every domain reference. A domain without a
    /// submission gets the stamp `now`.
    fn window_standing(
        &mut self,
        validator: Address,
        references: &[DomainReference],
        now: u64,
    ) -> Result<WindowStanding> {
        let mut newcomer = false;
        let mut behind = false;
        for (domain, reference) in references {
            let key = validator_domain_key(validator, *domain);
            if self.submitted_block.read(&key)? == 0 {
                self.submitted_block.write(&key, now)?;
                newcomer = true;
                continue;
            }
            if let Some(reference) = reference {
                behind |= self.submitted_index.read(&key)? < *reference;
            }
        }
        Ok(if newcomer {
            WindowStanding::Newcomer
        } else if behind {
            WindowStanding::Behind
        } else {
            WindowStanding::OnTime
        })
    }

    /// Updates the miss count of `validator`. Returns `true` when the
    /// validator reached [`MAX_MISSES`] and was jailed.
    fn apply_standing(
        &mut self,
        validator_set: &mut ValidatorSet<'_>,
        validator: Address,
        standing: WindowStanding,
    ) -> Result<bool> {
        match standing {
            WindowStanding::Newcomer => Ok(false),
            WindowStanding::OnTime => {
                if self.miss_count.read(&validator)? != 0 {
                    self.miss_count.write(&validator, 0)?;
                }
                Ok(false)
            }
            WindowStanding::Behind => self.record_miss(validator_set, validator),
        }
    }

    /// Adds one miss. At [`MAX_MISSES`] the validator is jailed and its miss
    /// count restarts at zero.
    fn record_miss(
        &mut self,
        validator_set: &mut ValidatorSet<'_>,
        validator: Address,
    ) -> Result<bool> {
        let misses = self.miss_count.read(&validator)?.saturating_add(1);
        self.emit(IHyperlaneController::LivenessMiss { validator, misses })?;
        if misses < MAX_MISSES {
            self.miss_count.write(&validator, misses)?;
            return Ok(false);
        }
        validator_set.jail_validator(validator)?;
        self.miss_count.write(&validator, 0)?;
        self.emit(IHyperlaneController::LivenessJailed { validator })?;
        Ok(true)
    }

    /// Hyperlane signers of the active validators, in validator-set order.
    pub(crate) fn active_signers(&self) -> Result<Vec<Address>> {
        let validator_set = ValidatorSet::new(self.storage.clone());
        validator_set
            .get_active_validators()?
            .into_iter()
            .map(|record| self.hyperlane_signer(record.validator_address))
            .collect()
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
