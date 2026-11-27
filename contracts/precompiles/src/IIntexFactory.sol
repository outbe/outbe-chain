// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @dev The IntexFactory precompile. Matches `INTEX_FACTORY_ADDRESS` in crates/system/common/src/addresses.rs.
address constant INTEX_FACTORY = 0x0000000000000000000000000000000000001015;

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
    error UnauthorizedProceedsCaller(address caller);
    error UnexpectedProceedsSource(uint32 srcChainId);
    error UnauthorizedProceedsSender(bytes from);
    error UndecodableProceedsData();
    error ProceedsTransferRefused();
    error ChainIdExceedsU32();
    error InvalidWireMessage(uint8 msgType);
    error ForfeitedPromisLoadOverflow();
    error IntexSeriesPageCountExceedsU16();
    error PriceExceedsWireType();
    error MarkedUpPriceOverflow();
    error OwnerUnitsExceedU32();
    error SettledUnitsExceedU32();
    error NftBalanceExceedsU32();
    error NftBalanceOfUndecodable();
    error PromisOverflow();
    error MiningSequenceOverflow();
    error MinedUnitsExceedSeries();
    error SeriesNotFound();
    error SeriesNotIssued();
    error DateStep(uint8 kind, uint32 dateKey);

    /// @notice Settle `units` Issued Intexes of `seriesId` held by
    ///         `owner`, paying the cost in `asset`. Any caller may pay. The
    ///         settled units stay with the owner. Allowed while uncalled (voluntary)
    ///         and once called (forced, until the deadline).
    /// @dev Approve the VaultRouter for the `quoteSettlement` amount before calling.
    ///      Payment is deposited into the reserve vault through VaultRouter.
    /// @param asset Token registered with the vault router under either of the
    ///        series' currencies. The issuance currency converts through COEN at
    ///        the trailing VWAP snapshot required at this block.
    /// @param snapshotId The snapshot `quoteSettlement` returned. An issuance-currency
    ///        payment naming any other snapshot reverts. Ignored on the reference rail.
    /// @param amountCapMinor The most the payer accepts to pay, in `asset`'s minor units. The
    ///        payer's allowance and balance must cover it, and a cost above it reverts. Only the
    ///        cost is taken.
    function settleIntex(
        uint256 intexId,
        address owner,
        uint256 units,
        address asset,
        uint256 snapshotId,
        uint256 amountCapMinor
    ) external;

    /// @notice What settling `units` of `intexId` with `asset` costs,
    ///         and which of the series' two currencies that asset settles on. Priced
    ///         exactly as `settleIntex` charges it. Reverts for an asset the series does
    ///         not accept.
    /// @return settlementCurrency ISO 4217 code the payment is denominated in.
    /// @return amountMinor Amount to pay, in `asset`'s own minor units.
    /// @return snapshotId Trailing VWAP snapshot the amount converts at. Zero on the
    ///         reference rail. It goes stale at the next update cutoff.
    function quoteSettlement(uint256 intexId, address asset, uint256 units)
        external
        view
        returns (uint16 settlementCurrency, uint256 amountMinor, uint256 snapshotId);

    /// @notice Burn settled Intexes and mint confidential Promis, gated by
    ///         off-chain proof of work. Any caller may submit. The units burn from
    ///         `owner` and the Promis is minted to them. The owner's Promis modify key
    ///         authorizes the call: `mac = HMAC(modifyKey, op-preimage)`.
    ///         `opNonce` MUST equal the owner's current on-chain promis
    ///         op-nonce (fetch it via `outbe_deriveKeys` + `IPromis.opNonceOf`). The
    ///         bound amount is `promis_load_minor * units`. Returns the minted
    ///         Promis.
    function minePromis(uint256 intexId, address owner, uint256 units, uint64 nonce, bytes32 mac, uint64 opNonce)
        external
        returns (uint256 promisMinor);

    /// @notice Leading zero bytes `minePromis` requires of the work hash.
    function powDifficulty() external view returns (uint8);

    /// @notice The ERC20Router of WCOEN delivers the proceeds of one page of winners from
    ///         IntexAuctionTargetRouter of a chain of the day, with `abi.encode(emissionDay, pageIndex)` in
    ///         `extraData`. The factory burns the WCOEN into the Promis Limit with code 43.
    function onCrosschainTokensReceived(
        uint32 srcChainId,
        bytes calldata from,
        uint256 amount,
        bytes calldata extraData
    ) external returns (bytes4);

    /// @notice IntexAuctionTargetRouter on Outbe hands over the proceeds of one page of winners: the factory
    ///         takes `amount` WCOEN that the router approved and burns it with code 43.
    function receiveLocalProceeds(uint32 emissionDay, uint32 pageIndex, uint256 amount) external;

    /// @notice One owner's units of a series. `issuedUnits` and `settledUnits` are the
    ///         owner's current balances on this, the origin chain, with Issued units counted
    ///         only while the series is not Forfeited. Units held on a target chain are not
    ///         counted. `minedUnits` and `gemFactoryUnits` are the owner's history across
    ///         chains, since both happen only here, and do not move with a transfer.
    ///         `ownerUnits` is `issuedUnits + settledUnits`, derived on every call, never stored.
    struct OwnerBalances {
        uint32 issuedUnits;
        uint32 settledUnits;
        uint32 minedUnits;
        uint32 gemFactoryUnits;
        uint32 ownerUnits;
    }

    /// @notice Read `owner`'s units of `intexId`. Reverts if the series does not exist.
    function ownerBalances(uint256 intexId, address owner) external view returns (OwnerBalances memory);

    /// @notice How close a right is to its group's call. The window ends at the last
    ///         finalized UTC day. Prices are COEN in `referenceCurrency` at scale 1e6.
    struct CallState {
        uint32 callWindowDays;
        uint32 callThresholdDays;
        /// Window days since the group's start, priced or not.
        uint32 daysChecked;
        /// Of them, days whose VWAP was above `callPriceMinor`.
        uint32 daysAboveCallPrice;
        /// The fewest further days above the call price before the call, as the oldest
        /// days leave the window. 0 once the call is decided.
        uint32 minDaysToCall;
        uint16 referenceCurrency;
        uint256 callPriceMinor;
        /// VWAP of the last finalized day, 0 when it has none.
        uint256 lastDayPriceMinor;
    }

    /// @notice Read the call progress of `intexId`. Reverts `SeriesNotFound`, or
    ///         `SeriesNotIssued` once the series is Called or Forfeited.
    function callState(uint256 intexId) external view returns (CallState memory);

    /// @notice The target chains that are not Off, with their modes: 1 Series, 2 Auctions.
    function targetChains() external view returns (uint32[] memory chainIds, uint8[] memory modes);

    /// @notice The chains in Auctions that `emissionDay` started on; empty before its start.
    function targetsOf(uint32 emissionDay) external view returns (uint32[] memory);

    /// @notice Sends the due messages of the outbox to the target chains. Anyone may call it.
    function flush() external;

    /// @notice The first message of the outbox of `chainId`: its type and day, the messages
    ///         that wait for that chain, the failed tries of the first one and its next try.
    function outboxHead(uint32 chainId)
        external
        view
        returns (uint8 msgType, uint32 emissionDay, uint32 pending, uint32 failures, uint64 nextTry);

    /// @notice The series was force-called. It settles until `settlementDeadline`, inclusive.
    event IntexSeriesCalled(
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint64 calledAt,
        uint64 settlementDeadline
    );

    /// @notice A new series was created from a cleared auction.
    event IntexSeriesOpened(
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint256 entryPriceMinor,
        uint256 callPriceMinor,
        uint64 callWindow,
        uint64 callThreshold,
        uint64 callNoticePeriod,
        uint64 transitMargin,
        uint32 units,
        uint256 unitPromisLoadMinor
    );

    /// @notice The series settlement window closed. Both are zero when every unit was realized in time.
    event IntexSeriesClosed(
        bytes14 indexed seriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint32 forfeitedCount,
        uint256 forfeitedAmountMinor
    );

    /// @notice `sendId` is the Hyperlane message id that CrosschainBridge returned.
    event IntexSeriesSent(
        bytes32 indexed sendId,
        uint32 indexed emissionDay,
        uint32 indexed dstChainId,
        uint32 pageIndex,
        uint32 totalPages,
        uint32 seriesCount
    );
    event IntexSeriesCallNoticeSent(bytes32 indexed sendId, bytes14 indexed seriesId, uint32 indexed dstChainId);

    /// @notice A try to send the message failed; `failures` counts the failed tries in a row.
    event IntexSeriesSendFailed(
        uint32 indexed emissionDay, uint32 indexed dstChainId, uint32 pageIndex, uint32 failures, bytes errorData
    );
    event IntexSeriesCallNoticeSendFailed(
        bytes14 indexed seriesId, uint32 indexed dstChainId, uint32 failures, bytes errorData
    );

    /// @notice `amountMinor` native COEN (18 decimals) of `emissionDay`'s auction proceeds from
    ///         `srcChainId` was burned.
    event IntexAuctionProceedsBurned(
        uint32 indexed emissionDay, uint32 indexed srcChainId, uint32 pageIndex, uint256 amountMinor
    );
}
