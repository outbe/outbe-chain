//! Module-local error types for the Oracle.
//!
//! Oracle errors cover pair registration, votes, VWAP/TWAP, and genesis operations.

use crate::schema::PairIndex;
use alloy_primitives::U256;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OracleError {
    // -- pair registry ------------------------------------------------------
    #[error("pair base and quote must differ")]
    PairBaseQuoteIdentical,
    #[error("pair {pair} does not match the registered orientation")]
    PairNotCanonical { pair: AddressPair },
    #[error("pair {pair} already registered")]
    PairAlreadyRegistered { pair: AddressPair },
    #[error("pair {pair} not registered")]
    PairNotRegistered { pair: AddressPair },
    #[error("pair index {index} out of range")]
    PairIndexOutOfRange { index: PairIndex },

    // -- authorization ------------------------------------------------------
    /// The argument completes the sentence, e.g. `"activate vote target"`.
    #[error("only system can {0}")]
    OnlySystem(&'static str),
    #[error("caller is not an active ORACLE signer")]
    NotActiveOracleSigner,

    // -- vote submission ----------------------------------------------------
    #[error("vote tuple count exceeds registered pair count")]
    VoteTupleCountExceedsPairCount,
    #[error("duplicate pair in vote submission")]
    DuplicatePairInVote,
    #[error("pair is not a vote target")]
    PairNotVoteTarget,
    #[error("validator already voted this period")]
    AlreadyVotedThisPeriod,
    #[error("vote price {price} exceeds {max}")]
    VotePriceExceedsCap { price: U256, max: U256 },
    #[error("vote volume {volume} exceeds {max}")]
    VoteVolumeExceedsCap { volume: U256, max: U256 },

    // -- VWAP / TWAP --------------------------------------------------------
    #[error("no VWAP data in the requested time range")]
    NoVwapData,
    #[error("no TWAP data in the requested time range")]
    NoTwapData,
    #[error("missing TWAP data")]
    MissingTwapData,
    /// The argument names the accumulator that overflowed.
    #[error("VWAP overflow: {0}")]
    VwapOverflow(&'static str),
    #[error("TWAP overflow")]
    TwapOverflow,
    #[error("cross-currency conversion overflow")]
    CrossRateOverflow,
    #[error("tally arithmetic overflow: {0}")]
    TallyArithmeticOverflow(&'static str),
    #[error("COEN rate for currency {iso_code} is stale")]
    StaleCoenRate { iso_code: u16 },
    #[error("lookback_seconds must be > 0 and <= lookback_duration")]
    InvalidLookbackSeconds,
    #[error("start_time must be less than end_time")]
    InvalidVwapRange,
    #[error("end_time - start_time must be <= lookback_duration")]
    VwapRangeTooWide,
    #[error("worldwide day VWAP snapshot not found")]
    WorldwideDayVwapSnapshotNotFound,
    #[error("no finalized VWAP for that UTC day")]
    NoFinalizedUtcDayVwap,
    #[error("unsupported trailing VWAP policy")]
    InvalidVwapPolicy,
    #[error("malformed or unfinalized VWAP snapshot id")]
    InvalidVwapSnapshot,

    // -- reference currencies -----------------------------------------------
    #[error("iso_code {iso_code} is not a registered reference currency")]
    NotReferenceCurrency { iso_code: u16 },
    #[error("no policy rate for iso_code {iso_code}")]
    NoPolicyRate { iso_code: u16 },

    // -- slash window -------------------------------------------------------
    #[error("Oracle slash-window validator set size {actual} exceeds cap {cap}")]
    SlashWindowValidatorSetExceedsCap { actual: usize, cap: usize },

    // -- genesis config -----------------------------------------------------
    #[error("vote_period must be > 0")]
    VotePeriodZero,
    #[error("slash_window must be > 0")]
    SlashWindowZero,
    #[error("lookback_duration exceeds snapshot retention window")]
    LookbackExceedsRetention,
    #[error("{field} {value} exceeds 1e18 (a ratio above 1)")]
    RatioAboveOne { field: &'static str, value: U256 },
    #[error("slash percent of slash_fraction {slash_fraction} exceeds u64")]
    SlashPercentOutOfRange { slash_fraction: U256 },
    #[error("reference iso_code must be non-zero")]
    ReferenceIsoCodeZero,
    #[error("duplicate reference iso_code: {iso_code}")]
    DuplicateReferenceIsoCode { iso_code: u16 },
    #[error("reference currencies must be sorted in ascending ISO order")]
    ReferenceCurrenciesNotSorted,
    #[error("reference currencies must include USD 840")]
    MissingUsdReferenceCurrency,
    #[error("policy iso_code must be non-zero")]
    PolicyIsoCodeZero,
    #[error("duplicate policy iso_code: {iso_code}")]
    DuplicatePolicyIsoCode { iso_code: u16 },
    #[error("policy rates must be sorted in ascending ISO order")]
    PolicyRatesNotSorted,
    #[error("policy rate must be non-zero for iso_code {iso_code}")]
    PolicyRateZero { iso_code: u16 },
    #[error("reference currency {iso_code} has no policy rate")]
    MissingPolicyRate { iso_code: u16 },

    // -- genesis aggregate votes --------------------------------------------
    #[error("aggregate vote validator must be non-zero")]
    AggregateVoteValidatorZero,
    #[error("duplicate aggregate vote validator")]
    DuplicateAggregateVoteValidator,
    #[error("aggregate vote already exists for validator")]
    AggregateVoteAlreadyExists,
    #[error("aggregate vote tuple count exceeds registered pair count")]
    AggregateVoteTupleCountExceedsPairCount,
    #[error("aggregate vote pair is not registered")]
    AggregateVotePairNotRegistered,
    #[error("duplicate pair in aggregate vote")]
    DuplicatePairInAggregateVote,
    #[error("aggregate vote pair is not a vote target")]
    AggregateVotePairNotVoteTarget,
    #[error("missing aggregate vote validator at voter index {voter_index}")]
    MissingAggregateVoteValidator { voter_index: u32 },
    #[error("voter list contains validator without aggregate vote")]
    VoterListEntryWithoutVote,
    #[error("missing pair metadata for pair_id {pair_id}")]
    MissingPairMetadata { pair_id: u32 },

    // -- storage invariants -------------------------------------------------
    #[error("Oracle snapshot write index overflow")]
    SnapshotWriteIndexOverflow,
    #[error("Oracle S-curve write index overflow")]
    ScurveWriteIndexOverflow,
    #[error("snapshot timestamp precedes the previous snapshot")]
    SnapshotOutOfOrder,
    #[error("No rate for pair {pair}")]
    NoRateForPair { pair: AddressPair },
}

impl From<OracleError> for PrecompileError {
    fn from(err: OracleError) -> Self {
        use OracleError::*;
        match err {
            SnapshotWriteIndexOverflow
            | ScurveWriteIndexOverflow
            | PairBaseQuoteIdentical
            | PairNotCanonical { .. }
            | PairAlreadyRegistered { .. }
            | PairNotRegistered { .. }
            | PairIndexOutOfRange { .. }
            | NoRateForPair { .. }
            | OnlySystem(_)
            | NotActiveOracleSigner
            | VoteTupleCountExceedsPairCount
            | DuplicatePairInVote
            | PairNotVoteTarget
            | AlreadyVotedThisPeriod
            | VotePriceExceedsCap { .. }
            | VoteVolumeExceedsCap { .. }
            | NoVwapData
            | NoTwapData
            | MissingTwapData
            | VwapOverflow(_)
            | TwapOverflow
            | CrossRateOverflow
            | TallyArithmeticOverflow(_)
            | StaleCoenRate { .. }
            | InvalidLookbackSeconds
            | InvalidVwapRange
            | VwapRangeTooWide
            | WorldwideDayVwapSnapshotNotFound
            | NoFinalizedUtcDayVwap
            | InvalidVwapPolicy
            | InvalidVwapSnapshot
            | SnapshotOutOfOrder
            | NotReferenceCurrency { .. }
            | NoPolicyRate { .. }
            | SlashWindowValidatorSetExceedsCap { .. }
            | VotePeriodZero
            | SlashWindowZero
            | LookbackExceedsRetention
            | RatioAboveOne { .. }
            | SlashPercentOutOfRange { .. }
            | ReferenceIsoCodeZero
            | DuplicateReferenceIsoCode { .. }
            | ReferenceCurrenciesNotSorted
            | MissingUsdReferenceCurrency
            | PolicyIsoCodeZero
            | DuplicatePolicyIsoCode { .. }
            | PolicyRatesNotSorted
            | PolicyRateZero { .. }
            | MissingPolicyRate { .. }
            | AggregateVoteValidatorZero
            | DuplicateAggregateVoteValidator
            | AggregateVoteAlreadyExists
            | AggregateVoteTupleCountExceedsPairCount
            | AggregateVotePairNotRegistered
            | DuplicatePairInAggregateVote
            | AggregateVotePairNotVoteTarget
            | MissingAggregateVoteValidator { .. }
            | VoterListEntryWithoutVote
            | MissingPairMetadata { .. } => PrecompileError::Revert(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_errors_revert_except_counter_overflows() {
        for err in [
            OracleError::SnapshotWriteIndexOverflow,
            OracleError::ScurveWriteIndexOverflow,
            OracleError::PairNotRegistered {
                pair: AddressPair::new_coen_to(840),
            },
            OracleError::NoVwapData,
            OracleError::OnlySystem("activate vote target"),
            OracleError::NoPolicyRate { iso_code: 978 },
        ] {
            let message = err.to_string();
            assert!(
                matches!(PrecompileError::from(err), PrecompileError::Revert(m) if m == message)
            );
        }
    }

    /// The messages are user-visible revert reasons. A reword is an ABI change.
    #[test]
    fn messages_match_the_strings_callers_see() {
        assert_eq!(
            OracleError::PairNotRegistered {
                pair: AddressPair::new_coen_to(840),
            }
            .to_string(),
            format!("pair {} not registered", AddressPair::new_coen_to(840))
        );
        assert_eq!(
            OracleError::VwapOverflow("rate * volume").to_string(),
            "VWAP overflow: rate * volume"
        );
        assert_eq!(
            OracleError::OnlySystem("set exchange rate directly").to_string(),
            "only system can set exchange rate directly"
        );
        assert_eq!(
            OracleError::NotReferenceCurrency { iso_code: 978 }.to_string(),
            "iso_code 978 is not a registered reference currency"
        );
    }
}
