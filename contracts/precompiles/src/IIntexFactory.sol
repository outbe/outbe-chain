// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IIntexFactory
/// @notice User-facing call surface for the IntexFactory runtime precompile:
///         settlement, Promis mining, and auction proceeds intake. Issuance is
///         a module-to-module call (Desis -> IntexFactory) exposed through the
///         Rust `api`, not a precompile selector. Series identity + lifecycle
///         live in Intex. This precompile runs settlement, mining and the call and
///         expiry sweeps over the Intex call-price index.
interface IIntexFactory {
    error ZeroAddress();
    error ZeroAmount();
    error ZeroUnits();
    error SettlementDeadlinePassed();
    error ZeroBalance();
    error UnitsExceedBalance();
    error InsufficientSettled();
    error InsufficientProofOfWork();
    error NotOriginRouter();
    error ForfeitedPromisLoadOverflow();
    error NoCalledGroupForTest();
    error IssueForTestCurrencyCountMismatch();
    error BlockTimestampExceedsU32();
    error IssuanceChunkCountExceedsU16();
    error PriceExceedsWireType();
    error MarkedUpPriceOverflow();
    error OwnerUnitsExceedU32();
    error SettledUnitsExceedU32();
    error NftBalanceExceedsU32();
    error NftBalanceOfUndecodable();
    error RouterAnsweredNoLegs();
    error PromisOverflow();
    error MiningSequenceOverflow();
    error ExercisedUnitsExceedSeries();
    error DateStep(uint8 kind, uint32 dateKey);

    /// @notice Settle `units` Issued Intexes of `seriesId` held by
    ///         `owner`, paying the cost in `asset`. Any caller may pay. The
    ///         settled units stay with the owner. Allowed while uncalled (voluntary)
    ///         and once called (forced, until the deadline).
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

    /// @notice Burn auction proceeds (native COEN, sent as msg.value) and credit them to the
    ///         Promis limit pool in whole protocol units. Callable only by the OriginRouter.
    /// @param worldwideDay Worldwide day (yyyymmdd) the proceeds belong to.
    /// @param srcChainId Target chain the proceeds arrived from.
    function distribute(uint32 worldwideDay, uint32 srcChainId) external payable;

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
    event IntexSeriesIssued(
        bytes14 indexed seriesId, uint32 issuedUnits, uint256 entryPriceMinor, uint256 callPriceMinor
    );

    /// @notice `units` Issued Intexes of `seriesId` were settled.
    event IntexSettled(bytes14 indexed seriesId, address indexed owner, uint256 units);

    /// @notice Settled Intexes were burned and `promisMinor` Promis minted.
    event IntexExercised(bytes14 indexed seriesId, address indexed owner, uint256 units, uint256 promisMinor);

    /// @notice The router kept refusing the series' Called notice, so it was dropped
    ///         and the targets were not told of the call.
    event CalledNoticeDropped(bytes14 indexed seriesId, uint32 calledAt);

    /// @notice A reference currency was left out of one day's Call scan because its
    ///         window price could not be indexed. The next day's pass tries it again.
    event CallScanSkipped(uint16 indexed referenceCurrency, uint32 indexed utcDay);

    /// @notice The expiry sweep could not expire a called group and parked it for
    ///         another pass at `retryAt`.
    event ExpiryDeferred(bytes32 indexed groupKey, uint64 retryAt);

    /// @notice `proceedsMinor` native COEN (18 decimals) of `worldwideDay`'s auction proceeds from
    ///         `srcChainId` was burned and `creditedPromisMinor` (6 decimals) was added to the Promis
    ///         limit pool.
    event ProceedsBurned(
        uint32 indexed worldwideDay, uint32 indexed srcChainId, uint256 proceedsMinor, uint256 creditedPromisMinor
    );
}
