// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

/**
 * @title IntexAuction Contract Interfaces
 * @author Outbe
 * @notice Public API, events, errors, and data types for `IntexAuction`.
 * @dev All auctions are keyed by `seriesId` (uint32, yyyymmdd).
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
    ///         `bidderAddress` (20B) + `intexBidPrice` (8B) + `timestamp` (4B) = 32B.
    struct SubmittedBidData {
        /// @notice Bidder IBA address.
        address bidderAddress;
        /// @notice Bid price per Intex unit the bidder accepts (payment-token decimals).
        uint64 intexBidPrice;
        /// @notice Timestamp assigned at reveal (ordering only).
        uint32 timestamp;
        /// @notice Requested quantity (Intex units).
        uint16 intexQuantity;
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

    /// @notice Auction input parameters, stored per auction.
    struct AuctionParams {
        /// @notice Promis tokens per Intex unit (18 decimals).
        uint128 intexSize;
        /// @notice Minimum allowed bid price per Intex unit; rejects bids below this on reveal.
        uint64 minIntexBidPrice;
        /// @notice Intex strike price (payment-token decimals).
        uint64 intexStrikePrice;
        /// @notice COEN price floor (payment-token decimals).
        uint64 coenPriceFloor;
        /// @notice Minimum quantity per bid (Intex units).
        uint16 minIntexBidQuantity;
    }

    /// @notice Auction results and statistics (final, set at clearing).
    struct AuctionResult {
        /// @notice Total Promis loaded into the issued Intex (`issuedIntexCount * intexSize`); derived on-chain at clearing.
        uint128 issuedIntexLoadedPromis;
        /// @notice Uniform auction clearing price used to issue Intex.
        uint64 auctionIntexClearingPrice;
        /// @notice Number of Intex units issued.
        uint32 issuedIntexCount;
        /// @notice Number of winning bids (provided by Outbe).
        uint32 wonBidsCount;
    }

    /// @notice Live bid counters tracked while the auction runs.
    struct AuctionRunningCounts {
        uint32 committedBidsCount;
        uint32 revealedBidsCount;
    }

    /// @notice Auction parameters and state, keyed by `seriesId`.
    struct AuctionData {
        WorldwideDayState worldwideDayState;
        AuctionSchedule schedule;
        AuctionParams params;
        AuctionResult result;
    }

    // --- Events ---

    /// @notice Emitted when the auction stage is updated.
    /// @param seriesId Auction series id (yyyymmdd as uint32).
    /// @param auctionStage Target stage.
    /// @param timestamp New stage timestamp (UNIX seconds).
    /// @param reason Optional reason (e.g. "Red day - auction cancelled"); empty if not applicable.
    event AuctionStageUpdated(uint32 indexed seriesId, AuctionStage auctionStage, uint32 timestamp, string reason);

    /// @notice Emitted when an auction is cleared.
    /// @param seriesId Auction series id.
    /// @param auctionIntexClearingPrice Uniform auction clearing price.
    /// @param issuedIntexCount Total number of issued Intex units.
    event AuctionClearingExecuted(uint32 indexed seriesId, uint64 auctionIntexClearingPrice, uint32 issuedIntexCount);

    /// @notice Emitted on `commitBid` with the sealed commit hash.
    /// @param seriesId Auction series id.
    /// @param bidder Bidder address (commit owner).
    /// @param commitHash The committed `keccak256(signature)`.
    event BidCommitted(uint32 indexed seriesId, address indexed bidder, bytes32 commitHash);

    /// @notice Emitted on `revealBid` after a successful reveal.
    /// @param seriesId Auction series id.
    /// @param bidder Bidder address.
    /// @param quantity Revealed Intex quantity.
    /// @param bidPrice Revealed bid price per unit.
    event BidRevealed(uint32 indexed seriesId, address indexed bidder, uint16 indexed quantity, uint64 bidPrice);

    /// @notice Emitted on `cancelCommit` after the bidder withdraws their commit during the commit stage.
    /// @param seriesId Auction series id.
    /// @param bidder Bidder address.
    event CommitCancelled(uint32 indexed seriesId, address indexed bidder);

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
    /// @notice Commit already registered for this bidder in this auction.
    error BidAlreadyCommitted();
    /// @notice Commit not found for this bidder in this auction.
    error BidNotFound();
    /// @notice Bid already revealed for this bidder in this auction.
    error BidAlreadyRevealed();
    /// @notice Reveal payload does not match the commit hash.
    error RevealHashMismatch();
    /// @notice Bid price is below `minIntexBidPrice`.
    error BidBelowMinIntexBidPrice();
    /// @notice Bid quantity is below `minIntexBidQuantity`.
    error BidBelowMinIntexBidQuantity();
    /// @notice `quantity * bidPrice` exceeds the uint64 lock-amount range.
    error BidAmountOverflow(uint16 quantity, uint64 bidPrice);
    /// @notice Auction does not exist.
    error AuctionNotFound();
    /// @notice Auction already exists.
    error AuctionAlreadyExists();
    /// @notice Clearing result claims more winners than were revealed on-chain.
    error WonBidsExceedRevealed(uint32 wonBidsCount, uint32 revealedBidsCount);
    /// @notice Clearing price is below the configured minimum bid price.
    error ClearingPriceBelowMin(uint64 clearingPrice, uint64 minIntexBidPrice);
    /// @notice Schedule timestamps are not strictly increasing or are in the past.
    error InvalidSchedule();
    /// @notice Commit hash must be non-zero.
    error InvalidCommitHash();
    /// @notice Chain id mismatch between the caller-supplied value and `block.chainid`.
    error WrongChain(uint256 expected, uint256 got);
    /// @notice `commitBid`/`cancelCommit` attempted at or after the published `commitEnd`.
    ///         The commit window is `[start, commitEnd)`; the deadline second is already closed.
    error CommitWindowClosed(uint32 commitEnd, uint32 nowTs);

    // --- Admin ---

    /// @notice Wire contract dependencies.
    /// @param _escrow Escrow contract address.
    function wire(address _escrow) external;

    // --- Lifecycle ---

    /// @notice Create and start a new auction for `seriesId`.
    /// @dev The schedule (`commitEnd`/`revealEnd`/`issuanceEnd`) is computed on the
    ///      Outbe side (Desis) and passed in.
    /// @param seriesId Auction series id (yyyymmdd as uint32).
    /// @param schedule Stage-end timestamps.
    /// @param params Auction input parameters.
    function auctionStart(uint32 seriesId, AuctionSchedule calldata schedule, AuctionParams calldata params) external;

    /// @notice Start the reveal stage (bridge-driven; green day proceeds, red day cancels).
    /// @dev Early green-day signal snaps `commitEnd` forward; `revealEnd` is unchanged.
    /// @param seriesId Auction series id.
    /// @param isGreenDay True = green day (proceed to reveal), false = red day (cancel auction).
    function startRevealingBidsStage(uint32 seriesId, bool isGreenDay) external;

    /// @notice Advance the auction to the issuance stage (bridge-driven clearing signal from Outbe).
    /// @dev Early signal snaps `revealEnd` forward; `issuanceEnd` is unchanged.
    /// @param seriesId Auction series id.
    function startClearingStage(uint32 seriesId) external;

    /// @notice Execute auction clearing with final data from Outbe.
    /// @dev `issuedIntexLoadedPromis` is derived on-chain (`issuedIntexCount * intexSize`).
    /// @param seriesId Auction series id.
    /// @param issuedIntexCount Final number of issued Intex units.
    /// @param auctionIntexClearingPrice Uniform clearing price calculated by Outbe.
    /// @param wonBidsCount Number of winning bids (from Outbe).
    function executeAuctionClearing(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionIntexClearingPrice,
        uint32 wonBidsCount
    ) external;

    // --- User Actions ---

    /// @notice Commit a sealed bid hash for an auction.
    /// @param seriesId Auction series id.
    /// @param commitHash `keccak256(signature)`, where `signature` is an EIP-712 typed-data
    ///                   signature over `RevealBid(uint32 seriesId,address bidder,uint16 quantity,uint64 bidPrice)`
    ///                   under the `IntexAuction` v1 domain (`chainId`, `verifyingContract = address(this)`).
    function commitBid(uint32 seriesId, bytes32 commitHash) external;

    /// @notice Cancel an existing commit during the commit stage.
    /// @dev Only callable before `commitEnd`. Once the commit window closes a commit can no longer
    ///      be cancelled or revealed — an unrevealed commit is permanently forfeited.
    /// @param seriesId Auction series id.
    function cancelCommit(uint32 seriesId) external;

    /// @notice Reveal a bid.
    /// @param seriesId Auction series id.
    /// @param quantity Requested quantity (Intex units).
    /// @param bidPrice Bid price per unit (payment-token decimals).
    /// @param chainId Chain id; must equal `block.chainid` (belt-and-braces; the EIP-712 domain
    ///                already binds it inside the signature).
    /// @param signature 65-byte ECDSA signature over the EIP-712 `RevealBid` typed data.
    function revealBid(
        uint32 seriesId,
        uint16 quantity,
        uint64 bidPrice,
        uint64 chainId,
        bytes memory signature
    ) external;

    // --- Views ---

    /// @notice Get auction information by series id.
    /// @param seriesId Auction series id.
    /// @return auctionData Auction information including schedule, params and result.
    function getAuctionInfo(uint32 seriesId) external view returns (AuctionData memory auctionData);

    /// @notice Get auction information plus the revealed bids by series id.
    /// @param seriesId Auction series id.
    /// @return auctionData Auction information.
    /// @return bidsData Array of revealed bids.
    function getAuctionDetails(uint32 seriesId)
        external
        view
        returns (AuctionData memory auctionData, SubmittedBidData[] memory bidsData);

    /// @notice Get the current auction stage by series id.
    /// @param seriesId Auction series id.
    /// @return Current auction stage.
    function getAuctionStage(uint32 seriesId) external view returns (AuctionStage);
}
