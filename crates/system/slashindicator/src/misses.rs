//! Liveness misses: per-epoch proposer and voter miss counters, and the
//! misdemeanor and felony thresholds that act on them.

use alloy_primitives::{Address, B256};
use outbe_primitives::error::Result;
use outbe_primitives::slashing_journal::{iso8601_now, record as journal_record, JournalRecord};
use outbe_primitives::storage::types::Mapping;
use outbe_staking::contract::Staking;
use outbe_validatorset::contract::ValidatorSet;
use tracing::{info, warn};

use crate::precompile::ISlashIndicator;
use crate::schema::SlashIndicator;

/// The kind of liveness miss that a validator accrues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissKind {
    /// The validator did not propose in its own leader slot.
    Proposer,
    /// The validator did not vote for a finalized block.
    Voter,
}

/// The two thresholds of one miss kind.
struct MissThresholds {
    felony: u64,
    misdemeanor: u64,
}

impl MissKind {
    /// Lower-case name used in log messages.
    const fn name(self) -> &'static str {
        match self {
            Self::Proposer => "proposer",
            Self::Voter => "voter",
        }
    }

    const fn miss_event(self) -> &'static str {
        match self {
            Self::Proposer => "proposer_miss",
            Self::Voter => "voter_miss",
        }
    }

    const fn felony_event(self) -> &'static str {
        match self {
            Self::Proposer => "proposer_felony",
            Self::Voter => "voter_felony",
        }
    }

    const fn misdemeanor_event(self) -> &'static str {
        match self {
            Self::Proposer => "proposer_misdemeanor",
            Self::Voter => "voter_misdemeanor",
        }
    }

    /// Per-epoch miss counter of this kind.
    fn counter<'a, 's>(self, si: &'a SlashIndicator<'s>) -> &'a Mapping<'s, Address, u64> {
        match self {
            Self::Proposer => &si.proposer_miss_count,
            Self::Voter => &si.voter_miss_count,
        }
    }

    /// Per-finalized-block guard that makes a window pass run once.
    pub(crate) fn window_guard<'a, 's>(
        self,
        si: &'a SlashIndicator<'s>,
    ) -> &'a Mapping<'s, B256, bool> {
        match self {
            Self::Proposer => &si.proposer_window_slashed,
            Self::Voter => &si.voter_window_slashed,
        }
    }

    /// Reads the felony threshold, then the misdemeanor threshold.
    fn thresholds(self, si: &SlashIndicator<'_>) -> Result<MissThresholds> {
        let (felony, misdemeanor) = match self {
            Self::Proposer => (
                si.proposer_felony_threshold()?,
                si.proposer_misdemeanor_threshold()?,
            ),
            Self::Voter => (
                si.voter_felony_threshold()?,
                si.voter_misdemeanor_threshold()?,
            ),
        };
        Ok(MissThresholds {
            felony,
            misdemeanor,
        })
    }

    fn record_miss_metrics(self, validator: Address, count: u64) {
        match self {
            Self::Proposer => {
                crate::metrics::record_proposer_miss_count(validator, count);
                crate::metrics::record_proposer_miss_event(validator);
            }
            Self::Voter => {
                crate::metrics::record_voter_miss_count(validator, count);
                crate::metrics::record_voter_miss_event(validator);
            }
        }
    }

    fn miss_record(self, entry: &MissEntry, thresholds: &MissThresholds) -> JournalRecord {
        let (wall_clock, block_number, validator) = entry.journal_header();
        match self {
            Self::Proposer => JournalRecord::ProposerMiss {
                wall_clock,
                block_number,
                validator,
                count: entry.count,
                felony_threshold: thresholds.felony,
                misdemeanor_threshold: thresholds.misdemeanor,
            },
            Self::Voter => JournalRecord::VoterMiss {
                wall_clock,
                block_number,
                validator,
                count: entry.count,
                misdemeanor_threshold: thresholds.misdemeanor,
            },
        }
    }

    fn felony_record(
        self,
        entry: &MissEntry,
        felony_threshold: u64,
        felony_count: u64,
        slash_percent: u64,
    ) -> JournalRecord {
        let (wall_clock, block_number, validator) = entry.journal_header();
        match self {
            Self::Proposer => JournalRecord::ProposerFelony {
                wall_clock,
                block_number,
                validator,
                miss_count: entry.count,
                felony_threshold,
                felony_count,
                slash_percent,
            },
            Self::Voter => JournalRecord::VoterFelony {
                wall_clock,
                block_number,
                validator,
                miss_count: entry.count,
                felony_threshold,
                felony_count,
                slash_percent,
            },
        }
    }

    fn misdemeanor_record(self, entry: &MissEntry, misdemeanor_threshold: u64) -> JournalRecord {
        let (wall_clock, block_number, validator) = entry.journal_header();
        match self {
            Self::Proposer => JournalRecord::ProposerMisdemeanor {
                wall_clock,
                block_number,
                validator,
                miss_count: entry.count,
                misdemeanor_threshold,
            },
            Self::Voter => JournalRecord::VoterMisdemeanor {
                wall_clock,
                block_number,
                validator,
                miss_count: entry.count,
                misdemeanor_threshold,
            },
        }
    }

    fn emit_felony(self, si: &mut SlashIndicator<'_>, entry: &MissEntry, fc: u64) -> Result<()> {
        match self {
            Self::Proposer => si.emit(ISlashIndicator::ProposerFelony {
                validator: entry.validator,
                missCount: entry.count,
                felonyCount: fc,
            }),
            Self::Voter => si.emit(ISlashIndicator::VoterFelony {
                validator: entry.validator,
                missCount: entry.count,
                felonyCount: fc,
            }),
        }
    }

    fn emit_misdemeanor(self, si: &mut SlashIndicator<'_>, entry: &MissEntry) -> Result<()> {
        match self {
            Self::Proposer => si.emit(ISlashIndicator::ProposerMisdemeanor {
                validator: entry.validator,
                missCount: entry.count,
            }),
            Self::Voter => si.emit(ISlashIndicator::VoterMisdemeanor {
                validator: entry.validator,
                missCount: entry.count,
            }),
        }
    }
}

/// One recorded miss: the validator, its new miss count and the block.
struct MissEntry {
    validator: Address,
    count: u64,
    block_number: u64,
}

impl MissEntry {
    /// Wall clock, block number and validator text of a journal record.
    fn journal_header(&self) -> (String, u64, String) {
        (
            iso8601_now(),
            self.block_number,
            format!("{:?}", self.validator),
        )
    }
}

impl SlashIndicator<'_> {
    /// Records a proposer miss for `validator`.
    ///
    /// - Increments proposer_miss_count[validator].
    /// - At multiples of felony_threshold: jails and slashes the validator.
    /// - At multiples of misdemeanor_threshold (non-felony): misdemeanor logged only.
    pub fn slash_proposer(&mut self, validator: Address) -> Result<()> {
        self.record_miss(MissKind::Proposer, validator)
    }

    /// Records a voter miss for `validator`.
    ///
    /// - Increments voter_miss_count[validator].
    /// - At multiples of voter_felony_threshold: jails and slashes the validator.
    /// - At multiples of voter_misdemeanor_threshold (non-felony): misdemeanor logged only.
    pub fn slash_voter(&mut self, validator: Address) -> Result<()> {
        self.record_miss(MissKind::Voter, validator)
    }

    /// Records one miss of `kind` and applies the graduated penalty.
    pub(crate) fn record_miss(&mut self, kind: MissKind, validator: Address) -> Result<()> {
        let count = kind.counter(self).read(&validator)? + 1;
        kind.counter(self).write(&validator, count)?;

        let thresholds = kind.thresholds(self)?;
        let entry = MissEntry {
            validator,
            count,
            block_number: self.storage.block_number().unwrap_or(0),
        };

        kind.record_miss_metrics(validator, count);
        journal_record(kind.miss_record(&entry, &thresholds));
        info!(
            target: "outbe::slashing",
            event = kind.miss_event(),
            %validator,
            count,
            felony_threshold = thresholds.felony,
            misdemeanor_threshold = thresholds.misdemeanor,
            block_number = entry.block_number,
            "{} miss recorded",
            kind.name(),
        );

        // The code above records the miss. Skip felony/misdemeanor
        // punishment for a validator already JAILED/EXITING for this fault.
        if self.validator_already_penalized(validator)? {
            return Ok(());
        }

        if count > 0 && count % thresholds.felony == 0 {
            self.apply_miss_felony(kind, &entry, thresholds.felony)
        } else if count > 0 && count % thresholds.misdemeanor == 0 {
            self.apply_miss_misdemeanor(kind, &entry, thresholds.misdemeanor)
        } else {
            Ok(())
        }
    }

    /// Felony: increments the cumulative counter, then jails and slashes.
    fn apply_miss_felony(
        &mut self,
        kind: MissKind,
        entry: &MissEntry,
        felony_threshold: u64,
    ) -> Result<()> {
        let validator = entry.validator;
        let fc = self.felony_count.read(&validator)? + 1;
        self.felony_count.write(&validator, fc)?;

        // Felony: JAIL (not force-exit) + slash. Jail BEFORE slash_stake.
        // The slash_stake call demotes ACTIVE/PENDING below min_stake but
        // leaves a JAILED status untouched. Thus this ordering preserves JAILED.
        let mut vs = ValidatorSet::new(self.storage.clone());
        vs.jail_validator(validator)?;

        let slash_percent = self.slash_amount_percent()?;
        let mut staking = Staking::new(self.storage.clone());
        staking.slash_stake(validator, slash_percent)?;

        crate::metrics::record_felony_count(validator, fc);
        crate::metrics::record_validator_slashed(validator, kind.felony_event());

        journal_record(kind.felony_record(entry, felony_threshold, fc, slash_percent));
        warn!(
            target: "outbe::slashing",
            event = kind.felony_event(),
            %validator,
            miss_count = entry.count,
            felony_threshold,
            felony_count = fc,
            slash_percent,
            block_number = entry.block_number,
            "{} felony - validator force-exited and slashed",
            kind.name(),
        );

        kind.emit_felony(self, entry, fc)
    }

    /// Misdemeanor: records the crossing only.
    fn apply_miss_misdemeanor(
        &mut self,
        kind: MissKind,
        entry: &MissEntry,
        misdemeanor_threshold: u64,
    ) -> Result<()> {
        journal_record(kind.misdemeanor_record(entry, misdemeanor_threshold));
        info!(
            target: "outbe::slashing",
            event = kind.misdemeanor_event(),
            validator = %entry.validator,
            miss_count = entry.count,
            misdemeanor_threshold,
            block_number = entry.block_number,
            "{} misdemeanor threshold crossed",
            kind.name(),
        );
        kind.emit_misdemeanor(self, entry)
    }
}
