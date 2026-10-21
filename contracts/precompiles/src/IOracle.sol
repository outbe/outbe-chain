// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IOracle
/// @notice Oracle precompile at 0x000000000000000000000000000000000000EE05
interface IOracle {
    // Events - precompile dispatch (appear in transaction receipts)
    event VoteSubmitted(address indexed validator, uint32 tupleCount);
    event FeederDelegated(address indexed validator, address indexed feeder);
    event VoteTargetDeactivated(address indexed base, address indexed quote);
    event VoteTargetActivated(address indexed base, address indexed quote);
    event ExchangeRateSet(address indexed base, address indexed quote, uint256 rate);

    // Events - block hooks (emitted during tally/slash/S-curve processing)
    event ExchangeRateUpdated(address indexed base, address indexed quote, uint256 rate, uint64 blockNumber);
    event TallyCompleted(uint64 blockNumber, uint32 pairsUpdated);
    event ValidatorSlashed(address indexed validator, uint64 slashPercent);
    event ValidatorForcedExit(address indexed validator);
    /// @dev `peakDay` is the UTC midnight of the peak, in Unix seconds.
    ///      It is not a yyyymmdd date key.
    event ScurvePeakDetected(address indexed base, address indexed quote, uint256 peakPrice, uint64 peakDay);
    /// @notice Emitted once per pair when a closed UTC calendar day's VWAP is
    /// finalized into state. `utcDay` is a yyyymmdd UTC date key (e.g. 20260625).
    /// @dev Uses all three indexable topics. No further field can be indexed.
    event VwapCalculated(uint32 indexed utcDay, address indexed base, address indexed quote, uint256 vwap);

    /// @notice One quoted rate in an aggregate vote.
    /// @dev `base` and `quote` must match the direction the pair was registered
    ///      in. The storage key is order-independent, so a flipped quote would
    ///      otherwise submit an uninverted rate for the same pair. A flipped quote
    ///      reverts.
    ///      COEN/ISO rates and COEN volumes use six decimals. Generic pairs keep
    ///      their existing decimal18 contract.
    struct ExchangeRateTuple {
        address base;
        address quote;
        uint256 exchangeRate;
        uint256 volume;
    }

    /// @notice Returns the current exchange rate for a market, quoted in the
    ///         caller's direction.
    /// @dev Generic pairs preserve their registered orientation. COEN/ISO uses
    ///      COEN as base. Quoting a market backwards returns `scale^2 / rate`:
    ///      `1e12 / rate` for COEN/ISO and `1e36 / rate` for generic pairs. An
    ///      unpublished rate is `0` from either side. Reverts
    ///      if the market is not registered. Unlike the other pair-scoped reads,
    ///      this one accepts either direction, because a spot rate is the only value
    ///      here that has a well-defined inverse.
    function getExchangeRate(address base, address quote) external view returns (uint256 rate);

    /// @notice `getExchangeRate` for `COEN/<isoCode>`. COEN is the zero address
    ///         and so always sorts first: this is never the inverted direction.
    ///         Every ISO reference currency uses the six-decimal COEN/ISO contract.
    function getCoenExchangeRateFor(uint16 isoCode) external view returns (uint256 rate);

    /// @notice `amount`, denominated in `fromIso`, re-expressed in `toIso` via
    ///         both COEN legs: `amount * rate(COEN/toIso) / rate(COEN/fromIso)`,
    ///         rounded up. Equal currencies return `amount` unchanged.
    /// @dev Reverts when either leg has no registered pair or no published rate.
    function currencyCrossRate(uint16 fromIso, uint16 toIso, uint256 amount) external view returns (uint256 converted);

    /// @notice `getExchangeRate` plus when the rate was last written. The block
    ///         and timestamp describe the stored observation and are the same
    ///         whichever direction the market is quoted in.
    function getExchangeRateData(address base, address quote)
        external
        view
        returns (uint256 rate, uint64 lastBlock, uint64 lastTimestamp);

    /// @notice Returns VWAP in the pair's registered scale over a lookback period.
    function getVwap(address base, address quote, uint64 lookbackSeconds) external view returns (uint256 vwap);

    /// @notice Returns VWAP in the pair's registered scale for an explicit range.
    /// @dev Reverts when the range is wider than the `lookbackDuration` parameter.
    ///      Use `getUtcDayVwap` for whole past days.
    function getVwapForTimeRange(address base, address quote, uint64 startTime, uint64 endTime)
        external
        view
        returns (uint256 vwap);

    /// @notice Returns the maximum active S-curve value at `timestamp`.
    /// @dev A COEN/ISO S-curve value uses 6 decimals.
    ///      The coefficient table is the COEN/840 six-decimal table.
    function getScurveValue(address base, address quote, uint64 timestamp) external view returns (uint256 value);

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
    function isVoteTarget(address base, address quote) external view returns (bool);

    /// @notice Returns the number of registered pairs.
    function getPairCount() external view returns (uint32 count);

    /// @notice The pair at a 1-based registry index, in registered orientation,
    ///         with the decimals each side is quoted in.
    /// @dev Together with `getPairCount` this is how the whole registry is
    ///      enumerated. Reverts outside `1..getPairCount()`.
    function getPairByIndex(uint32 index)
        external
        view
        returns (address base, address quote, uint8 baseScale, uint8 quoteScale);

    /// @notice Returns all active vote target pairs.
    function getVoteTargets() external view returns (address[] memory bases, address[] memory quotes);

    /// @notice Returns the mainnet feeder sources TOML for the active protocol
    ///         version. Returns an empty string on other networks.
    /// @dev The node binary holds the files. Each file applies from the
    ///      protocol version that introduces it.
    function getSources() external view returns (string memory toml);

    /// @notice Returns the pending aggregate vote for a validator.
    function getAggregateVote(address validator)
        external
        view
        returns (
            bool exists,
            address[] memory bases,
            address[] memory quotes,
            uint256[] memory rates,
            uint256[] memory volumes
        );

    /// @notice Returns slash window progress for a validator.
    function getSlashWindowProgress(address validator)
        external
        view
        returns (uint64 success, uint64 abstain, uint64 miss, uint64 slashWindow);

    // todo delete?
    /// @notice Bootstrap write: set exchange rate (system-only, Address::ZERO caller).
    function setExchangeRate(address base, address quote, uint256 rate) external;

    /// @notice Delegate feeder consent from validator to feeder address.
    function delegateFeederConsent(address feeder) external;

    /// @notice Submit aggregate oracle vote.
    function submitVote(ExchangeRateTuple[] calldata tuples) external;

    /// @notice Deactivate a pair's vote target status (system-only).
    function deactivateVoteTarget(address base, address quote) external;

    /// @notice Activate a pair's vote target status (system-only).
    function activateVoteTarget(address base, address quote) external;

    // --- New query functions (ORC-AUD-027) ---

    /// @notice Returns price snapshot history for a pair (most recent first).
    function getPriceSnapshotHistory(address base, address quote, uint32 count)
        external
        view
        returns (uint64[] memory timestamps, uint256[] memory rates, uint256[] memory volumes);

    /// @notice Returns flattened price snapshot history across all pairs
    ///         (most recent snapshots first).
    function getAllPriceSnapshotHistory(uint32 count)
        external
        view
        returns (
            uint64[] memory snapshotIds,
            uint64[] memory timestamps,
            address[] memory bases,
            address[] memory quotes,
            uint256[] memory rates,
            uint256[] memory volumes
        );

    /// @notice Returns the TWAP for a pair in that pair's registered scale.
    /// @dev COEN/ISO uses 6 decimals. A generic pair uses 18 decimals.
    ///      This read does not rescale a stored rate.
    function getTwap(address base, address quote, uint64 lookbackSeconds) external view returns (uint256 twap);

    /// @notice Returns one TWAP per active vote-target pair.
    /// @dev Each value uses that pair's registered scale.
    ///      COEN/ISO uses 6 decimals. A generic pair uses 18 decimals.
    ///      `lookback` is the requested window in seconds.
    ///      `lookbackSeconds` is the window this call used for that pair.
    function getTwaps(uint64 lookback)
        external
        view
        returns (
            address[] memory bases,
            address[] memory quotes,
            uint256[] memory twaps,
            uint64[] memory lookbackSeconds
        );

    /// @notice Returns the VWAP for the last 24 hours for one pair.
    /// @dev The window rolls. It is not a UTC calendar day and not a WorldwideDay.
    ///      The value uses the pair's registered scale.
    ///      COEN/ISO uses 6 decimals. A generic pair uses 18 decimals.
    function getDayVwap(address base, address quote) external view returns (uint256 vwap);

    /// @notice Returns the trailing VWAP policy pinned by the protocol constants:
    ///         snapshots cover `[cutoff - vwapLookbackSeconds, cutoff)` with a
    ///         cutoff every `vwapUpdateIntervalSeconds` from the UTC epoch.
    function getVwapPolicy()
        external
        view
        returns (uint32 policyVersion, uint64 vwapLookbackSeconds, uint64 vwapUpdateIntervalSeconds);

    /// @notice Returns the trailing VWAP snapshot required at the current block.
    /// @dev Layout: `policyVersion << 128 | vwapLookbackSeconds << 96 |
    ///      vwapUpdateIntervalSeconds << 64 | cutoff`. The window starts at
    ///      `cutoff - vwapLookbackSeconds`.
    function getVwapSnapshotId() external view returns (uint256 snapshotId);

    /// @notice Returns the finalized COEN/`currency` VWAP over a trailing snapshot
    ///         window, at six decimals.
    /// @dev Any well-formed closed snapshot is readable. Compare `snapshotId` with
    ///      `getVwapSnapshotId()` to price against the current one. Reverts for:
    ///      - an unregistered currency
    ///      - a malformed or open snapshot
    ///      - a window without a positive price
    ///      - a window with too little coverage
    ///      An hour counts only when its snapshots cover two thirds of the tally
    ///      rounds its blocks allowed. Hours that do not count are left out of the
    ///      VWAP. The hours that count must together cover two thirds of the whole
    ///      window's rounds.
    function getFinalizedWindowVwap(uint16 currency, uint256 snapshotId) external view returns (uint256 vwap);

    /// @notice Returns the finalized VWAP for one UTC calendar day.
    /// @param utcDay yyyymmdd UTC date key (e.g. 20260625). Reverts if the day
    ///        is not yet finalized or had no oracle data for the pair. For the
    ///        in-progress current day use `getVwapForTimeRange` instead.
    /// @return vwap Finalized VWAP in the pair's registered scale.
    /// @dev `utcDay` is a UTC date. It is not a WorldwideDay.
    ///      A WorldwideDay key uses UTC+14.
    ///      The UTC day starts at 00:00 UTC and lasts 86400 seconds.
    ///      COEN/ISO uses 6 decimals. A generic pair uses 18 decimals.
    function getUtcDayVwap(address base, address quote, uint32 utcDay) external view returns (uint256 vwap);

    /// @notice Returns VWAPs for all active vote-target pairs over an explicit range.
    /// @dev `startTime` and `endTime` are Unix seconds.
    ///      A WorldwideDay key is a separate yyyymmdd value at UTC+14.
    ///      Reverts when the range is wider than the `lookbackDuration` parameter.
    function getWorldwideDayVwap(uint64 startTime, uint64 endTime)
        external
        view
        returns (
            address[] memory bases,
            address[] memory quotes,
            uint256[] memory vwaps,
            uint64[] memory lookbackSeconds
        );

    /// @notice Returns one stored WorldwideDay VWAP snapshot.
    /// @param worldwideDay WorldwideDay key in yyyymmdd form.
    /// @dev A WorldwideDay uses UTC+14. It is not a UTC calendar day.
    function getWorldwideDayVwapSnapshot(uint32 worldwideDay)
        external
        view
        returns (
            uint64 startTime,
            uint64 endTime,
            address[] memory bases,
            address[] memory quotes,
            uint256[] memory vwaps,
            uint64[] memory lookbackSeconds
        );

    /// @notice Returns the active S-curve entry for a pair.
    /// @dev Each `peakDay` is the UTC midnight of that peak, in Unix seconds.
    ///      It is not a yyyymmdd date key.
    ///      A COEN/ISO peak price and current value use 6 decimals.
    function getScurveEntries(address base, address quote)
        external
        view
        returns (uint64[] memory peakDays, uint256[] memory peakPrices, uint256[] memory currentValues);

    /// @notice Returns S-curve values for a pair at `timestamp`.
    /// @return targetDay UTC midnight of `timestamp`, in Unix seconds.
    /// @dev Each `peakDay` uses that same UTC-midnight unit.
    ///      A COEN/ISO price and value use 6 decimals.
    function getScurveValues(address base, address quote, uint64 timestamp)
        external
        view
        returns (uint64 targetDay, uint64[] memory peakDays, uint256[] memory peakPrices, uint256[] memory values);

    /// @notice Returns the S-curve peak for every stored pair.
    /// @dev Each `peakDay` is a UTC midnight in Unix seconds.
    ///      It is not a yyyymmdd date key.
    ///      A COEN/ISO peak price uses 6 decimals.
    function getAllScurveData()
        external
        view
        returns (address[] memory bases, address[] memory quotes, uint64[] memory peakDays, uint256[] memory peakPrices);

    /// @notice Returns the stored S-curve peak for one pair.
    /// @dev Each `peakDay` is a UTC midnight in Unix seconds.
    ///      It is not a yyyymmdd date key.
    ///      A COEN/ISO peak price uses 6 decimals.
    function getAllScurveDataForPair(address base, address quote)
        external
        view
        returns (uint64[] memory peakDays, uint256[] memory peakPrices);

    /// @notice Returns the nominal price for a pair at `timestamp`.
    /// @dev Nominal price is the larger of that UTC day's VWAP and the S-curve value.
    ///      VWAP uses the pair's registered scale.
    ///      COEN/ISO VWAP uses 6 decimals. A generic pair VWAP uses 18 decimals.
    ///      A COEN/ISO S-curve value uses 6 decimals.
    ///      Policy rate uses a separate 1e6 scale. It is not this price.
    function getNominalPrice(address base, address quote, uint64 timestamp) external view returns (uint256 price);

    /// @notice Returns the parts of the nominal price at `timestamp`.
    /// @dev Nominal price is the larger of `vwap` and `maxScurve`.
    ///      `vwap` uses the pair's registered scale.
    ///      COEN/ISO VWAP uses 6 decimals. A generic pair VWAP uses 18 decimals.
    ///      A COEN/ISO S-curve value uses 6 decimals.
    ///      Policy rate uses a separate 1e6 scale. It is not `maxScurve`.
    /// @return nominalPrice The larger of `vwap` and `maxScurve`.
    /// @return vwap VWAP for the UTC day that contains `timestamp`.
    /// @return maxScurve Active S-curve value. A COEN/ISO value uses 6 decimals.
    function getNominalPriceComponents(address base, address quote, uint64 timestamp)
        external
        view
        returns (uint256 nominalPrice, uint256 vwap, uint256 maxScurve, string memory source);

    /// @notice Returns all registered reference currencies as ISO 4217 numeric codes.
    function getReferenceCurrencies() external view returns (uint16[] memory isoCodes);

    /// @notice Returns every ISO code with an independently configured policy rate.
    function getPolicyRateCurrencies() external view returns (uint16[] memory isoCodes);

    /// @notice Returns the annualized policy rate (scale 1e6) for an ISO 4217
    ///         code. Reverts when no policy rate is configured for the code.
    function getPolicyRate(uint16 isoCode) external view returns (uint256 rate);
}
