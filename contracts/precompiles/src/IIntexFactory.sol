// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IIntexFactory
/// @notice User-facing call surface for the IntexFactory runtime precompile:
///         settlement, Promis mining, and auction proceeds intake. Issuance is
///         a module-to-module call (Desis -> IntexFactory) exposed through the
///         Rust `api`, not a precompile selector. Series identity + lifecycle
///         live in Intex. This precompile owns settlement bookkeeping and the
///         call-price index.
interface IIntexFactory {
    /// @notice Settle `units` Issued Intexes of `seriesId` held by
    ///         `owner`, paying the cost in `asset`. Any caller may pay. The
    ///         settled units stay with the owner. Allowed once qualified (voluntary,
    ///         see `isSeriesQualified`) and once called (forced, until the deadline).
    /// @dev Approve IntexFactory for the `quoteSettlement` amount before calling.
    ///      Payment is deposited into the reserve vault through VaultRouter.
    /// @param asset Token registered with the vault router under either of the
    ///        series' currencies. The issuance currency converts through COEN at
    ///        the trailing VWAP snapshot required at this block.
    /// @param snapshotId The snapshot `quoteSettlement` returned. An issuance-currency
    ///        payment naming any other snapshot reverts. Ignored on the reference rail.
    function settleIntex(bytes14 seriesId, address owner, uint256 units, address asset, uint256 snapshotId) external;

    /// @notice What settling `units` of `seriesId` with `asset` costs,
    ///         and which of the series' two currencies that asset settles on. Priced
    ///         exactly as `settleIntex` charges it. Reverts for an asset the series does
    ///         not accept.
    /// @return settlementCurrency ISO 4217 code the payment is denominated in.
    /// @return paymentMinor Amount to pay, in `asset`'s own minor units.
    /// @return snapshotId Trailing VWAP snapshot the amount converts at. Zero on the
    ///         reference rail. It goes stale at the next update cutoff.
    function quoteSettlement(bytes14 seriesId, address asset, uint256 units)
        external
        view
        returns (uint16 settlementCurrency, uint256 paymentMinor, uint256 snapshotId);

    /// @notice Derived from finalized daily VWAPs on every call, never stored.
    function isSeriesQualified(bytes14 seriesId) external view returns (bool);

    /// @notice The origin's `IVwapSource`: the highest finalized daily VWAP from `fromUtcDay`
    ///         on, 0 when none.
    function maxUtcDayVwapSince(uint16 isoCode, uint32 fromUtcDay) external view returns (uint256);

    /// @notice Burn settled Intexes and mint confidential Promis, gated by
    ///         off-chain proof of work. Any caller may submit. The units burn from
    ///         `owner` and the Promis is minted to them. The owner's Promis modify key
    ///         authorizes the call: `mac = HMAC(modifyKey, op-preimage)`.
    ///         `opNonce` MUST equal the owner's current on-chain promis
    ///         op-nonce (fetch it via `outbe_deriveKeys` + `IPromis.opNonceOf`). The
    ///         bound amount is `promis_load_minor * units`. Returns the minted
    ///         Promis.
    function minePromis(bytes14 seriesId, address owner, uint256 units, uint64 nonce, bytes32 mac, uint64 opNonce)
        external
        returns (uint256 promisMinor);

    /// @notice Leading zero bytes `minePromis` requires of the work hash.
    function powDifficulty() external view returns (uint8);

    /// @notice Burn auction proceeds (native COEN, sent as msg.value) and credit the
    ///         same COEN to the Promis limit pool. Callable only by the OriginRouter.
    /// @param worldwideDay Worldwide day (yyyymmdd) the proceeds belong to.
    /// @param srcChainId Target chain the proceeds arrived from.
    function distribute(uint32 worldwideDay, uint32 srcChainId) external payable;

    /// @notice The classes an issued unit can be in. Disjoint: they sum to
    ///         `issuedUnits`. `activeUnits` is what is still unpaid while the series
    ///         lives, and `forfeitedUnits` is the same remainder once it is Expired.
    ///         `settledUnits` is what is paid and not yet exercised, the same
    ///         measure `IIntex.seriesData` reports.
    struct UnitCounts {
        uint32 issuedUnits;
        uint32 activeUnits;
        uint32 settledUnits;
        uint32 exercisedUnits;
        uint32 gemFactoryUnits;
        uint32 forfeitedUnits;
    }

    /// @notice Read the disjoint unit counts of `seriesId`.
    function seriesUnitCounts(bytes14 seriesId) external view returns (UnitCounts memory);

    /// @notice One owner's units of a series. `issuedUnits` and `settledUnits` are the
    ///         owner's current balances on this, the origin chain, with Issued units counted
    ///         only while the series has not expired. Units held on a target chain are not
    ///         counted. `exercisedUnits` and `gemFactoryUnits` are the owner's history across
    ///         chains, since both happen only here, and do not move with a transfer.
    ///         `ownerUnits` is `issuedUnits + settledUnits`, derived on every call, never stored.
    struct OwnerBalances {
        uint32 issuedUnits;
        uint32 settledUnits;
        uint32 exercisedUnits;
        uint32 gemFactoryUnits;
        uint32 ownerUnits;
    }

    /// @notice Read `owner`'s units of `seriesId`. Reverts if the series does not exist.
    function ownerBalances(bytes14 seriesId, address owner) external view returns (OwnerBalances memory);

    /// @notice A new series was created from a cleared auction.
    event SeriesIssued(bytes14 indexed seriesId, uint32 issuedUnits, uint256 entryPriceMinor);

    /// @notice `units` Issued Intexes of `seriesId` were settled.
    event Settled(bytes14 indexed seriesId, address indexed owner, uint256 units);

    /// @notice Settled Intexes were burned and `promisMinor` Promis minted.
    event PromisMined(bytes14 indexed seriesId, address indexed owner, uint256 units, uint256 promisMinor);

    /// @notice The series was force-called. It settles until `settlementDeadline`, inclusive.
    event SeriesCalled(bytes14 indexed seriesId, uint32 calledAt, uint64 settlementDeadline);

    /// @notice The router kept refusing the series' Called notice, so it was dropped
    ///         and the targets were not told of the call.
    event CalledNoticeDropped(bytes14 indexed seriesId, uint32 calledAt);

    /// @notice A reference currency was left out of one day's Call scan because its
    ///         window price could not be indexed. The next day's pass tries it again.
    event CallScanSkipped(uint16 indexed referenceCurrency, uint32 indexed utcDay);

    /// @notice The daily call sweep (`sweep` = 1) fell two days behind: `skippedDay`
    ///         gave its place to a newer day and will not be walked.
    event SweepDaySkipped(uint8 indexed sweep, uint32 skippedDay, uint32 inFlightDay);

    /// @notice The series' settlement window closed. Both are zero when every unit
    ///         was realized in time.
    event SeriesExpired(bytes14 indexed seriesId, uint32 forfeitedUnits, uint256 returnedPromisMinor);

    /// @notice The expiry sweep left members of a called group unretired and
    ///         parked it for another pass at `retryAt`.
    event ExpiryDeferred(uint16 indexed referenceCurrency, uint32 indexed worldwideDay, uint64 retryAt);

    /// @notice `amount` native COEN of `worldwideDay`'s auction proceeds from `srcChainId`
    ///         was burned and `creditedPromisMinor` was added to the Promis limit pool.
    event ProceedsBurned(
        uint32 indexed worldwideDay, uint32 indexed srcChainId, uint256 amount, uint256 creditedPromisMinor
    );
}
