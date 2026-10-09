use alloy_primitives::{keccak256, Address, B256, U256};
use commonware_codec::ReadExt as _;
use commonware_cryptography::bls12381;
use commonware_utils::ordered::Set;
use outbe_consensus::proof::V2VerifyError;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::slashing_journal::{iso8601_now, record as journal_record, JournalRecord};
use outbe_staking::contract::Staking;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::state::read_committee_snapshot_for_epoch;
use outbe_validatorset::ValidatorLifecycle;
use tracing::warn;

use crate::evidence::EvidenceCommittee;
use crate::precompile::ISlashIndicator;
use crate::schema::SlashIndicator;

/// Default config values used when the stored value is zero (uninitialized).
// Felony thresholds are the maximum validator misses TOLERATED within one epoch.
// The per-epoch reset (`reset_epoch_counters`, run at the epoch boundary) zeroes
// the miss counters. Thus a validator that crosses the threshold inside an epoch
// is jailed and slashed immediately. Otherwise its count resets next epoch.
// The epoch (`config_epoch_length_blocks`) is the ~1-hour window that also drives
// DKG reshare / active-set rotation / counter reset. Thus a felony threshold MUST
// stay below the epoch length. Otherwise the reset wipes the counter before it
// can trigger. The prod epoch is 1200 ~= 1h at ~3s. Dev/localnet seeds a smaller
// threshold for its short epoch via genesis (see `scripts/bootstrap-testnet.sh`).
// A voter miss accrues ~1 per finalized block. A proposer miss accrues only on
// the validator's own leader slots (~1/N). Both are genesis-overridable per
// network (`config_*_felony_threshold` slots).
const DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD: u64 = 50;
const DEFAULT_PROPOSER_FELONY_THRESHOLD: u64 = 150;
// Graduated escalation requires misdemeanor (warn) < felony (slash). The
// two voter defaults were inverted (misdemeanor 500 > felony 150). Thus the harsh
// penalty fired before the warning could ever emit. The defaults are now restored
// to misdemeanor 150 and felony 500. The voter misdemeanor default equals
// the proposer felony default (150). It does not sit above that threshold.
// The voter felony default (500) is above both proposer thresholds. Voters
// accrue ~1 miss per finalized block vs a proposer's ~1 per own leader slot.
// Both also sit below the prod epoch length (1200). Thus the felony can still
// trigger before the per-epoch reset.
const DEFAULT_VOTER_MISDEMEANOR_THRESHOLD: u64 = 150;
const DEFAULT_VOTER_FELONY_THRESHOLD: u64 = 500;
const DEFAULT_SLASH_AMOUNT_PERCENT: u64 = 5;
const DEFAULT_EVIDENCE_REWARD_PERCENT: u64 = 10;

impl SlashIndicator<'_> {
    // --- Config helpers ---

    pub fn proposer_felony_threshold(&self) -> Result<u64> {
        let v = self.config_proposer_felony_threshold.read()?;
        Ok(if v == 0 {
            DEFAULT_PROPOSER_FELONY_THRESHOLD
        } else {
            v
        })
    }

    pub fn proposer_misdemeanor_threshold(&self) -> Result<u64> {
        let v = self.config_proposer_misdemeanor_threshold.read()?;
        Ok(if v == 0 {
            DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD
        } else {
            v
        })
    }

    pub fn voter_misdemeanor_threshold(&self) -> Result<u64> {
        let v = self.config_voter_misdemeanor_threshold.read()?;
        Ok(if v == 0 {
            DEFAULT_VOTER_MISDEMEANOR_THRESHOLD
        } else {
            v
        })
    }

    pub fn voter_felony_threshold(&self) -> Result<u64> {
        let v = self.config_voter_felony_threshold.read()?;
        Ok(if v == 0 {
            DEFAULT_VOTER_FELONY_THRESHOLD
        } else {
            v
        })
    }

    pub fn slash_amount_percent(&self) -> Result<u64> {
        let v = self.config_slash_amount_percent.read()?;
        Ok(if v == 0 {
            DEFAULT_SLASH_AMOUNT_PERCENT
        } else {
            v
        })
    }

    pub fn evidence_reward_percent(&self) -> Result<u64> {
        let v = self.config_evidence_reward_percent.read()?;
        Ok(if v == 0 {
            DEFAULT_EVIDENCE_REWARD_PERCENT
        } else {
            v
        })
    }

    // --- Shared slashing helpers ---

    /// The next reshare removes a validator that is already JAILED or EXITING
    /// from the consensus set. While it stays in the committee snapshot, it must
    /// NOT be re-felonied (re-jailed + re-slashed 5% at every subsequent miss
    /// threshold) for the same continuous liveness fault. That would compound to
    /// far more than the intended single-felony penalty.
    pub(crate) fn validator_already_penalized(&self, validator: Address) -> Result<bool> {
        let vs = ValidatorSet::new(self.storage.clone());
        Ok(matches!(
            vs.validator_lifecycle(validator)?,
            ValidatorLifecycle::JailRetained(_)
                | ValidatorLifecycle::Jail(_)
                | ValidatorLifecycle::Exiting(_)
        ))
    }

    /// Submitter ACL for the BLS-evidence precompile entry points: only
    /// currently-ACTIVE validators may submit. The verifiers run heavy
    /// cryptography (BLS pairings + ecrecover + storage reads). Gating to the
    /// staked set makes DoS self-destructive (a griefer pays gas AND has
    /// slashable stake at risk) instead of free on the ZeroFee chain.
    pub(crate) fn require_active_submitter(&self, caller: Address) -> Result<()> {
        let vs = ValidatorSet::new(self.storage.clone());
        let lifecycle = vs.validator_lifecycle(caller)?;
        if !lifecycle.is_active_status() {
            return Err(PrecompileError::Revert(format!(
                "submitter {caller} is not an ACTIVE validator (status: {})",
                lifecycle.stored_status().unwrap_or_default()
            )));
        }
        Ok(())
    }

    /// Build the ordered committee `Set` for `epoch` from the on-chain
    /// `CommitteeSnapshot`. Equivocation vote signatures are committee-bound
    /// (notarize/nullify/finalize namespaces fold `participant_set_commitment`).
    /// Thus verification must use the SAME committee that the Simplex signer used.
    /// The snapshot committee order matches the signer's participant `Set`. Both
    /// are the canonical sorted/deduped pubkey set, so the commitment bytes agree.
    pub(crate) fn committee_set_for_epoch(&self, epoch: u64) -> Result<EvidenceCommittee> {
        let snapshot =
            read_committee_snapshot_for_epoch(self.storage.clone(), epoch)?.ok_or_else(|| {
                PrecompileError::Revert(format!(
                    "no committee snapshot for evidence epoch {epoch}; cannot verify the \
                     committee-bound vote signature"
                ))
            })?;
        let mut keys = Vec::with_capacity(snapshot.committee.len());
        for entry in &snapshot.committee {
            let pk = bls12381::PublicKey::read(&mut entry.consensus_pubkey.as_slice()).map_err(
                |_| PrecompileError::Revert("invalid committee pubkey in snapshot".into()),
            )?;
            keys.push(pk);
        }
        Ok(Set::from_iter_dedup(keys))
    }

    /// Felony core shared by evidence and byzantine felonies, in this order:
    /// jail (not force-exit), felony count, then slash. Jail runs before
    /// slash_stake, which leaves a JAILED status untouched. Returns the felony
    /// count, the slash percent and the slashed amount.
    fn jail_count_and_slash(
        &mut self,
        validator: Address,
        slashed_label: &'static str,
    ) -> Result<(u64, u64, U256)> {
        let mut vs = ValidatorSet::new(self.storage.clone());
        vs.jail_validator(validator)?;

        let fc = self.felony_count.read(&validator)? + 1;
        self.felony_count.write(&validator, fc)?;

        let slash_percent = self.slash_amount_percent()?;
        let mut staking = Staking::new(self.storage.clone());
        let slashed_amount = staking.slash_stake(validator, slash_percent)?;

        crate::metrics::record_felony_count(validator, fc);
        crate::metrics::record_validator_slashed(validator, slashed_label);
        Ok((fc, slash_percent, slashed_amount))
    }

    /// Applies a felony penalty from evidence submission: jail, slash, reward submitter.
    pub(crate) fn apply_evidence_felony(
        &mut self,
        validator: Address,
        evidence_submitter: Address,
    ) -> Result<()> {
        let block_number = self.storage.block_number().unwrap_or(0);
        let (fc, slash_percent, slashed_amount) =
            self.jail_count_and_slash(validator, "evidence_felony")?;

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
            "evidence-based felony applied - validator force-exited, stake slashed, submitter rewarded",
        );

        // Reward evidence submitter: mint evidence_reward_percent of slashed amount.
        // slash_stake now burns slashed tokens from STAKING_ADDRESS, so we
        // mint the reward directly to the submitter. Net effect: (slashed - reward)
        // is burned from supply. The reward goes to the submitter.
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

    /// Applies a felony for byzantine behavior.
    ///
    /// No production caller invokes this method. The same three evidence classes
    /// go through `submit_conflicting_notarize_evidence`,
    /// `submit_conflicting_finalize_evidence`, and
    /// `submit_nullify_finalize_evidence`.
    /// Unlike `apply_evidence_felony`, there is no external evidence submitter.
    /// Thus this method distributes no reward.
    pub fn slash_byzantine(&mut self, validator: Address) -> Result<()> {
        let block_number = self.storage.block_number().unwrap_or(0);
        let (fc, slash_percent, slashed_amount) =
            self.jail_count_and_slash(validator, "byzantine")?;

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
            "byzantine felony - equivocation detected, validator force-exited and slashed",
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

/// The registered validator whose consensus pubkey hash is `pubkey_hash`.
pub(crate) fn registered_validator(vs: &ValidatorSet<'_>, pubkey_hash: B256) -> Result<Address> {
    let validator = vs.lookup_by_pubkey_hash(pubkey_hash)?;
    if validator.is_zero() {
        return Err(PrecompileError::Revert(
            "signer is not a registered validator".into(),
        ));
    }
    Ok(validator)
}

/// Computes a canonical evidence hash that is order-independent.
///
/// Normalizes the order of two evidence payloads before hashing so that
/// `(block1, block2)` and `(block2, block1)` produce the same hash.
pub(crate) fn canonical_evidence_hash(ev1: &[u8], ev2: &[u8]) -> B256 {
    let (first, second) = if ev1 <= ev2 { (ev1, ev2) } else { (ev2, ev1) };
    let mut buf = Vec::with_capacity(first.len() + second.len());
    buf.extend_from_slice(first);
    buf.extend_from_slice(second);
    keccak256(&buf)
}

/// Maps a [`V2VerifyError`] to a canonical VRF failure class code
/// emitted in [`InvalidVrfProofEvidenceApplied`] and the slashing journal.
///
/// Returns `None` for any non-VRF failure. Those failures are not slashable
/// through `submitInvalidVrfProofEvidence`, and the caller must revert.
///
/// The codes are stable wire constants once the precompile is live; renaming
/// or renumbering them is a hard-fork change.
pub fn classify_vrf_failure(err: &V2VerifyError) -> Option<u16> {
    match err {
        V2VerifyError::MalformedVrfProof => Some(2),
        V2VerifyError::WrongVrfMaterialVersion { .. } => Some(3),
        V2VerifyError::WrongVrfGroupKeyHash { .. } => Some(4),
        V2VerifyError::WrongVrfNamespace => Some(5),
        V2VerifyError::WrongVrfSeedRound { .. } => Some(6),
        V2VerifyError::InvalidVrfSignature => Some(7),
        _ => None,
    }
}
