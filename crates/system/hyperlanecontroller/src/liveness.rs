//! Liveness: validators prove that their Hyperlane agent keeps signing.

use alloy_primitives::{keccak256, Address, B256};
use outbe_primitives::error::Result;
use outbe_validatorset::contract::ValidatorSet;

use crate::errors::HyperlaneControllerError;
use crate::precompile::IHyperlaneController;
use crate::runtime::consensus_threshold;
use crate::schema::{
    complaint_key, complaints_key, validator_domain_key, Fault, HyperlaneControllerContract,
};

/// Consecutive liveness misses before a validator is jailed.
pub const MAX_MISSES: u32 = 3;

/// Why a validator gets a liveness miss. The `reasons` field of the
/// `LivenessMiss` event is the sum of these values.
pub mod miss_reason {
    /// No vote at the target index, or a root that is not the quorum root.
    pub const ROOT: u8 = 1;
    /// A quorum named the validator in `notServing`.
    pub const NOT_SERVING: u8 = 2;
    /// A quorum named the validator in `notDelivered`.
    pub const NOT_DELIVERED: u8 = 4;
    /// The report of the validator did not name a validator that a quorum
    /// named in `notServing`.
    pub const NO_COMPLAINT: u8 = 8;
}

/// The most addresses that the two lists of one report can hold together.
pub const MAX_REPORT_ADDRESSES: usize = 32;

/// What a validator saw for one domain in the current liveness round. The
/// controller does not verify these values. It uses a value only when a
/// quorum of the active validators gives it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report<'a> {
    /// The latest checkpoint index that the validator signed. Zero = none.
    pub latest_index: u32,
    /// Validators that do not serve their signed checkpoint at the target.
    pub not_serving: &'a [Address],
    /// Validators that did not deliver their message in their turn.
    pub not_delivered: &'a [Address],
}

/// Reference index of one domain in a liveness round. It is `None` when too
/// few submissions exist for that domain.
type DomainReference = (u32, Option<u32>);

/// Target of one domain in a liveness round: the domain, its target index,
/// and the anchor root. The anchor is the root that at least `threshold`
/// active validators signed at the target index. It is `None` when no root
/// has that many votes.
type DomainAnchor = (u32, u32, Option<B256>);

/// Standing of one active validator in a liveness round.
enum WindowStanding {
    /// At least one domain had no submission. The verdict stamps that domain
    /// and evaluates the validator from the next round.
    Newcomer,
    /// The vote of the validator equals the anchor root on every domain
    /// that has an anchor.
    OnTime,
    /// On some domain with an anchor, the validator has no vote at the
    /// target index, or its root is not the anchor root.
    Behind,
}

impl HyperlaneControllerContract<'_> {
    /// The report of a validator for one domain. The validator or its
    /// Hyperlane delegate sends it. The controller does not verify `root`: a
    /// root counts only when a quorum gives it. The call does one or both of
    /// these:
    /// - The index equals the target index of the domain: the root becomes
    ///   the vote of the validator. A different root replaces an earlier vote.
    /// - The index, or the latest index of the report, is strictly newer
    ///   than the stored index: the higher value becomes the latest index of
    ///   the validator.
    ///
    /// When neither applies, the call fails with `StaleIndex`. The first
    /// accepted call of the validator for the domain in a round also gives
    /// its lists: each name is one complaint about that validator. The
    /// controller never accepts index 0: zero means "no target" and "no
    /// submission".
    pub fn submit_report(
        &mut self,
        caller: Address,
        domain: u32,
        index: u32,
        root: B256,
        report: &Report<'_>,
    ) -> Result<()> {
        self.require_initialized()?;
        let validator = self.validator_of_sender(caller)?;
        if self.hook_by_domain.read(&domain)? == Address::ZERO {
            return Err(HyperlaneControllerError::UnknownHook { domain }.into());
        }
        let named = report.not_serving.len() + report.not_delivered.len();
        if named > MAX_REPORT_ADDRESSES {
            return Err(HyperlaneControllerError::ReportTooLong { length: named }.into());
        }
        self.record_submission(validator, domain, index, root, report)
    }

    /// The current liveness round. It starts at 1.
    pub fn current_round(&self) -> Result<u64> {
        Ok(self.liveness_round.read()?.saturating_add(1))
    }

    /// Adds the complaint of `reporter` about each validator of `accused`. A
    /// second complaint of the same reporter about the same validator in one
    /// round does not count.
    fn record_complaints(
        &mut self,
        reporter: Address,
        domain: u32,
        fault: Fault,
        accused: &[Address],
        round: u64,
    ) -> Result<()> {
        for validator in accused {
            let complaint = complaint_key(reporter, *validator, domain, fault);
            if self.complaint_round.read(&complaint)? == round {
                continue;
            }
            self.complaint_round.write(&complaint, round)?;
            let key = complaints_key(*validator, domain, fault);
            let count = self.complaints_at(&key, round)?;
            self.complaints
                .write(&key, round << 16 | u64::from(count.saturating_add(1)))?;
        }
        Ok(())
    }

    /// The number of complaints under `key` that were made in `round`.
    pub(crate) fn complaints_at(&self, key: &B256, round: u64) -> Result<u16> {
        let packed = self.complaints.read(key)?;
        Ok(if packed >> 16 == round {
            packed as u16
        } else {
            0
        })
    }

    /// Records a submission as a vote, as the latest index, or as
    /// both. The first one of a round also records the report. See
    /// [`Self::submit_report`].
    pub(crate) fn record_submission(
        &mut self,
        validator: Address,
        domain: u32,
        index: u32,
        root: B256,
        report: &Report<'_>,
    ) -> Result<()> {
        let key = validator_domain_key(validator, domain);
        let submitted = self.submitted_index.read(&key)?;
        let target = self.target_index.read(&domain)?;
        let round = self.current_round()?;
        let is_report = self.report_round.read(&key)? != round;
        let latest = index.max(report.latest_index);
        let is_latest = latest > submitted;
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
                self.submitted_index.write(&key, latest)?;
                self.submitted_block.write(&key, block)?;
                self.emit(IHyperlaneController::CheckpointSubmitted {
                    validator,
                    domain,
                    index: latest,
                })?;
            }
            if is_report {
                self.report_round.write(&key, round)?;
                self.record_complaints(
                    validator,
                    domain,
                    Fault::NotServing,
                    report.not_serving,
                    round,
                )?;
                self.record_complaints(
                    validator,
                    domain,
                    Fault::NotDelivered,
                    report.not_delivered,
                    round,
                )?;
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

    /// Liveness verdict. It runs in the first block of each epoch.
    ///
    /// Each domain has a target index. In the round, every validator signs
    /// the checkpoint at that index. The anchor is the root that at least
    /// `threshold` active validators signed. A validator gets a miss for each
    /// of these reasons (see [`miss_reason`]):
    /// - It has no vote at the target, or its root is not the anchor.
    /// - At least `threshold` active validators named it in `notServing` or
    ///   in `notDelivered` of one domain.
    /// - It gave a report for a domain and did not name a validator that at
    ///   least `threshold` validators named in `notServing` of that domain.
    ///   `notDelivered` has no such rule: the delivery check uses values of
    ///   the agent configuration, so two honest agents can get different
    ///   results.
    ///
    /// A validator gets one miss in a round at most. [`MAX_MISSES`]
    /// consecutive misses jail it (no slash). A domain with no target or no
    /// anchor gives no root miss. A validator with no submission yet gets a
    /// stamp, and the verdict evaluates it from the next round.
    ///
    /// Then the verdict sets the target of the next round: the
    /// `threshold`-th highest latest index among active validators. This way,
    /// one validator cannot inflate the target, and a quorum already signed
    /// the target when the round starts. The target never decreases. The verdict
    /// also selects the delivery index of each domain from the indexes that
    /// the target moved over.
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
        let references = self.reference_indexes(&active, threshold)?;
        let complaints = self.complaint_reasons(&active, threshold)?;

        let mut jailed = Vec::new();
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            for (validator, complained) in active.iter().zip(&complaints) {
                let reasons = match self.window_standing(*validator, &anchors, now)? {
                    WindowStanding::Newcomer => continue,
                    WindowStanding::OnTime => *complained,
                    WindowStanding::Behind => miss_reason::ROOT | *complained,
                };
                if self.apply_reasons(&mut validator_set, *validator, reasons)? {
                    jailed.push(*validator);
                }
            }
            self.advance_targets(&references)?;
            self.liveness_round
                .write(self.liveness_round.read()?.saturating_add(1))
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

    /// The miss reasons that come from the reports of the current round, in
    /// the order of `active`.
    fn complaint_reasons(&self, active: &[Address], threshold: usize) -> Result<Vec<u8>> {
        let round = self.current_round()?;
        let mut reasons = vec![0u8; active.len()];
        for domain in self.domains.read_all()? {
            for (fault, reason) in [
                (Fault::NotServing, miss_reason::NOT_SERVING),
                (Fault::NotDelivered, miss_reason::NOT_DELIVERED),
            ] {
                for (i, accused) in active.iter().enumerate() {
                    let key = complaints_key(*accused, domain, fault);
                    if usize::from(self.complaints_at(&key, round)?) < threshold {
                        continue;
                    }
                    reasons[i] |= reason;
                    if fault != Fault::NotServing {
                        continue;
                    }
                    // A validator that gave a report for this domain and did
                    // not name the accused validator did not do the check.
                    for (j, reporter) in active.iter().enumerate() {
                        let reported = validator_domain_key(*reporter, domain);
                        let complaint = complaint_key(*reporter, *accused, domain, fault);
                        if j != i
                            && self.report_round.read(&reported)? == round
                            && self.complaint_round.read(&complaint)? != round
                        {
                            reasons[j] |= miss_reason::NO_COMPLAINT;
                        }
                    }
                }
            }
        }
        Ok(reasons)
    }

    /// Moves the target of each domain up to its reference index. Then it
    /// selects the delivery index of the domain: one of the indexes that the
    /// target moved over. The block number and the block time of the verdict
    /// are in the seed, so a validator cannot know the index when it
    /// delivers the message. When the target does not move, there is no
    /// delivery index.
    fn advance_targets(&mut self, references: &[DomainReference]) -> Result<()> {
        for (domain, reference) in references {
            let old = self.target_index.read(domain)?;
            let delivery = match reference.filter(|index| *index > old) {
                Some(index) => {
                    self.target_index.write(domain, index)?;
                    self.emit(IHyperlaneController::TargetIndexSet {
                        domain: *domain,
                        index,
                    })?;
                    let seed = self.delivery_seed(*domain, index)?;
                    // The result is below `index - old`, so it fits `u32`.
                    old + 1 + (seed % u64::from(index - old)) as u32
                }
                None => 0,
            };
            if self.delivery_index.read(domain)? != delivery {
                self.delivery_index.write(domain, delivery)?;
                self.emit(IHyperlaneController::DeliveryIndexSet {
                    domain: *domain,
                    index: delivery,
                })?;
            }
        }
        Ok(())
    }

    /// The seed of the delivery index of `domain` for the new `target`.
    fn delivery_seed(&self, domain: u32, target: u32) -> Result<u64> {
        let mut bytes = [0u8; 48];
        bytes[..8].copy_from_slice(&self.storage.block_number()?.to_be_bytes());
        bytes[8..40].copy_from_slice(&self.storage.timestamp()?.to_be_bytes::<32>());
        bytes[40..44].copy_from_slice(&domain.to_be_bytes());
        bytes[44..].copy_from_slice(&target.to_be_bytes());
        let hash = keccak256(bytes);
        let mut seed = [0u8; 8];
        seed.copy_from_slice(&hash[24..]);
        Ok(u64::from_be_bytes(seed))
    }

    /// Clears the liveness state of `domain`: the target, and the votes and
    /// submissions of the active validators.
    pub(crate) fn clear_liveness(&mut self, domain: u32) -> Result<()> {
        self.target_index.clear(&domain)?;
        self.delivery_index.clear(&domain)?;
        let validator_set = ValidatorSet::new(self.storage.clone());
        for record in validator_set.get_active_validators()? {
            let key = validator_domain_key(record.validator_address, domain);
            self.submitted_index.clear(&key)?;
            self.submitted_block.clear(&key)?;
            self.vote_index.clear(&key)?;
            self.vote_root.clear(&key)?;
            self.report_round.clear(&key)?;
        }
        Ok(())
    }

    /// The reference index of every domain: the `threshold`-th highest
    /// latest index among `active`.
    fn reference_indexes(
        &self,
        active: &[Address],
        threshold: usize,
    ) -> Result<Vec<DomainReference>> {
        let domains = self.domains.read_all()?;
        let mut references = Vec::with_capacity(domains.len());
        for domain in domains {
            let mut latest = self.latest_indexes(active, domain)?;
            latest.sort_unstable_by(|a, b| b.cmp(a));
            references.push((domain, latest.get(threshold - 1).copied()));
        }
        Ok(references)
    }

    /// The latest index on `domain` of each validator of `active` that the
    /// verdict already evaluates.
    fn latest_indexes(&self, active: &[Address], domain: u32) -> Result<Vec<u32>> {
        let mut latest = Vec::with_capacity(active.len());
        for validator in active {
            let key = validator_domain_key(*validator, domain);
            if self.submitted_block.read(&key)? != 0 {
                latest.push(self.submitted_index.read(&key)?);
            }
        }
        Ok(latest)
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

    /// Updates the miss count of `validator`: no reason clears it, a reason
    /// adds one miss. Returns `true` when the validator reached
    /// [`MAX_MISSES`] and was jailed.
    fn apply_reasons(
        &mut self,
        validator_set: &mut ValidatorSet<'_>,
        validator: Address,
        reasons: u8,
    ) -> Result<bool> {
        if reasons != 0 {
            return self.record_miss(validator_set, validator, reasons);
        }
        if self.miss_count.read(&validator)? != 0 {
            self.miss_count.write(&validator, 0)?;
        }
        Ok(false)
    }

    /// Adds one miss. At [`MAX_MISSES`] the validator is jailed and its miss
    /// count restarts at zero.
    fn record_miss(
        &mut self,
        validator_set: &mut ValidatorSet<'_>,
        validator: Address,
        reasons: u8,
    ) -> Result<bool> {
        let misses = self.miss_count.read(&validator)?.saturating_add(1);
        self.emit(IHyperlaneController::LivenessMiss {
            validator,
            misses,
            reasons,
        })?;
        if misses < MAX_MISSES {
            self.miss_count.write(&validator, misses)?;
            return Ok(false);
        }
        validator_set.jail_validator(validator)?;
        self.miss_count.write(&validator, 0)?;
        self.emit(IHyperlaneController::LivenessJailed { validator })?;
        Ok(true)
    }
}
