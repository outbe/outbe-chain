// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/**
 * @title IntexAuction Contract Interfaces
 * @author Outbe
 * @notice Public API, events, errors, and data types for `IntexAuction`.
 * @dev All auctions are keyed by `emissionDay` (uint32, yyyymmdd).
 */
interface IIntexAuction {
    // --- Types ---

    /// @notice Auction lifecycle stages.
    enum AuctionStage {
        CommittingBids,
        RevealingBids,
        AwaitingResult,
        Completed,
        Canceled,
        Failed
    }

    /// @notice Emission-day state: `Live` from `startAuction`, `Canceled` from `cancelAuction`.
    /// @dev `Unknown` is a day that neither message reached.
    enum DayState {
        Unknown,
        Live,
        Canceled
    }

    /// @notice Revealed bid payload. Slot-packed: slot 0 holds
    ///         `bidder` (20B) + `intexBidRate` (4B) + `intexQuantity` (2B) + `timestamp` (4B) = 30B.
    struct SubmittedIntexAuctionBidData {
        /// @notice Bidder IBA address.
        address bidder;
        /// @notice Bid rate the bidder accepts (`1e6` fixed-point, % of the escrow basis).
        uint32 intexBidRate;
        /// @notice Requested quantity (Intex units).
        uint16 intexQuantity;
        /// @notice Timestamp assigned at reveal (ordering only).
        uint32 timestamp;
        /// @notice Declared issuance currency (ISO numeric).
        uint16 issuanceCurrency;
        /// @notice Reference currency the bid prices in (ISO numeric).
        uint16 referenceCurrency;
    }

    /// @notice Auction schedule - stage-end timestamps.
    /// @dev Computed on the Outbe side (Desis) and passed into `startAuction`.
    struct AuctionSchedule {
        /// @notice End of the commit stage (UNIX seconds).
        uint32 commitEnd;
        /// @notice End of the reveal stage (UNIX seconds).
        uint32 revealEnd;
        /// @notice End of the issuance stage (UNIX seconds).
        uint32 issuanceEnd;
    }

    /// @notice Call-trigger parameters governing when a series is forced into Called.
    struct IntexCallTrigger {
        /// @notice Call-trigger observation window in seconds.
        uint32 callWindow;
        /// @notice Call-trigger threshold in seconds.
        uint32 callThreshold;
        /// @notice Called->deadline window in seconds. Stored verbatim. The issuer must supply a
        ///         non-zero value.
        uint32 callNoticePeriod;
    }

    /// @notice Entry and call price of one reference currency for a day.
    struct ReferenceCurrencyPrice {
        uint16 isoCode;
        uint64 entryPriceMinor;
        uint64 callPriceMinor;
    }

    /// @notice Auction input parameters, stored per auction.
    struct AuctionParams {
        /// @notice PROMIS-units per Intex unit (1e6).
        uint128 promisLoadMinor;
        /// @notice Call-trigger parameters (window/threshold/period).
        IntexCallTrigger callTrigger;
        /// @notice Minimum allowed bid rate (`1e6` fixed-point, % of the escrow basis). Reveal
        ///         rejects bids below it.
        uint32 minBidRate;
        /// @notice Minimum quantity per bid (Intex units).
        uint16 minBidQuantity;
        /// @notice One row per currency the day can clear in. The bid's reference
        ///         currency must appear here.
        ReferenceCurrencyPrice[] prices;
        /// @notice Entry bond in protocol units (1e6), scaled to the payment token by the escrow.
        ///         Taken at `commitIntexAuctionBid` and returned on reveal/cancel. 0 disables the bond.
        uint128 bidBondMinor;
    }

    /// @notice Auction results and statistics (final, set at clearing).
    struct IntexAuctionResult {
        /// @notice Uniform auction clearing rate (`1e6` fixed-point) used to issue Intex.
        uint64 clearingRate;
        /// @notice Number of winning bids (provided by Outbe).
        uint32 wonBidsCount;
        /// @notice Number of Intex units issued.
        uint32 issuedUnits;
        /// @notice Total Promis loaded into the issued Intex (`issuedUnits * promisLoadMinor`).
        ///         Derived on-chain at clearing.
        uint128 issuedPromisLoadMinor;
    }

    /// @notice Live bid counters tracked while the auction runs.
    struct AuctionRunningCounts {
        uint32 committedBidsCount;
        uint32 revealedBidsCount;
    }

    /// @notice Auction parameters and state, keyed by `emissionDay`.
    struct AuctionData {
        DayState dayState;
        AuctionSchedule schedule;
        AuctionParams params;
        IntexAuctionResult result;
    }

    // --- Events ---

    /// @notice Emitted when the auction of the day starts on this chain, with its windows and terms.
    event IntexAuctionStarted(
        uint32 indexed emissionDay,
        uint64 commitEnd,
        uint64 revealEnd,
        uint64 issuanceEnd,
        uint256 unitPromisLoadMinor,
        uint32 minBidRate,
        uint32 minBidQuantity,
        uint256 bidBondMinor
    );

    /// @notice Emitted when Outbe says that the day is canceled.
    /// @param reason The reason of `DesisCanceled` on Outbe.
    event IntexAuctionCanceled(uint32 indexed emissionDay, uint8 reason);

    /// @notice Emitted when the clearing start first arrives and the reveal ends on this chain.
    event IntexAuctionClearingStarted(uint32 indexed emissionDay);

    /// @notice Emitted when the result of Outbe completes the auction on this chain: the won quantity and
    ///         winning bids of this chain, and the totals of the day. With no sale, the counts are 0.
    event IntexAuctionCompleted(
        uint32 indexed emissionDay,
        uint32 wonQuantity,
        uint32 wonBidsCount,
        uint32 quantity,
        uint64 clearingRate,
        uint64 totalDemand
    );

    /// @notice Emitted when a bidder commits a bid.
    event IntexAuctionBidCommitted(uint32 indexed emissionDay, address indexed bidder, bytes32 commitHash);

    /// @notice Emitted when a bidder reveals a bid.
    event IntexAuctionBidRevealed(
        uint32 indexed emissionDay,
        address indexed bidder,
        uint32 quantity,
        uint32 bidRate,
        uint16 issuanceCurrency,
        uint16 referenceCurrency
    );

    /// @notice Emitted when a bidder cancels its commit in the commit stage.
    event IntexAuctionBidCanceled(uint32 indexed emissionDay, address indexed bidder);

    /// @notice Emitted when an admin sets the escrow.
    event EscrowSet(address escrow);

    /// @notice Emitted when an admin sets the Intex contract.
    event IntexContractSet(address intexContract);

    /// @notice Emitted when a redeem issues the units of a WinNote.
    event IntexAuctionWinNoteRedeemed(uint32 indexed emissionDay, address indexed bidder, bytes14 indexed seriesId);

    /// @notice Emitted when a redeem finds the series of the WinNote forfeited: no Intex is issued.
    event IntexAuctionWinNoteExpired(uint32 indexed emissionDay, address indexed bidder, bytes14 indexed seriesId);

    // --- Errors ---

    /// @notice Zero address provided.
    /// @param f Field name.
    error ZeroAddress(string f);
    /// @notice Zero value provided where non-zero is required.
    /// @param f Field name.
    error ZeroValue(string f);
    /// @notice Operation requires a different stage.
    error StageRequired(AuctionStage requiredStage, AuctionStage currentStage);
    /// @notice Commit already registered for this bidder in this auction.
    error IntexAuctionBidAlreadyCommitted();
    /// @notice Commit not found for this bidder in this auction.
    error IntexAuctionBidNotFound();
    /// @notice Bid already revealed for this bidder in this auction.
    error IntexAuctionBidAlreadyRevealed();
    /// @notice Reveal payload does not match the commit hash.
    error RevealHashMismatch();
    /// @notice Bid rate is below `minBidRate`.
    error IntexAuctionBidBelowMinBidRate();
    /// @notice Bid rate exceeds 100% of the escrow basis (scale `1e6`).
    error IntexAuctionBidRateAboveMax(uint32 bidRate);
    /// @notice Bid quantity is below `minBidQuantity`.
    error IntexAuctionBidBelowMinBidQuantity();
    /// @notice `issuedUnits * promisLoadMinor` exceeds the uint128 loaded-Promis range.
    error IssuedPromisOverflow(uint32 issuedUnits, uint128 promisLoadMinor);
    /// @notice Auction does not exist.
    error AuctionNotFound();
    /// @notice Auction already exists.
    error AuctionAlreadyExists();
    /// @notice Clearing result claims more winners than were revealed on-chain.
    error WonIntexAuctionBidsExceedRevealed(uint32 wonBidsCount, uint32 revealedBidsCount);
    /// @notice Clearing rate is below the configured minimum bid rate.
    error ClearingRateBelowMin(uint64 clearingRate, uint32 minBidRate);
    /// @notice Schedule timestamps are not strictly increasing or are in the past.
    error InvalidSchedule();
    /// @notice The day is cancelled.
    error AuctionCanceled();
    /// @notice The day does not carry a price for this reference currency.
    error ReferenceCurrencyNotPriced(uint32 emissionDay, uint16 isoCode);
    /// @notice The declared issuance currency is not an ISO 4217 code.
    error InvalidIssuanceCurrency(uint16 isoCode);
    /// @notice Commit hash must be non-zero.
    error InvalidCommitHash();
    /// @notice Chain id mismatch between the caller-supplied value and `block.chainid`.
    error WrongChain(uint256 expected, uint256 got);
    /// @notice The current escrow still holds something its bidders can claim.
    error EscrowHasLiveLocks();
    /// @notice The collection holds no series for the bid's day and currency pair yet.
    error SeriesNotCreated(uint32 emissionDay, uint16 issuanceCurrency, uint16 referenceCurrency);
    /// @notice `unlockIntexAuctionBidBond` was called before the no-reveal penalty window elapsed.
    /// @param unlockableAt Earliest unix-seconds timestamp the bond can be unlocked at.
    /// @param nowTs Current block timestamp.
    error IntexAuctionBidBondNotYetUnlockable(uint32 unlockableAt, uint32 nowTs);

    // --- Admin ---

    /// @notice Wire contract dependencies.
    /// @dev Bidders claim through the auction from the escrow that took their locks, so the escrow
    ///      changes only once it has nothing left to claim (`hasOutstandingLocks` is false). Every
    ///      claim is permissionless, so anyone can bring it there.
    /// @param _escrow Escrow contract address.
    function setEscrow(address _escrow) external;

    /// @notice Set the Intex collection winners' units are issued on. It changes only once the
    ///         escrow has nothing left to claim, like `setEscrow`.
    /// @param _intexContract Collection address.
    function setIntexContract(address _intexContract) external;

    // --- Lifecycle ---

    /// @notice Create and start a new auction for `emissionDay`.
    /// @dev The schedule (`commitEnd`/`revealEnd`/`issuanceEnd`) is computed on the
    ///      Outbe side (Desis) and passed in. Stage transitions follow the stored timestamps.
    ///      A day that a cancel reached first stays cancelled (`AuctionCanceled`).
    /// @param emissionDay Emission day (yyyymmdd, uint32).
    /// @param schedule Stage-end timestamps.
    /// @param params Auction input parameters.
    function startAuction(uint32 emissionDay, AuctionSchedule calldata schedule, AuctionParams calldata params) external;

    /// @notice Cancel the auction of `emissionDay` at any stage before its result.
    /// @dev A day this chain has not started yet is recorded as cancelled, so its later start
    ///      fails. A cleared or failed day reverts `StageRequired`, and a cancelled one
    ///      `AuctionCanceled`.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param reason Reason code of the cancel on Outbe.
    function cancelAuction(uint32 emissionDay, uint8 reason) external;

    /// @notice Move the auction to `AwaitingResult` (the bridge-driven signal from Outbe that it collects the bids).
    /// @dev Early signal snaps `revealEnd` forward; `issuanceEnd` is unchanged.
    /// @param emissionDay Emission day (yyyymmdd).
    function startAuctionClearing(uint32 emissionDay) external;

    /// @notice Complete the auction with the result of Outbe.
    /// @dev `issuedPromisLoadMinor` is derived on-chain (`quantity * promisLoadMinor`).
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param wonQuantity Intex quantity this chain's winners won.
    /// @param wonBidsCount Winning bids of this chain.
    /// @param quantity Intex quantity the day issued on all chains.
    /// @param clearingRate Uniform clearing rate (`1e6` fixed-point) calculated by Outbe.
    /// @param totalDemand Intex quantity all revealed bids asked for.
    function completeAuction(
        uint32 emissionDay,
        uint32 wonQuantity,
        uint32 wonBidsCount,
        uint32 quantity,
        uint64 clearingRate,
        uint64 totalDemand
    ) external;

    // --- User Actions ---

    /// @notice Commit a sealed bid hash for an auction.
    /// @dev When the series carries a non-zero `bidBondMinor`, the bond moves from the caller
    ///      into escrow in the same transaction. This requires prior payment-token
    ///      approval on the escrow adapter. Reveal/cancel return it immediately. A live-day
    ///      no-reveal locks it until `revealEnd + UNREVEALED_BOND_LOCK_PERIOD` (see `unlockIntexAuctionBidBond`).
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param commitHash `keccak256(signature)`, where `signature` is an EIP-712 typed-data
    ///                   signature over `RevealBid(uint32 emissionDay,address bidder,uint16 quantity,uint32 bidRate,uint16 issuanceCurrency,uint16 referenceCurrency)`
    ///                   under the `IntexAuction` v1 domain (`chainId`, `verifyingContract = address(this)`).
    function commitIntexAuctionBid(uint32 emissionDay, bytes32 commitHash) external;

    /// @notice Cancel an existing commit during the commit stage.
    /// @dev Only callable before `commitEnd`. Once the commit window closes a commit can no longer
    ///      be cancelled or revealed. An unrevealed commit is permanently forfeited (its bond
    ///      stays claimable via `unlockIntexAuctionBidBond`). Cancelling returns the bond immediately.
    /// @param emissionDay Emission day (yyyymmdd).
    function cancelIntexAuctionBid(uint32 emissionDay) external;

    /// @notice Reveal a bid.
    /// @dev Returns the commit bond (if any) before locking the bid escrow, so the bond can fund
    ///      the bid in the same transaction.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param quantity Requested quantity (Intex units).
    /// @param bidRate Bid rate (`1e6` fixed-point, % of the escrow basis).
    /// @param issuanceCurrency Declared issuance currency. It must be an ISO 4217 numeric code.
    /// @param referenceCurrency Reference currency the bid prices in. It must be one the day carries.
    /// @param chainId Chain id. It must equal `block.chainid`. This is a redundant check: the EIP-712
    ///                domain already binds it inside the signature.
    /// @param signature 65-byte ECDSA signature over the EIP-712 `RevealBid` typed data.
    function revealIntexAuctionBid(
        uint32 emissionDay,
        uint16 quantity,
        uint32 bidRate,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint64 chainId,
        bytes memory signature
    ) external;

    /// @notice Permissionless refund. The money always goes to `bidder`.
    ///         - A winner collects its change. Its bid stays for `issueIntex`.
    ///         - A loser of a finalized day collects its principal and its bid is cleared.
    ///         - A bidder whose day never finalized collects its principal after the escrow's
    ///           delay. Its bid stays until the day finalizes, since the bids relay may still read it.
    ///         The escrow-local `returnAbandonedIntexAuctionAsset` is the fallback if this contract stops working.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder whose refund is being claimed.
    function returnIntexAuctionAsset(uint32 emissionDay, address bidder) external;

    /// @notice Permissionless redeem of a winner's WinNote. The units always go to `bidder`.
    ///         The escrow pays any change still in it. Once its series was forfeited, the
    ///         WinNote is redeemed without issuing.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Winner whose WinNote is redeemed.
    function redeemWinNote(uint32 emissionDay, address bidder) external;

    /// @notice Permissionless commit-bond claim for a bidder who committed but never revealed.
    ///         A cancelled-day auction releases immediately. Otherwise the bond is claimable
    ///         only after `revealEnd + UNREVEALED_BOND_LOCK_PERIOD`. Pays the stored bidder, not the
    ///         caller. The escrow-local time-based valve (`unlockAbandonedIntexAuctionBidBond`) is the fallback
    ///         if the auction contract itself is rotated away.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder whose bond is being claimed.
    function unlockIntexAuctionBidBond(uint32 emissionDay, address bidder) external;

    // --- Views ---

    /// @notice Get auction information by series id.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @return auctionData Auction information including schedule, params and result.
    function getAuctionInfo(uint32 emissionDay) external view returns (AuctionData memory auctionData);

    /// @notice How many bids the day has revealed.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @return count Revealed bid count.
    function revealedIntexAuctionBidsCount(uint32 emissionDay) external view returns (uint256 count);

    /// @notice A window of the day's revealed bids, so a relay can read only what it is about to send.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param offset First bid to return.
    /// @param limit How many bids at most.
    /// @return slice The bids in `[offset, offset + limit)`, clipped to what the day holds.
    function revealedIntexAuctionBidsSlice(uint32 emissionDay, uint256 offset, uint256 limit)
        external
        view
        returns (SubmittedIntexAuctionBidData[] memory slice);

    /// @notice Get auction information plus the revealed bids by series id.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @return auctionData Auction information.
    /// @return bidsData Array of revealed bids.
    function getAuctionDetails(uint32 emissionDay)
        external
        view
        returns (AuctionData memory auctionData, SubmittedIntexAuctionBidData[] memory bidsData);

    /// @notice Get the current auction stage by series id.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @return Current auction stage.
    function auctionStage(uint32 emissionDay) external view returns (AuctionStage);
}
