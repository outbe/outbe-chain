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

/// Submissions younger than this many blocks do not count towards the next
/// per-domain target index. This way, the target is an index that most
/// validators already signed when the window starts.
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

/// Target of one domain in a liveness window: the domain, its target index,
/// and the anchor root. The anchor is the root that at least `threshold`
/// active validators signed at the target index. It is `None` when no root
/// has that many votes.
type DomainAnchor = (u32, u32, Option<B256>);

/// Standing of one active validator in a liveness window.
enum WindowStanding {
    /// At least one domain had no submission. The window stamps that domain
    /// and evaluates the validator from the next window.
    Newcomer,
    /// The vote of the validator equals the anchor root on every domain
    /// that has an anchor.
    OnTime,
    /// On some domain with an anchor, the validator has no vote at the
    /// target index, or its root is not the anchor root.
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

    /// Liveness proof for one domain. The validator or its Hyperlane delegate
    /// submits a signed checkpoint.
    /// The controller verifies the signature against the validator's
    /// Hyperlane signer. Then it does one or both of these:
    /// - The index equals the target index of the domain: the root becomes
    ///   the vote of the validator. A different root replaces an earlier vote.
    /// - The index is strictly newer than the stored index: it becomes the
    ///   latest index of the validator.
    ///
    /// When neither applies, the call fails with `StaleIndex`. The
    /// controller never accepts index 0: zero means "no target" and "no
    /// submission".
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
        self.record_submission(validator, domain, checkpoint.index, checkpoint.root)
    }

    /// Records a verified submission as a vote, as the latest index, or as
    /// both. See [`Self::submit_checkpoint`].
    pub(crate) fn record_submission(
        &mut self,
        validator: Address,
        domain: u32,
        index: u32,
        root: B256,
    ) -> Result<()> {
        let key = validator_domain_key(validator, domain);
        let submitted = self.submitted_index.read(&key)?;
        let target = self.target_index.read(&domain)?;
        let is_latest = index > submitted;
        // A repeat of the stored vote changes nothing, so it is not a vote.
        let is_vote = target != 0
            && index == target
            && self.vote_at(validator, domain, target)? != Some(root);
        if !is_latest && !is_vote {
            return Err(HyperlaneControllerError::StaleIndex { index, submitted }.into());
        }
        let block = self.storage.block_number()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            if is_vote {
                self.vote_index.write(&key, index)?;
                self.vote_root.write(&key, root)?;
                self.emit(IHyperlaneController::TargetCheckpointSubmitted {
                    validator,
                    domain,
                    index,
                    root,
                })?;
            }
            if is_latest {
                self.submitted_index.write(&key, index)?;
                self.submitted_block.write(&key, block)?;
                self.emit(IHyperlaneController::CheckpointSubmitted {
                    validator,
                    domain,
                    index,
                })?;
            }
            Ok(())
        })
    }

    /// The root that `validator` signed at the index `target` of `domain`.
    /// `None` when its last vote is for another index. `target` must not be 0.
    fn vote_at(&self, validator: Address, domain: u32, target: u32) -> Result<Option<B256>> {
        let key = validator_domain_key(validator, domain);
        if self.vote_index.read(&key)? != target {
            return Ok(None);
        }
        Ok(Some(self.vote_root.read(&key)?))
    }

    /// Liveness-window verdict (every [`LIVENESS_WINDOW_BLOCKS`]).
    ///
    /// Each domain has a target index. In the window, every validator signs
    /// the checkpoint at that index. The anchor is the root that at least
    /// `threshold` active validators signed. A validator with no vote at the
    /// target, or with a root that is not the anchor, gets a miss.
    /// [`MAX_MISSES`] consecutive misses jail it (no slash). A domain with no
    /// target or no anchor gives no miss. A validator with no submission yet
    /// gets a stamp, and the verdict evaluates it from the next window.
    ///
    /// Then the verdict sets the target of the next window: the
    /// `threshold`-th highest latest index among active validators. Only
    /// submissions older than [`GRACE_BLOCKS`] count. This way, one validator
    /// cannot inflate the target. The target never decreases.
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
        let anchors = self.anchors(&active, threshold)?;
        let references = self.reference_indexes(&active, threshold, now)?;

        let mut jailed = Vec::new();
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            for validator in &active {
                let standing = self.window_standing(*validator, &anchors, now)?;
                if self.apply_standing(&mut validator_set, *validator, standing)? {
                    jailed.push(*validator);
                }
            }
            self.advance_targets(&references)
        })?;
        Ok(jailed)
    }

    /// The target index and the anchor root of every domain.
    fn anchors(&self, active: &[Address], threshold: usize) -> Result<Vec<DomainAnchor>> {
        let domains = self.domains.read_all()?;
        let mut anchors = Vec::with_capacity(domains.len());
        for domain in domains {
            let target = self.target_index.read(&domain)?;
            let mut roots = Vec::with_capacity(active.len());
            if target != 0 {
                for validator in active {
                    roots.extend(self.vote_at(*validator, domain, target)?);
                }
            }
            // `threshold` is above half of the active set, so at most one
            // root can reach it.
            let anchor = roots
                .iter()
                .find(|root| roots.iter().filter(|other| other == root).count() >= threshold)
                .copied();
            anchors.push((domain, target, anchor));
        }
        Ok(anchors)
    }

    /// Moves the target of each domain up to its reference index.
    fn advance_targets(&mut self, references: &[DomainReference]) -> Result<()> {
        for (domain, reference) in references {
            let Some(index) = *reference else { continue };
            if index > self.target_index.read(domain)? {
                self.target_index.write(domain, index)?;
                self.emit(IHyperlaneController::TargetIndexSet {
                    domain: *domain,
                    index,
                })?;
            }
        }
        Ok(())
    }

    /// Clears the liveness state of `domain`: the target, and the votes and
    /// submissions of the active validators.
    pub(crate) fn clear_liveness(&mut self, domain: u32) -> Result<()> {
        self.target_index.clear(&domain)?;
        let validator_set = ValidatorSet::new(self.storage.clone());
        for record in validator_set.get_active_validators()? {
            let key = validator_domain_key(record.validator_address, domain);
            self.submitted_index.clear(&key)?;
            self.submitted_block.clear(&key)?;
            self.vote_index.clear(&key)?;
            self.vote_root.clear(&key)?;
        }
        Ok(())
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

    /// Compares the vote of `validator` with every domain anchor. A domain
    /// without a submission gets the stamp `now`.
    fn window_standing(
        &mut self,
        validator: Address,
        anchors: &[DomainAnchor],
        now: u64,
    ) -> Result<WindowStanding> {
        let mut newcomer = false;
        let mut behind = false;
        for (domain, target, anchor) in anchors {
            let key = validator_domain_key(validator, *domain);
            if self.submitted_block.read(&key)? == 0 {
                self.submitted_block.write(&key, now)?;
                newcomer = true;
                continue;
            }
            if anchor.is_some() {
                behind |= self.vote_at(validator, *domain, *target)? != *anchor;
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
    /// itself or its Hyperlane delegate.
    fn validator_of_sender(&self, sender: Address) -> Result<Address> {
        let validator_set = ValidatorSet::new(self.storage.clone());
        let validator = validator_set
            .resolve_validator_for_role(sender, ValidatorDelegateRole::Hyperlane)?
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
