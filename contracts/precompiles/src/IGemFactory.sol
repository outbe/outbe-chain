// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGemFactory {
    error GemNotFound();
    error GemAlreadySettled();
    error GemNotSettled();
    error GemNotCalled();
    error GemNotIssued();
    error SettlementDeadlinePassed();
    error SourceIntexNotFound();
    error SourceIntexNotIssued();
    error GemFactoryPositionNotFound();
    error NotGemFactoryPositionOwner();
    error GemFactoryPositionAlreadyExists();
    error IndexOutOfBounds();
    error InsufficientCapacity();
    error ExpiredGemFactoryPosition();
    error InsufficientProofOfWork();
    error CannotMineInIssueBlock();
    error InvalidCurrency(uint16 currency);
    error OracleUnavailable();
    error InvalidOwner();
    error Overflow();
    error ZeroPromisLoad();
    error EmptyMemberSlotDuringCleanup(bytes32 groupKey);
    error MissingMemberRecordDuringCleanup(bytes32 groupKey, uint256 gemId);
    error MissingGroupForCall(bytes32 group);
    error IneligibleGroupMember(bytes32 groupKey, uint256 gemId);
    error CallPriceOverflow();
    error DateStep(uint8 kind, uint32 dateKey);

    /// @notice Send the caller's Intex units of `intexId` to the Gem Factory (burning
    ///         `units` via IntexNFT1155) and create a GemFactory position owned by the caller.
    ///         Returns the new `gemFactoryPositionId`.
    function createGemFactoryPosition(uint256 intexId, uint256 units) external returns (uint256 gemFactoryPositionId);
    /// @notice Issue one Merchant gem to `owner` in the currencies the merchant names,
    ///         draining the position's capacity. Only the position's owner (the caller)
    ///         may call. `issuanceCurrency` is any ISO 4217 code, `referenceCurrency` an
    ///         oracle reference currency. A merchant issues one owner one Gem of a sub-series
    ///         in each block, so a second one in the same sub-series and block reverts
    ///         `AlreadyExists`.
    function issueGem(
        uint256 gemFactoryPositionId,
        address owner,
        uint256 promisLoadMinor,
        uint16 issuanceCurrency,
        uint16 referenceCurrency
    ) external returns (uint256 gemId);

    /// @notice Settle a gem paying its cost in `asset`. Any caller may pay. The
    ///         gem stays with its owner.
    /// @dev Approve the VaultRouter for `amountCapMinor` before calling.
    ///      Payment is deposited into the Reserve through VaultRouter.
    /// @param asset Settlement asset the gem accepts.
    /// @param snapshotId The trailing VWAP snapshot `quoteSettlement` returned. An
    ///        issuance-currency payment naming any other snapshot reverts. Ignored
    ///        on the reference rail.
    /// @param amountCapMinor The most the payer pays, in `asset` units. A payer whose
    ///        allowance or balance is below it reverts `InsufficientFunds`; a cost above
    ///        it reverts `CostAboveCap`. The call takes only the cost.
    function settleGem(uint256 gemId, address asset, uint256 snapshotId, uint256 amountCapMinor) external;
    /// @notice Burn a settled gem and mint confidential Promis to its owner,
    ///         gated by off-chain proof of work. Any caller may submit. The owner's Promis
    ///         modify key authorizes the call: `mac = HMAC(modifyKey, op-preimage)`.
    ///         `opNonce` MUST equal the owner's current on-chain promis op-nonce (fetch it via
    ///         `outbe_deriveKeys` + `IPromis.opNonceOf`). The bound amount is the gem's load.
    ///         A gem is not mined in its issue block. Returns the minted Promis.
    /// @dev    `nonce` solves SHA256("OUTBE_GEM_MINING_V1" || `gemId` || owner || uint64(0) ||
    ///         `nonce`) to the protocol difficulty. The owner is the gem's, not the caller's.
    function minePromis(uint256 gemId, uint64 nonce, bytes32 mac, uint64 opNonce) external returns (uint256 promisMinor);
    /// @notice Cumulative totals since genesis. `totalCapacityMinor` counts all Promis
    ///         capacity ever sent there. A position that drains or expires does not
    ///         reduce it.
    function getStatistics() external view returns (uint256 totalGemsIssued, uint256 totalCapacityMinor);

    /// @notice What settling `gemId` with `asset` costs, and which of the gem's
    ///         two currencies that asset settles on. Reverts for an asset the
    ///         gem does not accept.
    /// @return settlementCurrency ISO 4217 code the payment is denominated in.
    /// @return paymentMinor Amount to pay, in `asset`'s own minor units.
    /// @return snapshotId Trailing VWAP snapshot the amount converts at. Zero on
    ///         the reference rail. It goes stale at the next update cutoff.
    function quoteSettlement(uint256 gemId, address asset)
        external
        view
        returns (uint16 settlementCurrency, uint256 paymentMinor, uint256 snapshotId);

    // --- GemFactory position: a record with an owner, not a token ---
    // An expired position stays readable until the cleanup deletes it. The positions of an
    // owner are found from `GemFactoryPositionCreated`.
    /// @notice Full terms of the position `gemFactoryPositionId`.
    function getGemFactoryPosition(uint256 gemFactoryPositionId) external view returns (GemFactoryPositionData memory);

    /// @notice How close a right is to its group's call. The window ends at the last
    ///         finalized UTC day. Prices are COEN in `referenceCurrency` at scale 1e6.
    struct CallProgress {
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

    /// @notice Read the call progress of `gemId`. Reverts `GemNotFound`, or
    ///         `GemNotIssued` once it is Called, Settled or Forfeited.
    function getCallProgress(uint256 gemId) external view returns (CallProgress memory);

    /// @notice A merchant's Intex in the Gem Factory: the pool Merchant gems are drawn from.
    struct GemFactoryPositionData {
        uint256 gemFactoryPositionId;
        address owner;
        uint256 sourceIntexId;
        /// @notice The source Intex series, readable after the series is gone.
        bytes14 sourceSeriesId;
        /// @notice 0 once the position expired.
        uint256 remainingCapacityMinor;
        /// @notice When the position was issued.
        uint64 issuedAt;
        /// @notice When the position stops issuing and returns its remainder.
        uint64 expiresAt;
    }

    // --- Events (emitted by the GemFactory precompile) ---
    /// @notice A Gem sub-series was opened by its first Gem: an issue hour at the market price, or a
    ///         higher call price from 24.
    event GemSeriesOpened(
        bytes14 indexed seriesId,
        uint16 indexed subSeriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint256 entryPriceMinor,
        uint256 callPriceMinor,
        uint64 callWindow,
        uint64 callThreshold,
        uint64 callNoticePeriod
    );
    /// @notice A merchant sent Intex units into a new position.
    event GemFactoryPositionCreated(
        uint256 indexed gemFactoryPositionId,
        address indexed owner,
        bytes14 indexed sourceSeriesId,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        bytes payload
    );
    /// @notice The deletion removed an expired position. The capacity its day never issued
    ///         returned before, in one `PromisLimitAdded` of the Promis Limit.
    event GemFactoryPositionExpired(uint256 indexed gemFactoryPositionId, address indexed owner);
}
