use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_consensus::proof::{invalid_vrf_evidence_hash_v2, verify_v2_proof, V2VerifyError};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::protocol_schedule::OutbeProtocolSchedule;
use outbe_primitives::slashing_journal::{iso8601_now, record as journal_record, JournalRecord};
use outbe_primitives::system_tx::{recover_phase1_proposer, SystemTxInputV2};
use outbe_staking::contract::Staking;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::runtime::status as validator_status;
use outbe_validatorset::state::{committee_snapshot_key, read_committee_snapshot};
use tracing::{info, warn};

use crate::evidence::EvidenceBlock;
use crate::vrf_evidence::InvalidVrfProofEvidence;

use crate::schema::SlashIndicator;

use crate::precompile::ISlashIndicator;

/// Default config values used when the stored value is zero (uninitialized).
// Felony thresholds are the maximum validator misses TOLERATED within one epoch.
// The per-epoch reset (`reset_epoch_counters`, run at the epoch boundary) zeroes
// the miss counters, so a validator that crosses the threshold inside an epoch is
// force-exited + slashed immediately; otherwise its count resets next epoch. The
// epoch (`config_epoch_length_blocks`) is the ~1-hour window that also drives DKG
// reshare / active-set rotation / counter reset, so a felony threshold MUST stay
// below the epoch length, else the reset wipes the counter before it can trigger
// (prod epoch 1200 ≈ 1h at ~3s; dev/localnet seeds a smaller threshold for its
// short epoch via genesis — see `scripts/bootstrap-testnet.sh`). A voter miss
// accrues ~1 per finalized block; a proposer miss only on the validator's own
// leader slots (~1/N). Both are genesis-overridable per network
// (`config_*_felony_threshold` slots).
const DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD: u64 = 50;
const DEFAULT_PROPOSER_FELONY_THRESHOLD: u64 = 150;
const DEFAULT_VOTER_MISDEMEANOR_THRESHOLD: u64 = 500;
const DEFAULT_VOTER_FELONY_THRESHOLD: u64 = 150;
const DEFAULT_SLASH_AMOUNT_PERCENT: u64 = 5;
const DEFAULT_EVIDENCE_REWARD_PERCENT: u64 = 10;

impl SlashIndicator<'_> {
    // --- Config helpers ---

    fn proposer_felony_threshold(&self) -> Result<u64> {
        let v = self.config_proposer_felony_threshold.read()?;
        Ok(if v == 0 {
            DEFAULT_PROPOSER_FELONY_THRESHOLD
        } else {
            v
        })
    }

    fn proposer_misdemeanor_threshold(&self) -> Result<u64> {
        let v = self.config_proposer_misdemeanor_threshold.read()?;
        Ok(if v == 0 {
            DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD
        } else {
            v
        })
    }

    fn voter_misdemeanor_threshold(&self) -> Result<u64> {
        let v = self.config_voter_misdemeanor_threshold.read()?;
        Ok(if v == 0 {
            DEFAULT_VOTER_MISDEMEANOR_THRESHOLD
        } else {
            v
        })
    }

    fn voter_felony_threshold(&self) -> Result<u64> {
        let v = self.config_voter_felony_threshold.read()?;
        Ok(if v == 0 {
            DEFAULT_VOTER_FELONY_THRESHOLD
        } else {
            v
        })
    }

    fn slash_amount_percent(&self) -> Result<u64> {
        let v = self.config_slash_amount_percent.read()?;
        Ok(if v == 0 {
            DEFAULT_SLASH_AMOUNT_PERCENT
        } else {
            v
        })
    }

    fn evidence_reward_percent(&self) -> Result<u64> {
        let v = self.config_evidence_reward_percent.read()?;
        Ok(if v == 0 {
            DEFAULT_EVIDENCE_REWARD_PERCENT
        } else {
            v
        })
    }

    // --- Slash actions ---

    /// Records a proposer miss for `validator`.
    ///
    /// - Increments proposer_miss_count[validator].
    /// - At multiples of felony_threshold: forces exit and slashes the validator.
    /// - At multiples of misdemeanor_threshold (non-felony): misdemeanor logged only.
    pub fn slash_proposer(&mut self, validator: Address) -> Result<()> {
        let count = self.proposer_miss_count.read(&validator)? + 1;
        self.proposer_miss_count.write(&validator, count)?;

        let felony_threshold = self.proposer_felony_threshold()?;
        let misdemeanor_threshold = self.proposer_misdemeanor_threshold()?;
        let block_number = self.storage.block_number().unwrap_or(0);

        crate::metrics::record_proposer_miss_count(validator, count);
        crate::metrics::record_proposer_miss_event(validator);

        journal_record(JournalRecord::ProposerMiss {
            wall_clock: iso8601_now(),
            block_number,
            validator: format!("{validator:?}"),
            count,
            felony_threshold,
            misdemeanor_threshold,
        });

        info!(
            target: "outbe::slashing",
            event = "proposer_miss",
            %validator,
            count,
            felony_threshold,
            misdemeanor_threshold,
            block_number,
            "proposer miss recorded",
        );

        if count > 0 && count % felony_threshold == 0 {
            // Felony: increment cumulative counter, force exit and slash.
            let fc = self.felony_count.read(&validator)? + 1;
            self.felony_count.write(&validator, fc)?;

            // Felony: JAIL (not force-exit) + slash. Jail BEFORE slash_stake —
            // slash_stake demotes ACTIVE/PENDING below min_stake but leaves a
            // JAILED status untouched, so this ordering preserves JAILED.
            let mut vs = ValidatorSet::new(self.storage.clone());
            vs.jail_validator(validator)?;

            let slash_percent = self.slash_amount_percent()?;
            let mut staking = Staking::new(self.storage.clone());
            staking.slash_stake(validator, slash_percent)?;

            crate::metrics::record_felony_count(validator, fc);
            crate::metrics::record_validator_slashed(validator, "proposer_felony");

            journal_record(JournalRecord::ProposerFelony {
                wall_clock: iso8601_now(),
                block_number,
                validator: format!("{validator:?}"),
                miss_count: count,
                felony_threshold,
                felony_count: fc,
                slash_percent,
            });

            warn!(
                target: "outbe::slashing",
                event = "proposer_felony",
                %validator,
                miss_count = count,
                felony_threshold,
                felony_count = fc,
                slash_percent,
                block_number,
                "proposer felony — validator force-exited and slashed",
            );

            self.emit(ISlashIndicator::ProposerFelony {
                validator,
                missCount: count,
                felonyCount: fc,
            })?;
        } else if count > 0 && count % misdemeanor_threshold == 0 {
            journal_record(JournalRecord::ProposerMisdemeanor {
                wall_clock: iso8601_now(),
                block_number,
                validator: format!("{validator:?}"),
                miss_count: count,
                misdemeanor_threshold,
            });
            info!(
                target: "outbe::slashing",
                event = "proposer_misdemeanor",
                %validator,
                miss_count = count,
                misdemeanor_threshold,
                block_number,
                "proposer misdemeanor threshold crossed",
            );
            self.emit(ISlashIndicator::ProposerMisdemeanor {
                validator,
                missCount: count,
            })?;
        }

        Ok(())
    }

    /// Records a voter miss for `validator`.
    ///
    /// - Increments voter_miss_count[validator].
    /// - At multiples of voter_felony_threshold: forces exit and slashes the validator.
    /// - At multiples of voter_misdemeanor_threshold (non-felony): misdemeanor logged only.
    pub fn slash_voter(&mut self, validator: Address) -> Result<()> {
        let count = self.voter_miss_count.read(&validator)? + 1;
        self.voter_miss_count.write(&validator, count)?;

        let felony_threshold = self.voter_felony_threshold()?;
        let misdemeanor_threshold = self.voter_misdemeanor_threshold()?;
        let block_number = self.storage.block_number().unwrap_or(0);

        crate::metrics::record_voter_miss_count(validator, count);
        crate::metrics::record_voter_miss_event(validator);

        journal_record(JournalRecord::VoterMiss {
            wall_clock: iso8601_now(),
            block_number,
            validator: format!("{validator:?}"),
            count,
            misdemeanor_threshold,
        });

        info!(
            target: "outbe::slashing",
            event = "voter_miss",
            %validator,
            count,
            felony_threshold,
            misdemeanor_threshold,
            block_number,
            "voter miss recorded",
        );

        if count > 0 && count % felony_threshold == 0 {
            // Felony: increment cumulative counter, force exit and slash. Mirrors
            // the proposer-felony path so missed finalize votes are punitive once
            // they cross the configured threshold (vote_ext.md E8 graduated to T1).
            let fc = self.felony_count.read(&validator)? + 1;
            self.felony_count.write(&validator, fc)?;

            // Felony: JAIL (not force-exit) + slash. Jail BEFORE slash_stake —
            // slash_stake demotes ACTIVE/PENDING below min_stake but leaves a
            // JAILED status untouched, so this ordering preserves JAILED.
            let mut vs = ValidatorSet::new(self.storage.clone());
            vs.jail_validator(validator)?;

            let slash_percent = self.slash_amount_percent()?;
            let mut staking = Staking::new(self.storage.clone());
            staking.slash_stake(validator, slash_percent)?;

            crate::metrics::record_felony_count(validator, fc);
            crate::metrics::record_validator_slashed(validator, "voter_felony");

            journal_record(JournalRecord::VoterFelony {
                wall_clock: iso8601_now(),
                block_number,
                validator: format!("{validator:?}"),
                miss_count: count,
                felony_threshold,
                felony_count: fc,
                slash_percent,
            });

            warn!(
                target: "outbe::slashing",
                event = "voter_felony",
                %validator,
                miss_count = count,
                felony_threshold,
                felony_count = fc,
                slash_percent,
                block_number,
                "voter felony — validator force-exited and slashed",
            );

            self.emit(ISlashIndicator::VoterFelony {
                validator,
                missCount: count,
                felonyCount: fc,
            })?;
        } else if count > 0 && count % misdemeanor_threshold == 0 {
            journal_record(JournalRecord::VoterMisdemeanor {
                wall_clock: iso8601_now(),
                block_number,
                validator: format!("{validator:?}"),
                miss_count: count,
                misdemeanor_threshold,
            });
            info!(
                target: "outbe::slashing",
                event = "voter_misdemeanor",
                %validator,
                miss_count = count,
                misdemeanor_threshold,
                block_number,
                "voter misdemeanor threshold crossed",
            );
            self.emit(ISlashIndicator::VoterMisdemeanor {
                validator,
                missCount: count,
            })?;
        }

        Ok(())
    }

    /// Submits double-proposal evidence.
    ///
    /// Each evidence block is encoded as:
    ///   `pubkey[48] || signature[96] || proposal_encoded[variable]`
    ///
    /// Verification:
    /// 1. Both blocks must have the same BLS MinPk signer public key.
    /// 2. Both proposals must be for the same round (epoch + view).
    /// 3. The proposal bytes must differ (two different proposals for the same round).
    /// 4. Both BLS signatures must be valid (signed over the Simplex notarize payload).
    /// 5. The signer must be a registered validator.
    ///
    /// On success: the validator is forced out and slashed (felony), and the
    /// evidence submitter receives a reward (evidence_reward_percent of slashed amount).
    pub fn submit_double_proposal_evidence(
        &mut self,
        caller: Address,
        block1: &[u8],
        block2: &[u8],
    ) -> Result<()> {
        let ev1 = EvidenceBlock::parse(block1)?;
        let ev2 = EvidenceBlock::parse(block2)?;

        // Same signer
        if ev1.pubkey != ev2.pubkey {
            return Err(PrecompileError::Revert(
                "evidence blocks must have the same signer".into(),
            ));
        }

        // Different proposals
        if ev1.proposal_bytes == ev2.proposal_bytes {
            return Err(PrecompileError::Revert(
                "proposals must differ for double-proposal evidence".into(),
            ));
        }

        // Same round (epoch + view)
        let round1 = ev1.round()?;
        let round2 = ev2.round()?;
        if round1 != round2 {
            return Err(PrecompileError::Revert(
                "proposals must be for the same round".into(),
            ));
        }

        // A-03: Compute canonical evidence hash and reject duplicates.
        // Normalize order so (block1, block2) and (block2, block1) produce the same hash.
        let evidence_hash = canonical_evidence_hash(block1, block2);
        if self.evidence_processed.read(&evidence_hash)? {
            return Err(PrecompileError::Revert("evidence already processed".into()));
        }

        // Verify both signatures
        ev1.verify_notarize_signature()?;
        ev2.verify_notarize_signature()?;

        // Look up validator by consensus pubkey hash (keccak256 of 48-byte BLS pubkey)
        let vs = ValidatorSet::new(self.storage.clone());
        let validator_addr = vs.lookup_by_pubkey_hash(ev1.pubkey_hash())?;
        if validator_addr.is_zero() {
            return Err(PrecompileError::Revert(
                "signer is not a registered validator".into(),
            ));
        }

        // Mark evidence as processed before applying effects
        self.evidence_processed.write(&evidence_hash, true)?;

        // Felony: forced exit + slash + reward evidence submitter.
        self.apply_evidence_felony(validator_addr, caller)
    }

    /// Submits conflicting vote evidence (notarize + nullify in the same view).
    ///
    /// Each evidence block is encoded as:
    ///   `pubkey[48] || signature[96] || payload_bytes[variable]`
    ///
    /// One vote must be a valid notarize signature and the other a valid nullify
    /// signature for the same round (epoch + view) by the same signer. This proves
    /// the validator voted both to accept and skip the same view.
    ///
    /// On success: the validator is forced out and slashed (felony), and the
    /// evidence submitter receives a reward.
    pub fn submit_conflicting_vote_evidence(
        &mut self,
        caller: Address,
        vote1: &[u8],
        vote2: &[u8],
    ) -> Result<()> {
        let ev1 = EvidenceBlock::parse(vote1)?;
        let ev2 = EvidenceBlock::parse(vote2)?;

        // Same signer
        if ev1.pubkey != ev2.pubkey {
            return Err(PrecompileError::Revert(
                "evidence blocks must have the same signer".into(),
            ));
        }

        // Same round (epoch + view)
        let round1 = ev1.round()?;
        let round2 = ev2.round()?;
        if round1 != round2 {
            return Err(PrecompileError::Revert(
                "votes must be for the same round".into(),
            ));
        }

        // A-03: Compute canonical evidence hash and reject duplicates.
        let evidence_hash = canonical_evidence_hash(vote1, vote2);
        if self.evidence_processed.read(&evidence_hash)? {
            return Err(PrecompileError::Revert("evidence already processed".into()));
        }

        // Verify conflicting vote types: one must be notarize, the other nullify.
        // Try ev1=notarize + ev2=nullify first, then the reverse.
        let valid = (ev1.verify_notarize_signature().is_ok()
            && ev2.verify_nullify_signature().is_ok())
            || (ev1.verify_nullify_signature().is_ok() && ev2.verify_notarize_signature().is_ok());

        if !valid {
            return Err(PrecompileError::Revert(
                "evidence must contain one valid notarize and one valid nullify signature".into(),
            ));
        }

        // Look up validator by consensus pubkey hash
        let vs = ValidatorSet::new(self.storage.clone());
        let validator_addr = vs.lookup_by_pubkey_hash(ev1.pubkey_hash())?;
        if validator_addr.is_zero() {
            return Err(PrecompileError::Revert(
                "signer is not a registered validator".into(),
            ));
        }

        // Mark evidence as processed before applying effects
        self.evidence_processed.write(&evidence_hash, true)?;

        // Felony: forced exit + slash + reward evidence submitter.
        self.apply_evidence_felony(validator_addr, caller)
    }

    /// Applies a felony penalty from evidence submission: forced exit, slash, reward submitter.
    fn apply_evidence_felony(
        &mut self,
        validator: Address,
        evidence_submitter: Address,
    ) -> Result<()> {
        let block_number = self.storage.block_number().unwrap_or(0);
        // Felony: JAIL (not force-exit) + slash. Jail before slash_stake (which
        // leaves a JAILED status untouched).
        let mut vs = ValidatorSet::new(self.storage.clone());
        vs.jail_validator(validator)?;

        let fc = self.felony_count.read(&validator)? + 1;
        self.felony_count.write(&validator, fc)?;

        let slash_percent = self.slash_amount_percent()?;
        let mut staking = Staking::new(self.storage.clone());
        let slashed_amount = staking.slash_stake(validator, slash_percent)?;

        crate::metrics::record_felony_count(validator, fc);
        crate::metrics::record_validator_slashed(validator, "evidence_felony");

        journal_record(JournalRecord::EvidenceFelony {
            wall_clock: iso8601_now(),
            block_number,
            validator: format!("{validator:?}"),
            evidence_submitter: format!("{evidence_submitter:?}"),
            felony_count: fc,
            slash_percent,
            slashed_amount: slashed_amount.to_string(),
        });

        warn!(
            target: "outbe::slashing",
            event = "evidence_felony",
            %validator,
            %evidence_submitter,
            felony_count = fc,
            slash_percent,
            slashed_amount = %slashed_amount,
            block_number,
            "evidence-based felony applied — validator force-exited, stake slashed, submitter rewarded",
        );

        // Reward evidence submitter: mint evidence_reward_percent of slashed amount.
        // A-05: slash_stake now burns slashed tokens from STAKING_ADDRESS, so we
        // mint the reward directly to the submitter. Net effect: (slashed - reward)
        // is burned from supply, reward goes to the submitter.
        let mut reward = U256::ZERO;
        if !slashed_amount.is_zero() {
            let reward_pct = self.evidence_reward_percent()?;
            reward = slashed_amount * U256::from(reward_pct) / U256::from(100u64);
            if !reward.is_zero() {
                self.storage.increase_balance(evidence_submitter, reward)?;
            }
        }

        self.emit(ISlashIndicator::EvidenceFelonyApplied {
            validator,
            submitter: evidence_submitter,
            slashedAmount: slashed_amount,
            submitterReward: reward,
        })?;

        Ok(())
    }

    /// submit evidence that the Phase 1 system transaction in a
    /// child block carried an invalid threshold VRF proof.
    ///
    /// `evidence` is the wire form of [`InvalidVrfProofEvidence`] (see
    /// `vrf_evidence.rs`). The runtime applies, in order:
    ///
    /// 1. **Submitter ACL**: `caller` must be a currently-`ACTIVE` validator
    ///    in `ValidatorSet`. Reading + verifying VRF/BLS proofs is heavy
    ///    cryptographic work; gating the entry-point to active validators
    ///    keeps DoS exposure inside the staked set (a malicious validator
    ///    pays gas AND has slashable stake at risk) instead of permitting
    ///    arbitrary EOAs to spam the chain.
    /// 2. Size cap (`invalid_vrf_evidence_max_bytes`).
    /// 3. Block-age cap (`invalid_vrf_evidence_max_age_blocks`).
    /// 4. Epoch-lag cap (`invalid_vrf_evidence_max_epoch_lag`) — read from
    ///    on-chain state via [`ValidatorSet::epoch_number`]
    ///    BP-0 option C: epoch is consensus state, not a derived value).
    /// 5. Child and parent canonicity: claimed child and parent
    ///    hashes must match the canonical chain.
    /// 6. Dedup via
    ///    [`outbe_consensus::proof::invalid_vrf_evidence_hash_v2`] keyed by
    ///    `(child_block_hash, keccak256(phase1_tx_bytes))`; replay of the same
    ///    evidence reverts with `"evidence already processed"` matching the
    ///    `submitDoubleProposalEvidence` / `submitConflictingVoteEvidence`
    ///    precedent.
    /// 7. Cryptographic proposer attribution (D-2): validate
    ///    `phase1_tx_bytes` as the canonical Phase 1 tx for the child block,
    ///    recover the signer, and decode metadata from its calldata. The
    ///    signed tx is the only source of truth for metadata/proof bytes.
    /// 8. Look up the committee snapshot for `metadata.finalized_epoch +
    ///    committee_set_hash` via the canonical
    ///    [`outbe_validatorset::state::committee_snapshot_key`] +
    ///    [`outbe_validatorset::state::read_committee_snapshot`] path; reject
    ///    if the snapshot is absent OR if the recovered proposer is not in
    ///    the committee.
    /// 9. Re-run [`outbe_consensus::proof::verify_v2_proof`] against the
    ///    same metadata/snapshot/parent_hash the child block used; accept
    ///    only VRF-class failures from `V2VerifyError`. Any `Ok` or
    ///    non-VRF rejection reverts — the precompile is strictly for VRF
    ///    misbehavior, not for re-litigating BLS quorum or accounting
    ///    binding failures.
    /// 10. Mark dedup BEFORE applying effects, then call
    ///     [`Self::apply_evidence_felony`] for forced exit + 5% slash +
    ///     10% submitter reward (reusing the existing felony helper —
    ///     economics shared with the other evidence types).
    pub fn submit_invalid_vrf_evidence(
        &mut self,
        caller: Address,
        evidence_bytes: &[u8],
    ) -> Result<()> {
        self.submit_invalid_vrf_evidence_with_schedule(
            caller,
            evidence_bytes,
            &OutbeProtocolSchedule::default(),
        )
    }

    /// Test seam for [`Self::submit_invalid_vrf_evidence`].
    ///
    /// Production callers go through the no-arg `submit_invalid_vrf_evidence`,
    /// which always passes [`OutbeProtocolSchedule::default`] — that is the
    /// canonical V2 schedule and the only schedule the precompile dispatcher
    /// ever uses. This `_with_schedule` variant exists so integration tests
    /// can relax admissibility caps (max_age, max_epoch_lag, max_bytes) to
    /// stress a single axis without bumping into the others.
    #[doc(hidden)]
    pub fn submit_invalid_vrf_evidence_with_schedule(
        &mut self,
        caller: Address,
        evidence_bytes: &[u8],
        schedule: &OutbeProtocolSchedule,
    ) -> Result<()> {
        // (1) Submitter ACL. Only currently-ACTIVE validators can submit.
        // Rationale: the verifier path is heavy cryptography (BLS + VRF +
        // ecrecover + storage reads); restricting the entry-point to the
        // staked set means any griefer pays gas AND has slashable stake at
        // risk, so DoS becomes self-destructive rather than free.
        let vs = ValidatorSet::new(self.storage.clone());
        let caller_status = vs.val_status.read(&caller)?;
        if caller_status != validator_status::ACTIVE {
            return Err(PrecompileError::Revert(format!(
                "submitter {caller} is not an ACTIVE validator (status: {caller_status})"
            )));
        }

        // (2) Size cap. Bound the work the precompile body does on
        // attacker-controlled input.
        if evidence_bytes.len() > schedule.invalid_vrf_evidence_max_bytes {
            return Err(PrecompileError::Revert(format!(
                "evidence too large: {} > {} bytes",
                evidence_bytes.len(),
                schedule.invalid_vrf_evidence_max_bytes,
            )));
        }

        // (3) Decode the wire form.
        let ev = InvalidVrfProofEvidence::decode(evidence_bytes)?;

        // (4) Block-age admissibility.
        let current_block = self.storage.block_number().unwrap_or(0);
        let max_acceptable_block = ev
            .child_block_number
            .saturating_add(schedule.invalid_vrf_evidence_max_age_blocks);
        if current_block > max_acceptable_block {
            return Err(PrecompileError::Revert(format!(
                "evidence stale: current_block {} > child_block {} + max_age {}",
                current_block, ev.child_block_number, schedule.invalid_vrf_evidence_max_age_blocks,
            )));
        }

        // (5) Epoch-lag admissibility — read the canonical on-chain
        // epoch counter from ValidatorSet (option C: epoch is consensus
        // state, recorded by update_epoch at boundaries; we do NOT
        // re-derive it from block height).
        let vs = ValidatorSet::new(self.storage.clone());
        let current_epoch_u256 = vs.epoch_number.read()?;
        let current_epoch: u64 = current_epoch_u256
            .try_into()
            .map_err(|_| PrecompileError::Revert("ValidatorSet.epoch_number exceeds u64".into()))?;
        let max_acceptable_epoch = ev
            .child_epoch
            .saturating_add(schedule.invalid_vrf_evidence_max_epoch_lag);
        if current_epoch > max_acceptable_epoch {
            return Err(PrecompileError::Revert(format!(
                "evidence epoch-stale: current_epoch {} > child_epoch {} + max_lag {}",
                current_epoch, ev.child_epoch, schedule.invalid_vrf_evidence_max_epoch_lag,
            )));
        }

        // (6) Child + parent canonicity. The evidence must bind to
        // canonical chain history on both sides: the accused child block and
        // the parent proof target.
        let canonical_child = self
            .storage
            .canonical_block_hash(ev.child_block_number)?
            .ok_or_else(|| {
                PrecompileError::Revert(format!(
                    "evidence child block {} not in canonical-history window",
                    ev.child_block_number,
                ))
            })?;
        if canonical_child != ev.child_block_hash {
            return Err(PrecompileError::Revert(format!(
                "evidence child hash {} is not canonical at block {} (canonical: {})",
                ev.child_block_hash, ev.child_block_number, canonical_child,
            )));
        }

        if ev.parent_block_number.saturating_add(1) != ev.child_block_number {
            return Err(PrecompileError::Revert(format!(
                "evidence parent/child number mismatch: parent={} child={}",
                ev.parent_block_number, ev.child_block_number,
            )));
        }

        let canonical_parent = self
            .storage
            .canonical_block_hash(ev.parent_block_number)?
            .ok_or_else(|| {
                PrecompileError::Revert(format!(
                    "evidence parent block {} not in canonical-history window",
                    ev.parent_block_number,
                ))
            })?;
        if canonical_parent != ev.parent_block_hash {
            return Err(PrecompileError::Revert(format!(
                "evidence parent hash {} is not canonical at block {} (canonical: {})",
                ev.parent_block_hash, ev.parent_block_number, canonical_parent,
            )));
        }

        // (7) Dedup key. A child block has exactly one Phase 1 system
        // transaction, so (child_hash, phase1_tx_hash) uniquely
        // identifies one invalid-VRF event.
        let phase1_tx_hash = keccak256(&ev.phase1_tx_bytes);
        let evidence_hash = invalid_vrf_evidence_hash_v2(ev.child_block_hash, phase1_tx_hash);
        if self.invalid_vrf_evidence_processed.read(&evidence_hash)? {
            return Err(PrecompileError::Revert("evidence already processed".into()));
        }

        // (8) Cryptographic proposer attribution. The Phase 1 tx is signed by
        // the child block's proposer. Its calldata is the single
        // source of truth for metadata and proof bytes.
        let chain_id = self.storage.chain_id()?;
        let (proposer, calldata) =
            recover_phase1_proposer(&ev.phase1_tx_bytes, chain_id, ev.child_block_number)
                .map_err(|err| PrecompileError::Revert(format!("phase1_tx invalid: {err}")))?;
        let metadata = match SystemTxInputV2::decode(calldata.as_ref()).map_err(|err| {
            PrecompileError::Revert(format!("phase1 calldata decode failed: {err}"))
        })? {
            SystemTxInputV2::CertifiedParentAccounting { metadata } => metadata,
            other => {
                return Err(PrecompileError::Revert(format!(
                    "phase1 calldata is not CertifiedParentAccounting: {:?}",
                    other.kind(),
                )));
            }
        };
        if metadata.finalized_block_number != ev.parent_block_number
            || metadata.finalized_block_hash != ev.parent_block_hash
        {
            return Err(PrecompileError::Revert(format!(
                "metadata parent binding mismatch: evidence=({}, {}), metadata=({}, {})",
                ev.parent_block_number,
                ev.parent_block_hash,
                metadata.finalized_block_number,
                metadata.finalized_block_hash,
            )));
        }
        if metadata.finalized_epoch != ev.child_epoch {
            return Err(PrecompileError::Revert(format!(
                "metadata epoch {} does not match evidence child_epoch {}",
                metadata.finalized_epoch, ev.child_epoch,
            )));
        }

        // (9) Load the canonical committee snapshot for the child's epoch.
        let snapshot_key =
            committee_snapshot_key(metadata.finalized_epoch, metadata.committee_set_hash);
        let snapshot =
            read_committee_snapshot(self.storage.clone(), snapshot_key)?.ok_or_else(|| {
                PrecompileError::Revert(format!(
                    "no committee snapshot for finalized_epoch={} committee_set_hash={}",
                    metadata.finalized_epoch, metadata.committee_set_hash,
                ))
            })?;

        // (10) Proposer must be in the snapshot's committee. Defends
        // against a future bug that signs a Phase 1 tx with a key not
        // bound to any active validator — without this check, the
        // felony helper would call jail_validator on a
        // non-existent validator and the slash path would silently
        // no-op.
        if !snapshot
            .committee
            .iter()
            .any(|entry| entry.address == proposer)
        {
            return Err(PrecompileError::Revert(format!(
                "phase1_tx proposer {proposer} not in committee for epoch {}",
                metadata.finalized_epoch,
            )));
        }

        // (11) Re-verify the proof. We expect verify_v2_proof to REJECT
        // with a VRF-class error; anything else is non-slashable here.
        let verify_err = match verify_v2_proof(
            &metadata,
            &snapshot,
            metadata.proof.as_ref(),
            ev.parent_block_hash,
        ) {
            Ok(_) => {
                return Err(PrecompileError::Revert(
                    "evidence shows a VALID proof; nothing to slash".into(),
                ));
            }
            Err(err) => err,
        };
        let failure_class = classify_vrf_failure(&verify_err).ok_or_else(|| {
            PrecompileError::Revert(format!(
                "verify_v2_proof rejected with non-VRF class ({verify_err}); not slashable here",
            ))
        })?;

        // (12) Mark dedup BEFORE applying effects so a panic / abort
        // between this point and the felony cannot enable replay.
        self.invalid_vrf_evidence_processed
            .write(&evidence_hash, true)?;

        // (13) Apply felony — forced exit + 5% slash + 10% submitter
        // reward, using the same helper the other evidence types call.
        self.apply_evidence_felony(proposer, caller)?;

        // (14) Canonical event with re-derived failure class.
        self.emit(ISlashIndicator::InvalidVrfProofEvidenceApplied {
            proposer,
            submitter: caller,
            childBlockHash: ev.child_block_hash,
            failureCode: failure_class,
        })?;

        // (15) Journal + structured warn! for operator visibility.
        let block_number = self.storage.block_number().unwrap_or(0);
        journal_record(JournalRecord::InvalidVrfProofEvidence {
            wall_clock: iso8601_now(),
            block_number,
            proposer: format!("{proposer:?}"),
            evidence_submitter: format!("{caller:?}"),
            child_block_hash: format!("{:?}", ev.child_block_hash),
            child_block_number: ev.child_block_number,
            child_epoch: ev.child_epoch,
            failure_class,
        });
        warn!(
            target: "outbe::slashing",
            event = "invalid_vrf_proof_evidence",
            %proposer,
            %caller,
            child_block_hash = %ev.child_block_hash,
            child_block_number = ev.child_block_number,
            child_epoch = ev.child_epoch,
            failure_class,
            block_number,
            "invalid-VRF evidence accepted — proposer self-incriminated by Phase 1 tx signature",
        );

        Ok(())
    }

    /// Applies a felony for byzantine behavior detected by the consensus layer.
    ///
    /// Called from post-execution hooks when the consensus layer detects equivocation
    /// (ConflictingNotarize, ConflictingFinalize, NullifyFinalize).
    /// Unlike `apply_evidence_felony`, there is no external evidence submitter,
    /// so no reward is distributed.
    pub fn slash_byzantine(&mut self, validator: Address) -> Result<()> {
        let block_number = self.storage.block_number().unwrap_or(0);
        // Felony: JAIL (not force-exit) + slash. Jail before slash_stake (which
        // leaves a JAILED status untouched).
        let mut vs = ValidatorSet::new(self.storage.clone());
        vs.jail_validator(validator)?;

        let fc = self.felony_count.read(&validator)? + 1;
        self.felony_count.write(&validator, fc)?;

        let slash_percent = self.slash_amount_percent()?;
        let mut staking = Staking::new(self.storage.clone());
        let slashed_amount = staking.slash_stake(validator, slash_percent)?;

        crate::metrics::record_felony_count(validator, fc);
        crate::metrics::record_validator_slashed(validator, "byzantine");

        journal_record(JournalRecord::ByzantineFelony {
            wall_clock: iso8601_now(),
            block_number,
            validator: format!("{validator:?}"),
            felony_count: fc,
            slash_percent,
            slashed_amount: slashed_amount.to_string(),
        });

        warn!(
            target: "outbe::slashing",
            event = "byzantine_felony",
            %validator,
            felony_count = fc,
            slash_percent,
            slashed_amount = %slashed_amount,
            block_number,
            "byzantine felony — equivocation detected, validator force-exited and slashed",
        );

        self.emit(ISlashIndicator::ByzantineFelony {
            validator,
            slashedAmount: slashed_amount,
            felonyCount: fc,
        })?;

        Ok(())
    }

    /// Resets per-epoch miss counters (proposer and voter) to zero for all given validators.
    ///
    /// Called at epoch boundary. Does NOT reset felony_count (that is cumulative).
    pub fn reset_epoch_counters(&mut self, validators: &[Address]) -> Result<()> {
        tracing::debug!(
            target: "outbe::slashing",
            event = "epoch_counters_reset",
            validator_count = validators.len(),
            block_number = self.storage.block_number().unwrap_or(0),
            "resetting per-epoch proposer/voter miss counters",
        );
        crate::metrics::record_epoch_counters_reset(validators.len());
        for v in validators {
            crate::metrics::record_proposer_miss_count(*v, 0);
            crate::metrics::record_voter_miss_count(*v, 0);
        }
        journal_record(JournalRecord::EpochCountersReset {
            wall_clock: iso8601_now(),
            block_number: self.storage.block_number().unwrap_or(0),
            validator_count: validators.len(),
        });
        for &validator in validators {
            self.proposer_miss_count.write(&validator, 0)?;
            self.voter_miss_count.write(&validator, 0)?;
        }
        Ok(())
    }

    // --- Getters ---

    /// Returns the current proposer miss count for `validator`.
    pub fn get_proposer_miss_count(&self, validator: Address) -> Result<u64> {
        self.proposer_miss_count.read(&validator)
    }

    /// Returns the current voter miss count for `validator`.
    pub fn get_voter_miss_count(&self, validator: Address) -> Result<u64> {
        self.voter_miss_count.read(&validator)
    }

    /// Returns the cumulative felony count for `validator`.
    pub fn get_felony_count(&self, validator: Address) -> Result<u64> {
        self.felony_count.read(&validator)
    }

    /// Returns whether an evidence hash has already been processed.
    pub fn is_evidence_processed(&self, evidence_hash: B256) -> Result<bool> {
        self.evidence_processed.read(&evidence_hash)
    }
}

/// A-03: Computes a canonical evidence hash that is order-independent.
///
/// Normalizes the order of two evidence payloads before hashing so that
/// `(block1, block2)` and `(block2, block1)` produce the same hash.
fn canonical_evidence_hash(ev1: &[u8], ev2: &[u8]) -> B256 {
    let (first, second) = if ev1 <= ev2 { (ev1, ev2) } else { (ev2, ev1) };
    let mut buf = Vec::with_capacity(first.len() + second.len());
    buf.extend_from_slice(first);
    buf.extend_from_slice(second);
    keccak256(&buf)
}

/// maps a [`V2VerifyError`] to a canonical VRF failure class code
/// emitted in [`InvalidVrfProofEvidenceApplied`] and the slashing journal.
///
/// Returns `None` for any non-VRF failure — those are not slashable through
/// `submitInvalidVrfProofEvidence` and the caller must revert.
///
/// The codes are stable wire constants once the precompile is live; renaming
/// or renumbering them is a hard-fork change.
pub fn classify_vrf_failure(err: &V2VerifyError) -> Option<u16> {
    match err {
        V2VerifyError::MissingVrfProof => Some(1),
        V2VerifyError::MalformedVrfProof => Some(2),
        V2VerifyError::WrongVrfMaterialVersion { .. } => Some(3),
        V2VerifyError::WrongVrfGroupKeyHash { .. } => Some(4),
        V2VerifyError::WrongVrfNamespace => Some(5),
        V2VerifyError::WrongVrfSeedRound { .. } => Some(6),
        V2VerifyError::InvalidVrfSignature => Some(7),
        _ => None,
    }
}
