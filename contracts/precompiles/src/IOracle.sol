// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IOracle
/// @notice Oracle precompile at 0x000000000000000000000000000000000000EE05
interface IOracle {
    // Events — precompile dispatch (appear in transaction receipts)
    event VoteSubmitted(address indexed validator, uint32 tupleCount);
    event FeederDelegated(address indexed validator, address indexed feeder);
    event VoteTargetDeactivated(string base, string quote);
    event VoteTargetActivated(string base, string quote);
    event ExchangeRateSet(string base, string quote, uint256 rate);

    // Events — block hooks (emitted during tally/slash/S-curve processing)
    event ExchangeRateUpdated(uint32 indexed pairId, uint256 rate, uint64 blockNumber);
    event TallyCompleted(uint64 blockNumber, uint32 pairsUpdated);
    event ValidatorSlashed(address indexed validator, uint64 slashPercent);
    event ValidatorForcedExit(address indexed validator);
    event ScurvePeakDetected(uint32 indexed pairId, uint256 peakPrice, uint64 peakDay);
    /// @notice Emitted once per pair when a closed UTC calendar day's VWAP is
    /// finalized into state. `utcDay` is a yyyymmdd UTC date key (e.g. 20260625).
    event VwapCalculated(uint32 indexed utcDay, uint32 indexed pairId, uint256 vwap);

    struct ExchangeRateTuple {
        string base;
        string quote;
        uint256 exchangeRate;
        uint256 volume;
    }

    /// @notice Returns the current exchange rate for a pair.
    function getExchangeRate(string calldata base, string calldata quote)
        external
        view
        returns (uint256 rate, uint64 lastBlock, uint64 lastTimestamp);

    /// @notice Returns VWAP for a pair over a lookback period in seconds from current block timestamp.
    function getVwap(string calldata base, string calldata quote, uint64 lookbackSeconds)
        external
        view
        returns (uint256 vwap);

    /// @notice Returns VWAP for a pair over an explicit time range.
    function getVwapForTimeRange(string calldata base, string calldata quote, uint64 startTime, uint64 endTime)
        external
        view
        returns (uint256 vwap);

    /// @notice Returns the maximum active S-curve value for a pair at the given timestamp.
    function getScurveValue(string calldata base, string calldata quote, uint64 timestamp)
        external
        view
        returns (uint256 value);

    /// @notice Returns oracle parameters.
    function getParams()
        external
        view
        returns (
            uint64 votePeriod,
            uint256 rewardBand,
            uint64 slashWindow,
            uint256 minValidPerWindow,
            uint256 slashFraction,
            uint64 lookbackDuration,
            bool enabled
        );

    /// @notice Returns vote penalty counters for a validator.
    function getVotePenaltyCounter(address validator)
        external
        view
        returns (uint64 success, uint64 abstain, uint64 miss);

    /// @notice Returns the feeder address delegated by a validator.
    function getFeederDelegation(address validator) external view returns (address feeder);

    /// @notice Returns whether a pair is an active vote target.
    function isVoteTarget(string calldata base, string calldata quote) external view returns (bool);

    /// @notice Returns the number of registered pairs.
    function getPairCount() external view returns (uint32 count);

    /// @notice Returns all pair exchange rates as parallel arrays.
    function getExchangeRates()
        external
        view
        returns (uint256[] memory rates, uint64[] memory blocks, uint64[] memory timestamps);

    /// @notice Returns all active vote target pair_ids.
    function getVoteTargets() external view returns (uint32[] memory pairIds);

    /// @notice Returns the pending aggregate vote for a validator.
    function getAggregateVote(address validator)
        external
        view
        returns (bool exists, uint32[] memory pairIds, uint256[] memory rates, uint256[] memory volumes);

    /// @notice Returns slash window progress for a validator.
    function getSlashWindowProgress(address validator)
        external
        view
        returns (uint64 success, uint64 abstain, uint64 miss, uint64 slashWindow);

    /// @notice Bootstrap write: set exchange rate (system-only, Address::ZERO caller).
    function setExchangeRate(string calldata base, string calldata quote, uint256 rate) external;

    /// @notice Delegate feeder consent from validator to feeder address.
    function delegateFeederConsent(address feeder) external;

    /// @notice Submit aggregate oracle vote.
    function submitVote(ExchangeRateTuple[] calldata tuples) external;

    /// @notice Deactivate a pair's vote target status (system-only).
    function deactivateVoteTarget(string calldata base, string calldata quote) external;

    /// @notice Activate a pair's vote target status (system-only).
    function activateVoteTarget(string calldata base, string calldata quote) external;

    // --- New query functions (ORC-AUD-027) ---

    /// @notice Returns price snapshot history for a pair (most recent first).
    function getPriceSnapshotHistory(string calldata base, string calldata quote, uint32 count)
        external
        view
        returns (uint64[] memory timestamps, uint256[] memory rates, uint256[] memory volumes);

    /// @notice Returns flattened price snapshot history across all pairs (most recent snapshots first).
    function getAllPriceSnapshotHistory(uint32 count)
        external
        view
        returns (
            uint64[] memory snapshotIds,
            uint64[] memory timestamps,
            uint32[] memory pairIds,
            uint256[] memory rates,
            uint256[] memory volumes
        );

    /// @notice Returns TWAP (time-weighted average price) for a pair.
    function getTwap(string calldata base, string calldata quote, uint64 lookbackSeconds)
        external
        view
        returns (uint256 twap);

    /// @notice Returns TWAPs for all active vote-target pairs. The input
    /// `lookback` is the requested lookback window in seconds; the returned
    /// `lookbackSeconds` array reports the lookback actually used per pair.
    function getTwaps(uint64 lookback)
        external
        view
        returns (uint32[] memory pairIds, uint256[] memory twaps, uint64[] memory lookbackSeconds);

    /// @notice Returns VWAP over the last 24 hours for a pair.
    function getDayVwap(string calldata base, string calldata quote) external view returns (uint256 vwap);

    /// @notice Returns the finalized VWAP for a full UTC calendar day.
    /// @param utcDay yyyymmdd UTC date key (e.g. 20260625). Reverts if the day
    ///        is not yet finalized or had no oracle data for the pair. For the
    ///        in-progress current day use `getVwapForTimeRange` instead.
    function getUtcDayVwap(string calldata base, string calldata quote, uint32 utcDay)
        external
        view
        returns (uint256 vwap);

    /// @notice Returns VWAPs for all active vote-target pairs over an explicit WorldwideDay-style window.
    function getWorldwideDayVwap(uint64 startTime, uint64 endTime)
        external
        view
        returns (uint32[] memory pairIds, uint256[] memory vwaps, uint64[] memory lookbackSeconds);

    /// @notice Returns a stored WorldwideDay VWAP snapshot by WWD key.
    function getWorldwideDayVwapSnapshot(uint32 worldwideDay)
        external
        view
        returns (
            uint64 startTime,
            uint64 endTime,
            uint32[] memory pairIds,
            uint256[] memory vwaps,
            uint64[] memory lookbackSeconds
        );

    /// @notice Returns all active S-curve entries for a pair.
    function getScurveEntries(string calldata base, string calldata quote)
        external
        view
        returns (uint64[] memory peakDays, uint256[] memory peakPrices, uint256[] memory currentValues);

    /// @notice Returns S-curve values for a pair at a timestamp.
    function getScurveValues(string calldata base, string calldata quote, uint64 timestamp)
        external
        view
        returns (uint64 targetDay, uint64[] memory peakDays, uint256[] memory peakPrices, uint256[] memory values);

    /// @notice Returns all S-curve data across all pairs.
    function getAllScurveData()
        external
        view
        returns (uint32[] memory pairIds, uint64[] memory peakDays, uint256[] memory peakPrices);

    /// @notice Returns all S-curve data for one pair.
    function getAllScurveDataForPair(string calldata base, string calldata quote)
        external
        view
        returns (uint64[] memory peakDays, uint256[] memory peakPrices);

    /// @notice Returns all registered pairs as parallel arrays of
    ///         (pairId, base, quote, isActive).
    function getPairs()
        external
        view
        returns (uint32[] memory pairIds, string[] memory bases, string[] memory quotes, bool[] memory isActive);

    /// @notice Returns the S-curve adjusted nominal price for a pair at a timestamp.
    function getNominalPrice(string calldata base, string calldata quote, uint64 timestamp)
        external
        view
        returns (uint256 price);

    /// @notice Returns nominal price components where nominal = max(VWAP, S-curve).
    function getNominalPriceComponents(string calldata base, string calldata quote, uint64 timestamp)
        external
        view
        returns (uint256 nominalPrice, uint256 vwap, uint256 maxScurve, string memory source);

    /// @notice Returns the settlement currency pair hash for an ISO 4217 code.
    function getSettlementCurrency(uint16 isoCode) external view returns (bytes32 denomHash, bytes32 pairHash);

    /// @notice Returns all registered settlement currencies.
    function getSettlementCurrencies()
        external
        view
        returns (
            uint16[] memory isoCodes,
            string[] memory denoms,
            bytes32[] memory denomHashes,
            bytes32[] memory pairHashes
        );

    /// @notice Returns the number of registered settlement currencies.
    function getSettlementCount() external view returns (uint32 count);

    /// @notice Returns all registered reference currencies as ISO 4217 numeric codes.
    function getReferenceCurrencies() external view returns (uint16[] memory isoCodes);

    /// @notice Returns the annualized refinancing rate (1e18 scaled) for an ISO
    ///         4217 code. Reverts if the code is not a registered reference
    ///         currency or carries no rate.
    function getRefinancingRate(uint16 isoCode) external view returns (uint256 rate);
}
