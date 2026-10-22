//! Module-local error types for the Oracle.
//!
//! Oracle errors cover pair registration, votes, VWAP/TWAP, and genesis operations.

use crate::precompile::IOracle;
use crate::schema::PairIndex;
use alloy_primitives::Bytes;
use alloy_primitives::U256;
use alloy_sol_types::SolError;
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
    fn from(error: OracleError) -> Self {
        match error {
            OracleError::PairBaseQuoteIdentical => PrecompileError::RevertBytes(Bytes::from(
                IOracle::PairBaseQuoteIdentical {}.abi_encode(),
            )),
            OracleError::PairNotCanonical { pair } => PrecompileError::RevertBytes(Bytes::from(
                IOracle::PairNotCanonical {
                    pairBase: pair.address1(),
                    pairQuote: pair.address2(),
                }
                .abi_encode(),
            )),
            OracleError::PairAlreadyRegistered { pair } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IOracle::PairAlreadyRegistered {
                        pairBase: pair.address1(),
                        pairQuote: pair.address2(),
                    }
                    .abi_encode(),
                ))
            }
            OracleError::PairNotRegistered { pair } => PrecompileError::RevertBytes(Bytes::from(
                IOracle::PairNotRegistered {
                    pairBase: pair.address1(),
                    pairQuote: pair.address2(),
                }
                .abi_encode(),
            )),
            OracleError::PairIndexOutOfRange { index } => PrecompileError::RevertBytes(
                Bytes::from(IOracle::PairIndexOutOfRange { index }.abi_encode()),
            ),
            OracleError::OnlySystem(value) => PrecompileError::RevertBytes(Bytes::from(
                IOracle::OnlySystem {
                    value: value.to_string(),
                }
                .abi_encode(),
            )),
            OracleError::NotActiveOracleSigner => PrecompileError::RevertBytes(Bytes::from(
                IOracle::NotActiveOracleSigner {}.abi_encode(),
            )),
            OracleError::VoteTupleCountExceedsPairCount => PrecompileError::RevertBytes(
                Bytes::from(IOracle::VoteTupleCountExceedsPairCount {}.abi_encode()),
            ),
            OracleError::DuplicatePairInVote => PrecompileError::RevertBytes(Bytes::from(
                IOracle::DuplicatePairInVote {}.abi_encode(),
            )),
            OracleError::PairNotVoteTarget => PrecompileError::RevertBytes(Bytes::from(
                IOracle::PairNotVoteTarget {}.abi_encode(),
            )),
            OracleError::AlreadyVotedThisPeriod => PrecompileError::RevertBytes(Bytes::from(
                IOracle::AlreadyVotedThisPeriod {}.abi_encode(),
            )),
            OracleError::VotePriceExceedsCap { price, max } => PrecompileError::RevertBytes(
                Bytes::from(IOracle::VotePriceExceedsCap { price, max }.abi_encode()),
            ),
            OracleError::VoteVolumeExceedsCap { volume, max } => PrecompileError::RevertBytes(
                Bytes::from(IOracle::VoteVolumeExceedsCap { volume, max }.abi_encode()),
            ),
            OracleError::NoVwapData => {
                PrecompileError::RevertBytes(Bytes::from(IOracle::NoVwapData {}.abi_encode()))
            }
            OracleError::NoTwapData => {
                PrecompileError::RevertBytes(Bytes::from(IOracle::NoTwapData {}.abi_encode()))
            }
            OracleError::MissingTwapData => {
                PrecompileError::RevertBytes(Bytes::from(IOracle::MissingTwapData {}.abi_encode()))
            }
            OracleError::VwapOverflow(reason) => PrecompileError::RevertBytes(Bytes::from(
                IOracle::VwapOverflow {
                    reason: reason.to_string(),
                }
                .abi_encode(),
            )),
            OracleError::TwapOverflow => {
                PrecompileError::RevertBytes(Bytes::from(IOracle::TwapOverflow {}.abi_encode()))
            }
            OracleError::CrossRateOverflow => PrecompileError::RevertBytes(Bytes::from(
                IOracle::CrossRateOverflow {}.abi_encode(),
            )),
            OracleError::TallyArithmeticOverflow(value) => {
                PrecompileError::RevertBytes(Bytes::from(
                    IOracle::TallyArithmeticOverflow {
                        value: value.to_string(),
                    }
                    .abi_encode(),
                ))
            }
            OracleError::StaleCoenRate { iso_code } => PrecompileError::RevertBytes(Bytes::from(
                IOracle::StaleCoenRate { iso_code }.abi_encode(),
            )),
            OracleError::InvalidLookbackSeconds => PrecompileError::RevertBytes(Bytes::from(
                IOracle::InvalidLookbackSeconds {}.abi_encode(),
            )),
            OracleError::InvalidVwapRange => {
                PrecompileError::RevertBytes(Bytes::from(IOracle::InvalidVwapRange {}.abi_encode()))
            }
            OracleError::VwapRangeTooWide => {
                PrecompileError::RevertBytes(Bytes::from(IOracle::VwapRangeTooWide {}.abi_encode()))
            }
            OracleError::WorldwideDayVwapSnapshotNotFound => PrecompileError::RevertBytes(
                Bytes::from(IOracle::WorldwideDayVwapSnapshotNotFound {}.abi_encode()),
            ),
            OracleError::NoFinalizedUtcDayVwap => PrecompileError::RevertBytes(Bytes::from(
                IOracle::NoFinalizedUtcDayVwap {}.abi_encode(),
            )),
            OracleError::InvalidVwapPolicy => PrecompileError::RevertBytes(Bytes::from(
                IOracle::InvalidVwapPolicy {}.abi_encode(),
            )),
            OracleError::InvalidVwapSnapshot => PrecompileError::RevertBytes(Bytes::from(
                IOracle::InvalidVwapSnapshot {}.abi_encode(),
            )),
            OracleError::NotReferenceCurrency { iso_code } => PrecompileError::RevertBytes(
                Bytes::from(IOracle::NotReferenceCurrency { iso_code }.abi_encode()),
            ),
            OracleError::NoPolicyRate { iso_code } => PrecompileError::RevertBytes(Bytes::from(
                IOracle::NoPolicyRate { iso_code }.abi_encode(),
            )),
            OracleError::SlashWindowValidatorSetExceedsCap { actual, cap } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IOracle::SlashWindowValidatorSetExceedsCap {
                        actual: actual as u64,
                        cap: cap as u64,
                    }
                    .abi_encode(),
                ))
            }
            OracleError::VotePeriodZero => {
                PrecompileError::RevertBytes(Bytes::from(IOracle::VotePeriodZero {}.abi_encode()))
            }
            OracleError::SlashWindowZero => {
                PrecompileError::RevertBytes(Bytes::from(IOracle::SlashWindowZero {}.abi_encode()))
            }
            OracleError::LookbackExceedsRetention => PrecompileError::RevertBytes(Bytes::from(
                IOracle::LookbackExceedsRetention {}.abi_encode(),
            )),
            OracleError::RatioAboveOne { field, value } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IOracle::RatioAboveOne {
                        field: field.to_string(),
                        value,
                    }
                    .abi_encode(),
                ))
            }
            OracleError::SlashPercentOutOfRange { slash_fraction } => PrecompileError::RevertBytes(
                Bytes::from(IOracle::SlashPercentOutOfRange { slash_fraction }.abi_encode()),
            ),
            OracleError::PolicyIsoCodeZero => PrecompileError::RevertBytes(Bytes::from(
                IOracle::PolicyIsoCodeZero {}.abi_encode(),
            )),
            OracleError::DuplicatePolicyIsoCode { iso_code } => PrecompileError::RevertBytes(
                Bytes::from(IOracle::DuplicatePolicyIsoCode { iso_code }.abi_encode()),
            ),
            OracleError::PolicyRatesNotSorted => PrecompileError::RevertBytes(Bytes::from(
                IOracle::PolicyRatesNotSorted {}.abi_encode(),
            )),
            OracleError::PolicyRateZero { iso_code } => PrecompileError::RevertBytes(Bytes::from(
                IOracle::PolicyRateZero { iso_code }.abi_encode(),
            )),
            OracleError::MissingPolicyRate { iso_code } => PrecompileError::RevertBytes(
                Bytes::from(IOracle::MissingPolicyRate { iso_code }.abi_encode()),
            ),
            OracleError::AggregateVoteValidatorZero => PrecompileError::RevertBytes(Bytes::from(
                IOracle::AggregateVoteValidatorZero {}.abi_encode(),
            )),
            OracleError::DuplicateAggregateVoteValidator => PrecompileError::RevertBytes(
                Bytes::from(IOracle::DuplicateAggregateVoteValidator {}.abi_encode()),
            ),
            OracleError::AggregateVoteAlreadyExists => PrecompileError::RevertBytes(Bytes::from(
                IOracle::AggregateVoteAlreadyExists {}.abi_encode(),
            )),
            OracleError::AggregateVoteTupleCountExceedsPairCount => PrecompileError::RevertBytes(
                Bytes::from(IOracle::AggregateVoteTupleCountExceedsPairCount {}.abi_encode()),
            ),
            OracleError::AggregateVotePairNotRegistered => PrecompileError::RevertBytes(
                Bytes::from(IOracle::AggregateVotePairNotRegistered {}.abi_encode()),
            ),
            OracleError::DuplicatePairInAggregateVote => PrecompileError::RevertBytes(Bytes::from(
                IOracle::DuplicatePairInAggregateVote {}.abi_encode(),
            )),
            OracleError::AggregateVotePairNotVoteTarget => PrecompileError::RevertBytes(
                Bytes::from(IOracle::AggregateVotePairNotVoteTarget {}.abi_encode()),
            ),
            OracleError::MissingAggregateVoteValidator { voter_index } => {
                PrecompileError::RevertBytes(Bytes::from(
                    IOracle::MissingAggregateVoteValidator { voter_index }.abi_encode(),
                ))
            }
            OracleError::VoterListEntryWithoutVote => PrecompileError::RevertBytes(Bytes::from(
                IOracle::VoterListEntryWithoutVote {}.abi_encode(),
            )),
            OracleError::MissingPairMetadata { pair_id } => PrecompileError::RevertBytes(
                Bytes::from(IOracle::MissingPairMetadata { pair_id }.abi_encode()),
            ),
            OracleError::SnapshotWriteIndexOverflow => PrecompileError::RevertBytes(Bytes::from(
                IOracle::SnapshotWriteIndexOverflow {}.abi_encode(),
            )),
            OracleError::ScurveWriteIndexOverflow => PrecompileError::RevertBytes(Bytes::from(
                IOracle::ScurveWriteIndexOverflow {}.abi_encode(),
            )),
            OracleError::SnapshotOutOfOrder => PrecompileError::RevertBytes(Bytes::from(
                IOracle::SnapshotOutOfOrder {}.abi_encode(),
            )),
            OracleError::NoRateForPair { pair } => PrecompileError::RevertBytes(Bytes::from(
                IOracle::NoRateForPair {
                    pairBase: pair.address1(),
                    pairQuote: pair.address2(),
                }
                .abi_encode(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_sol_types::SolValue;

    /// The caller sees `VwapOverflow(string reason)`. The string is the overflow argument.
    #[test]
    fn vwap_overflow_revert_uses_the_declared_selector() {
        let encoded = match PrecompileError::from(OracleError::VwapOverflow("rate * volume")) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        };
        let selector = alloy_primitives::keccak256(b"VwapOverflow(string)");
        assert_eq!(&encoded[..4], &selector[..4]);
        let text = String::abi_decode(&encoded[4..]).expect("string payload");
        assert_eq!(text, "rate * volume");
    }

    #[test]
    fn oracle_errors_revert_except_counter_overflows() {
        let cases = [
            (
                OracleError::SnapshotWriteIndexOverflow,
                IOracle::SnapshotWriteIndexOverflow::SELECTOR,
            ),
            (
                OracleError::ScurveWriteIndexOverflow,
                IOracle::ScurveWriteIndexOverflow::SELECTOR,
            ),
            (
                OracleError::PairNotRegistered {
                    pair: AddressPair::new_coen_to(840),
                },
                IOracle::PairNotRegistered::SELECTOR,
            ),
            (OracleError::NoVwapData, IOracle::NoVwapData::SELECTOR),
            (
                OracleError::OnlySystem("activate vote target"),
                IOracle::OnlySystem::SELECTOR,
            ),
            (
                OracleError::NoPolicyRate { iso_code: 978 },
                IOracle::NoPolicyRate::SELECTOR,
            ),
        ];
        for (err, selector) in cases {
            match PrecompileError::from(err) {
                PrecompileError::RevertBytes(bytes) => assert_eq!(&bytes[..4], &selector),
                other => panic!("expected typed revert bytes, got {other:?}"),
            }
        }
        let pair = AddressPair::new_coen_to(840);
        let encoded = match PrecompileError::from(OracleError::PairNotRegistered { pair }) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        };
        let decoded = IOracle::PairNotRegistered::abi_decode(&encoded).expect("pair payload");
        assert_eq!(decoded.pairBase, pair.address1());
        assert_eq!(decoded.pairQuote, pair.address2());
        let encoded = match PrecompileError::from(OracleError::OnlySystem("activate vote target")) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        };
        let decoded = IOracle::OnlySystem::abi_decode(&encoded).expect("string payload");
        assert_eq!(decoded.value, "activate vote target");
        let encoded = match PrecompileError::from(OracleError::NoPolicyRate { iso_code: 978 }) {
            PrecompileError::RevertBytes(bytes) => bytes,
            other => panic!("expected typed revert bytes, got {other:?}"),
        };
        let decoded = IOracle::NoPolicyRate::abi_decode(&encoded).expect("code payload");
        assert_eq!(decoded.iso_code, 978);
    }

    /// Display text is for operators. The caller ABI is the Solidity error.
    #[test]
    fn messages_match_the_operator_display() {
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
