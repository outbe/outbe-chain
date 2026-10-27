//! Default SlashIndicator policy. Nonzero `config_*` storage values take precedence.
//!
//! Miss thresholds count events per validator per epoch. A voter miss is an
//! absent vote for a finalized block. A proposer miss is a skipped leader view.
//! One finalized block can account for several misses of the same proposer.
//! Epoch transitions reset both miss counters, but preserve the felony count.
//!
//! Each non-felony multiple of a misdemeanor threshold emits a warning only.
//! A felony multiple takes precedence: it jails the validator and slashes stake.
//! Already jailed or exiting validators keep accumulating misses without another
//! penalty for the same continuous fault.
//!
//! Each felony default is four times its misdemeanor default. Voter thresholds
//! exceed the corresponding proposer thresholds because votes occur more often.
//! Both felony defaults fit within a 1,200-block epoch. Proposer opportunities
//! depend on committee size and skipped views, so this alone does not guarantee
//! that every validator can reach the proposer threshold. Short-epoch networks
//! must select suitable storage overrides.

/// Proposer misses between warning events within one epoch.
pub const DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD: u64 = 32;

/// Proposer misses at the first jail and stake slash within one epoch.
pub const DEFAULT_PROPOSER_FELONY_THRESHOLD: u64 = 128;

/// Missed finalized-block votes between warning events within one epoch.
pub const DEFAULT_VOTER_MISDEMEANOR_THRESHOLD: u64 = 128;

/// Missed finalized-block votes at the first jail and stake slash within one epoch.
pub const DEFAULT_VOTER_FELONY_THRESHOLD: u64 = 512;

/// Percentage of stake slashed for one felony.
pub const DEFAULT_SLASH_AMOUNT_PERCENT: u64 = 8;

/// Default percent of the slashed amount paid to a valid evidence submitter.
/// The default is 0%.
pub const DEFAULT_EVIDENCE_REWARD_PERCENT: u64 = 0;

/// Storage slot of `config_slash_amount_percent`.
pub const SLASH_AMOUNT_PERCENT_SLOT: u64 = 3;

/// Storage slot of `config_evidence_reward_percent`.
pub const EVIDENCE_REWARD_PERCENT_SLOT: u64 = 4;
