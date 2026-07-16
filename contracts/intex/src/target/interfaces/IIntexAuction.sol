// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/**
 * @title IntexAuction Contract Interfaces
 * @author Outbe
 * @notice Public API, events, errors, and data types for `IntexAuction`.
 * @dev All auctions are keyed by `worldwideDay` (uint32, yyyymmdd).
 */
interface IIntexAuction {
    // --- Types ---

    /// @notice Auction lifecycle stages.
    enum AuctionStage {
        CommittingBids,
        RevealingBids,
        Issuance,
        Completed,
        Cancelled
    }

    /// @notice Worldwide-day state gating the reveal stage.
    /// @dev `Unknown` = awaiting the bridge signal, `Green` = reveal allowed,
    ///      `Red` = auction cancelled.
    enum WorldwideDayState {
        Unknown,
        Green,
        Red
    }

    /// @notice Revealed bid payload. Slot-packed: slot 0 holds
    ///         `bidderAddress` (20B) + `intexBidRate` (4B) + `intexQuantity` (2B) + `timestamp` (4B) = 30B.
    struct SubmittedBidData {
        /// @notice Bidder IBA address.
        address bidderAddress;
        /// @notice Bid rate the bidder accepts (`1e6` fixed-point, % of the escrow basis).
        uint32 intexBidRate;
        /// @notice Requested quantity (Intex units).
        uint16 intexQuantity;
        /// @notice Timestamp assigned at reveal (ordering only).
        uint32 timestamp;
    }

    /// @notice Auction schedule — stage-end timestamps.
    /// @dev Computed on the Outbe side (Desis) and passed into `auctionStart`.
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
        /// @notice Call-trigger observation window in days.
        uint16 windowDays;
        /// @notice Call-trigger threshold in days.
        uint16 thresholdDays;
        /// @notice Called→deadline window in seconds; stored verbatim, the issuer must supply a non-zero value.
        uint32 intexCallPeriod;
    }

    /// @notice Auction input parameters, stored per auction. Field order mirrors the spec
    ///         (single-currency: flat entry/floor/call stand in for the `referencePrices[]` slot).
    struct AuctionParams {
        /// @notice Issuance currency (ISO numeric); single USD (840) until multi-currency.
        uint16 issuanceCurrency;
        /// @notice Reference currency (ISO numeric); single USD (840) until multi-currency.
        uint16 referenceCurrency;
        /// @notice Promis tokens per Intex unit (18 decimals).
        uint128 promisLoadMinor;
        /// @notice Call-trigger parameters (window/threshold/period).
        IntexCallTrigger callTrigger;
        /// @notice Minimum allowed bid rate (`1e6` fixed-point, % of the escrow basis); rejects bids below it on reveal.
        uint32 minIntexBidRate;
        /// @notice Minimum quantity per bid (Intex units).
        uint16 minIntexBidQuantity;
        /// @notice Per-unit entry price (reference ccy); feeds floor/call.
        uint64 entryPriceMinor;
        /// @notice Floor price (reference ccy).
        uint64 floorPriceMinor;
        /// @notice Call price (reference ccy).
        uint64 callPriceMinor;
        /// @notice Entry bond (payment-token minor units) taken at `commitBid` and returned on
        ///         reveal/cancel; 0 disables the bond.
        uint128 commitBondMinor;
    }

    /// @notice Auction results and statistics (final, set at clearing).
    struct AuctionResult {
        /// @notice Uniform auction clearing rate (`1e6` fixed-point) used to issue Intex.
        uint64 auctionClearingRate;
        /// @notice Number of winning bids (provided by Outbe).
        uint32 wonBidsCount;
        /// @notice Number of Intex units issued.
        uint32 issuedIntexCount;
        /// @notice Total Promis loaded into the issued Intex (`issuedIntexCount * promisLoadMinor`); derived on-chain at clearing.
        uint128 issuedIntexLoadedPromis;
    }

    /// @notice Live bid counters tracked while the auction runs.
    struct AuctionRunningCounts {
        uint32 committedBidsCount;
        uint32 revealedBidsCount;
    }

    /// @notice Auction parameters and state, keyed by `worldwideDay`.
    struct AuctionData {
        WorldwideDayState worldwideDayState;
        AuctionSchedule schedule;
        AuctionParams params;
        AuctionResult result;
    }

    // --- Events ---

    /// @notice Emitted when the auction stage is updated.
    /// @param worldwideDay Auction series id (yyyymmdd as uint32).
    /// @param auctionStage Target stage.
    /// @param timestamp New stage timestamp (UNIX seconds).
    /// @param reason Optional reason (e.g. "Red day - auction cancelled"); empty if not applicable.
    event AuctionStageUpdated(uint32 indexed worldwideDay, AuctionStage auctionStage, uint32 timestamp, string reason);

    /// @notice Emitted when an auction is cleared.
    /// @param worldwideDay Auction series id.
    /// @param auctionClearingRate Uniform auction clearing rate (`1e6` fixed-point).
    /// @param issuedIntexCount Total number of issued Intex units.
    event AuctionClearingExecuted(uint32 indexed worldwideDay, uint64 auctionClearingRate, uint32 issuedIntexCount);

    /// @notice Emitted on `commitBid` with the sealed commit hash.
    /// @param worldwideDay Auction series id.
    /// @param bidder Bidder address (commit owner).
    /// @param commitHash The committed `keccak256(signature)`.
    event BidCommitted(uint32 indexed worldwideDay, address indexed bidder, bytes32 commitHash);

    /// @notice Emitted on `revealBid` after a successful reveal.
    /// @param worldwideDay Auction series id.
    /// @param bidder Bidder address.
    /// @param quantity Revealed Intex quantity.
    /// @param bidRate Revealed bid rate (`1e6` fixed-point, % of the escrow basis).
    event BidRevealed(uint32 indexed worldwideDay, address indexed bidder, uint16 indexed quantity, uint32 bidRate);

    /// @notice Emitted on `cancelCommit` after the bidder withdraws their commit during the commit stage.
    /// @param worldwideDay Auction series id.
    /// @param bidder Bidder address.
    event CommitCancelled(uint32 indexed worldwideDay, address indexed bidder);
    /// @notice A terminal auction's stored revealed-bid records were reclaimed; `remaining` still to reap.
    event AuctionReaped(uint32 indexed worldwideDay, uint256 remaining);

    /// @notice Emitted on `wire` after the escrow contract address is set.
    /// @param previous Escrow contract address before the update.
    /// @param current Escrow contract address after the update.
    event EscrowWired(address previous, address current);

    // --- Errors ---

    /// @notice Zero address provided.
    /// @param f Field name.
    error ZeroAddress(string f);
    /// @notice Zero value provided where non-zero is required.
    /// @param f Field name.
    error ZeroValue(string f);
    /// @notice Operation requires a different stage.
    error StageRequired(AuctionStage requiredStage, AuctionStage currentStage);
    /// @notice `reapAuction` was called before the auction passed its issuance deadline.
    error TooEarlyToReap();
    /// @notice Commit already registered for this bidder in this auction.
    error BidAlreadyCommitted();
    /// @notice Commit not found for this bidder in this auction.
    error BidNotFound();
    /// @notice Bid already revealed for this bidder in this auction.
    error BidAlreadyRevealed();
    /// @notice Reveal payload does not match the commit hash.
    error RevealHashMismatch();
    /// @notice Bid rate is below `minIntexBidRate`.
    error BidBelowMinIntexBidRate();
    /// @notice Bid rate exceeds 100% of the escrow basis (`RATE_SCALE`).
    error BidRateAboveMax(uint32 bidRate);
    /// @notice Bid quantity is below `minIntexBidQuantity`.
    error BidBelowMinIntexBidQuantity();
    /// @notice `quantity * escrowBasis * bidRate / RATE_SCALE` exceeds the uint128 lock-amount range.
    error BidAmountOverflow(uint16 quantity, uint32 bidRate);
    /// @notice `issuedIntexCount * promisLoadMinor` exceeds the uint128 loaded-Promis range.
    error IssuedPromisOverflow(uint32 issuedIntexCount, uint128 promisLoadMinor);
    /// @notice `wire` called while the current escrow still holds live locks.
    error EscrowHasLiveLocks();
    /// @notice Auction does not exist.
    error AuctionNotFound();
    /// @notice Auction already exists.
    error AuctionAlreadyExists();
    /// @notice Clearing result claims more winners than were revealed on-chain.
    error WonBidsExceedRevealed(uint32 wonBidsCount, uint32 revealedBidsCount);
    /// @notice Clearing rate is below the configured minimum bid rate.
    error ClearingRateBelowMin(uint64 clearingRate, uint32 minIntexBidRate);
    /// @notice Schedule timestamps are not strictly increasing or are in the past.
    error InvalidSchedule();
    /// @notice Commit hash must be non-zero.
    error InvalidCommitHash();
    /// @notice Chain id mismatch between the caller-supplied value and `block.chainid`.
    error WrongChain(uint256 expected, uint256 got);
    /// @notice `commitBid`/`cancelCommit` attempted at or after the published `commitEnd`.
    ///         The commit window is `[start, commitEnd)`; the deadline second is already closed.
    error CommitWindowClosed(uint32 commitEnd, uint32 nowTs);
    /// @notice `claimCommitBond` was called before the no-reveal penalty window elapsed.
    /// @param claimableAt Earliest unix-seconds timestamp the bond can be claimed at.
    /// @param nowTs Current block timestamp.
    error CommitBondNotYetClaimable(uint32 claimableAt, uint32 nowTs);

    // --- Admin ---

    /// @notice Wire contract dependencies.
    /// @param _escrow Escrow contract address.
    function wire(address _escrow) external;

    // --- Lifecycle ---

    /// @notice Create and start a new auction for `worldwideDay`.
    /// @dev The schedule (`commitEnd`/`revealEnd`/`issuanceEnd`) is computed on the
    ///      Outbe side (Desis) and passed in.
    /// @param worldwideDay Auction series id (yyyymmdd as uint32).
    /// @param schedule Stage-end timestamps.
    /// @param params Auction input parameters.
    function auctionStart(uint32 worldwideDay, AuctionSchedule calldata schedule, AuctionParams calldata params) external;

    /// @notice Start the reveal stage (bridge-driven; green day proceeds, red day cancels).
    /// @dev Early green-day signal snaps `commitEnd` forward; `revealEnd` is unchanged.
    /// @param worldwideDay Auction series id.
    /// @param isGreenDay True = green day (proceed to reveal), false = red day (cancel auction).
    function startRevealingBidsStage(uint32 worldwideDay, bool isGreenDay) external;

    /// @notice Advance the auction to the issuance stage (bridge-driven clearing signal from Outbe).
    /// @dev Early signal snaps `revealEnd` forward; `issuanceEnd` is unchanged.
    /// @param worldwideDay Auction series id.
    function startClearingStage(uint32 worldwideDay) external;

    /// @notice Execute auction clearing with final data from Outbe.
    /// @dev `issuedIntexLoadedPromis` is derived on-chain (`issuedIntexCount * promisLoadMinor`).
    /// @param worldwideDay Auction series id.
    /// @param issuedIntexCount Final number of issued Intex units.
    /// @param auctionClearingRate Uniform clearing rate (`1e6` fixed-point) calculated by Outbe.
    /// @param wonBidsCount Number of winning bids (from Outbe).
    function executeAuctionClearing(
        uint32 worldwideDay,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) external;

    // --- User Actions ---

    /// @notice Commit a sealed bid hash for an auction.
    /// @dev When the series carries a non-zero `commitBondMinor`, the bond is pulled from the
    ///      caller into escrow in the same transaction (requires prior payment-token approval on
    ///      the escrow adapter). Reveal/cancel return it immediately; a green-day no-reveal locks
    ///      it until `revealEnd + COMMIT_BOND_LOCK_PERIOD` (see `claimCommitBond`).
    /// @param worldwideDay Auction series id.
    /// @param commitHash `keccak256(signature)`, where `signature` is an EIP-712 typed-data
    ///                   signature over `RevealBid(uint32 worldwideDay,address bidder,uint16 quantity,uint32 bidRate)`
    ///                   under the `IntexAuction` v1 domain (`chainId`, `verifyingContract = address(this)`).
    function commitBid(uint32 worldwideDay, bytes32 commitHash) external;

    /// @notice Cancel an existing commit during the commit stage.
    /// @dev Only callable before `commitEnd`. Once the commit window closes a commit can no longer
    ///      be cancelled or revealed — an unrevealed commit is permanently forfeited (its bond
    ///      stays claimable via `claimCommitBond`). Cancelling returns the bond immediately.
    /// @param worldwideDay Auction series id.
    function cancelCommit(uint32 worldwideDay) external;

    /// @notice Reclaim a terminal, past-issuance auction's stored revealed-bid records.
    /// @param worldwideDay Series identifier.
    /// @param limit Maximum records to delete this call; paginate large sets across calls.
    function reapAuction(uint32 worldwideDay, uint256 limit) external;

    /// @notice Reveal a bid.
    /// @dev Returns the commit bond (if any) before locking the bid escrow, so the bond can fund
    ///      the bid in the same transaction.
    /// @param worldwideDay Auction series id.
    /// @param quantity Requested quantity (Intex units).
    /// @param bidRate Bid rate (`1e6` fixed-point, % of the escrow basis).
    /// @param chainId Chain id; must equal `block.chainid` (belt-and-braces; the EIP-712 domain
    ///                already binds it inside the signature).
    /// @param signature 65-byte ECDSA signature over the EIP-712 `RevealBid` typed data.
    function revealBid(uint32 worldwideDay, uint16 quantity, uint32 bidRate, uint64 chainId, bytes memory signature)
        external;

    /// @notice Permissionless commit-bond claim for a bidder who committed but never revealed.
    ///         A cancelled (red-day) auction releases immediately; otherwise the bond is claimable
    ///         only after `revealEnd + COMMIT_BOND_LOCK_PERIOD`. Pays the stored bidder, not the
    ///         caller. The escrow-local time-based valve (`claimAbandonedCommitBond`) backs this up
    ///         if the auction contract itself is rotated away.
    /// @param worldwideDay Auction series id.
    /// @param bidder Bidder whose bond is being claimed.
    function claimCommitBond(uint32 worldwideDay, address bidder) external;

    // --- Views ---

    /// @notice Get auction information by series id.
    /// @param worldwideDay Auction series id.
    /// @return auctionData Auction information including schedule, params and result.
    function getAuctionInfo(uint32 worldwideDay) external view returns (AuctionData memory auctionData);

    /// @notice Get auction information plus the revealed bids by series id.
    /// @param worldwideDay Auction series id.
    /// @return auctionData Auction information.
    /// @return bidsData Array of revealed bids.
    function getAuctionDetails(uint32 worldwideDay)
        external
        view
        returns (AuctionData memory auctionData, SubmittedBidData[] memory bidsData);

    /// @notice Get the current auction stage by series id.
    /// @param worldwideDay Auction series id.
    /// @return Current auction stage.
    function getAuctionStage(uint32 worldwideDay) external view returns (AuctionStage);
}
