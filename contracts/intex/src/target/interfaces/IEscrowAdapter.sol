// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/**
 * @title EscrowAdapter Contract Interface
 * @author Outbe
 * @notice Public API, events, errors, and data types for escrow operations with The Compact.
 * @dev Integrates with The Compact protocol for locking bid funds and handles auction
 *      finalization. All escrow state is keyed by `worldwideDay` (uint32).
 */
interface IEscrowAdapter {
    // --- Types ---

    /// @notice Lock status for a bid. `Won`: the payment left with the day's proceeds and the rest waits for
    ///         `claimRefund`. A claimed lock is deleted and reads as `None`.
    enum LockStatus {
        None,
        Locked,
        // Settled by a finalization that paid out on the spot; nothing writes it now.
        Finalized,
        Won
    }

    /// @notice Bid lock data stored per series per bidder.
    /// @dev Slot-packed: `lockedAmount` (16B) + `lockedAt` (4B) + `status` (1B) + `bidRate` (4B) + `quantity` (2B)
    ///      = 27B, one slot;
    ///      `failedRefund` (16B) + `splitRecorded` (1B) = 17B, a second slot.
    struct BidLock {
        /// @notice Amount of payment-token locked.
        uint128 lockedAmount;
        /// @notice Timestamp when the lock was created (UNIX seconds).
        uint32 lockedAt;
        /// @notice Current status of the lock.
        LockStatus status;
        /// @notice Bid rate the lock was taken at (`1e6` fixed-point, share of the escrow basis).
        uint32 bidRate;
        /// @notice Intex units the bid asked for.
        uint16 quantity;
        /// @notice Refund portion of a split recorded when a finalization instruction failed.
        /// @dev Only locks from before refunds became claims carry one; `claimRefund` still honours it.
        uint128 failedRefund;
        /// @notice Whether `failedRefund` was recorded.
        bool splitRecorded;
    }

    /// @notice The clearing terms a day's winners paid at, taken from its first refund chunk.
    struct DayClearing {
        /// @notice Clearing rate (`1e6` fixed-point, share of the escrow basis).
        uint64 clearingRate;
        /// @notice Escrow basis (`promisLoadMinor`).
        uint128 basis;
    }

    /// @notice Per-series escrow state.
    struct AuctionEscrowState {
        /// @notice Total payment-token currently locked for the series.
        uint128 totalLocked;
        /// @notice Number of bid locks created for the series.
        uint32 lockCount;
        /// @notice Timestamp when `finalizeAuction` flipped `finalized = true` (UNIX seconds); 0 if never finalized.
        uint32 finalizedAt;
        /// @notice Whether the series escrow has been finalized.
        bool finalized;
        /// @notice Asset version the day's locks were taken under; meaningful once `lockCount > 0`.
        uint8 assetVersion;
    }

    /// @notice Commit-entry bond taken at `commitBid` and held until reveal/cancel/claim.
    /// @dev Existence sentinel is `amount > 0`; the record is deleted on release so a
    ///      commit->cancel->commit cycle can re-lock within the same series.
    struct CommitBond {
        /// @notice Amount of payment-token bonded.
        uint128 amount;
        /// @notice Timestamp when the bond was locked (UNIX seconds). Anchors the
        ///         escrow-local `claimAbandonedCommitBond` safety window.
        uint32 lockedAt;
        /// @notice Asset version the bond was taken under.
        uint8 assetVersion;
    }

    /// @notice A payment token and Compact position the escrow rotated away from. Locks and bonds taken
    ///         under it keep withdrawing from it.
    struct AssetVersion {
        /// @notice The Compact the position lives in.
        address compact;
        /// @notice Payment token the position holds.
        IERC20 paymentToken;
        /// @notice The Compact resource lock id of the position.
        uint256 lockId;
    }

    // --- Events ---

    /// @notice Emitted when funds are locked for a bid during reveal.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder whose funds were locked.
    /// @param amount Amount of payment-token locked.
    event FundsLocked(uint32 indexed worldwideDay, address indexed bidder, uint128 amount);

    /// @notice Emitted when a commit-entry bond is locked at `commitBid`.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder whose bond was taken.
    /// @param amount Amount of payment-token bonded.
    event CommitBondLocked(uint32 indexed worldwideDay, address indexed bidder, uint128 amount);

    /// @notice Emitted when a commit-entry bond is returned to its owner (reveal, cancel,
    ///         auction-side claim, or the escrow-local abandoned-bond claim).
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder the bond was returned to.
    /// @param amount Amount of payment-token returned.
    event CommitBondReleased(uint32 indexed worldwideDay, address indexed bidder, uint128 amount);

    /// @notice Emitted when funds are refunded to a bidder.
    /// @param receiveId `bytes32(0)`: a refund is paid by `claimRefund`, never by a bridge message.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder who received the refund.
    /// @param amount Amount refunded to the bidder.
    event FundsRefunded(bytes32 indexed receiveId, uint32 indexed worldwideDay, address indexed bidder, uint128 amount);

    /// @notice Emitted when the winning remainder of a split recorded before refunds became claims is burned
    ///         (sent to the canonical dead address); its proceeds were already routed on Outbe.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder whose winning portion was burned.
    /// @param amount Amount of payment-token burned.
    event ProceedsBurned(uint32 indexed worldwideDay, address indexed bidder, uint128 amount);

    /// @notice Emitted for each refund chunk the escrow applies.
    /// @param receiveId Inbound bridge message that carried the chunk.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param totalRefunded Refunds the chunk's winners are now owed through `claimRefund`.
    /// @param totalPaid Winning payments routed to the proceeds recipient.
    /// @param bidsProcessed Winners the chunk carried.
    event AuctionEscrowFinalized(
        bytes32 indexed receiveId,
        uint32 indexed worldwideDay,
        uint128 totalRefunded,
        uint128 totalPaid,
        uint32 bidsProcessed
    );

    /// @notice Emitted on each successful `wire()` call (initial + rotations).
    /// @dev Carries old+new for every dependency so a rotation is reconstructible from the log
    ///      alone; the `*Old` fields are `address(0)` on the initial wire.
    /// @param intexAuctionOld IntexAuction address before this wire.
    /// @param intexAuctionNew IntexAuction address after this wire.
    /// @param compactOld The Compact address before this wire.
    /// @param compactNew The Compact address after this wire.
    /// @param paymentTokenOld Active payment-token address before this wire.
    /// @param paymentTokenNew Active payment-token address after this wire.
    event Wired(
        address intexAuctionOld,
        address intexAuctionNew,
        address compactOld,
        address compactNew,
        address paymentTokenOld,
        address paymentTokenNew
    );

    /// @notice Emitted when a rotation retires the active asset; locks and bonds taken under it keep using it.
    /// @param version Version number the retired asset keeps.
    /// @param compact The Compact the retired position lives in.
    /// @param paymentToken Payment token of the retired position.
    /// @param lockId Resource lock id of the retired position.
    event AssetRetired(uint8 indexed version, address compact, address paymentToken, uint256 lockId);

    /// @notice Emitted when a winner in a refund chunk cannot be settled - its lock is not live, the partial fill
    ///         does not fit it, or its payment would exceed it. The lock is left as it was.
    /// @param receiveId Inbound bridge message that carried the chunk.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Winner that was skipped.
    /// @param reason Encoded error naming why.
    event BidderRefundFailed(
        bytes32 indexed receiveId, uint32 indexed worldwideDay, address indexed bidder, bytes reason
    );

    /// @notice Emitted when a refund chunk carried winners and settled none of them.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidsProcessed Winners the chunk carried, all skipped.
    event FinalizationNoOp(uint32 indexed worldwideDay, uint32 bidsProcessed);

    /// @notice Emitted when the finalized-proceeds recipient is configured.
    /// @param recipient Address receiving each series' finalized proceeds.
    event ProceedsRecipientSet(address recipient);

    // --- Errors ---

    /// @notice Zero address provided.
    /// @param f Field name.
    error ZeroAddress(string f);
    /// @notice Zero value provided where non-zero is required.
    /// @param f Field name.
    error ZeroValue(string f);
    /// @notice Payment token does not report 18 decimals.
    /// @param actual Decimals the token reports.
    error PaymentTokenDecimals(uint8 actual);
    /// @notice Bidder already has locked funds for this series.
    error BidAlreadyLocked();
    /// @notice Lock is not in the active state required for this operation.
    error LockNotActive();
    /// @notice Series escrow has already been finalized.
    error AlreadyFinalized();
    /// @notice A winner's payment at the day's clearing terms would exceed its lock.
    /// @param locked Locked amount.
    /// @param paid Payment the clearing terms work out.
    error PaymentExceedsLock(uint128 locked, uint256 paid);
    /// @notice A refund chunk names a partially filled winner outside the winners it carries.
    /// @param partialIndex Index the partial fill points at.
    /// @param winners Winners the chunk carries.
    error PartialFillOutsideChunk(uint16 partialIndex, uint256 winners);
    /// @notice A partial fill is not smaller than the quantity the winner locked for.
    /// @param won Units the winner received.
    /// @param quantity Units the lock was taken for.
    error PartialFillNotPartial(uint16 won, uint16 quantity);
    /// @notice A refund chunk with winners arrived without clearing terms.
    error ClearingTermsMissing();
    /// @notice A refund chunk's clearing terms differ from the ones the day's first chunk recorded.
    /// @param clearingRate Clearing rate the chunk carried.
    /// @param basis Escrow basis the chunk carried.
    error ClearingTermsMismatch(uint64 clearingRate, uint128 basis);
    /// @notice `attest` was called for a lock id that does not match this escrow's `lockId`.
    /// @param id The unexpected lock id passed to `attest`.
    error UnexpectedLockId(uint256 id);
    /// @notice `authorizeClaim` is not a supported allocator operation on this escrow.
    error ClaimAuthorizationUnsupported();
    /// @notice The Compact forced withdrawal returned false (e.g. the reset period has not elapsed).
    error ForcedWithdrawalFailed();
    /// @notice No deposits made yet (lock id not set).
    error NoDeposits();
    /// @notice A lock was offered for a day whose earlier locks sit under a retired asset.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param assetVersion Asset version the day is bound to.
    error DayAssetRetired(uint32 worldwideDay, uint8 assetVersion);
    /// @notice Finalization produced proceeds but no recipient is configured.
    error ProceedsRecipientNotSet();
    /// @notice `claimRefund` was called before the safety window elapsed.
    /// @param claimableAt Earliest unix-seconds timestamp the refund can be claimed at.
    /// @param now_ Current block timestamp.
    error RefundNotYetClaimable(uint32 claimableAt, uint32 now_);
    /// @notice `lockCommitBond` called while the bidder already holds a live bond for the series.
    error CommitBondAlreadyLocked();
    /// @notice No live commit bond exists for the series/bidder pair.
    error CommitBondNotFound();
    /// @notice `claimAbandonedCommitBond` was called before the escrow-local safety window elapsed.
    /// @param claimableAt Earliest unix-seconds timestamp the bond can be claimed at.
    /// @param now_ Current block timestamp.
    error CommitBondNotYetAbandoned(uint32 claimableAt, uint32 now_);

    // --- Admin ---

    /// @notice Wire contract dependencies.
    /// @dev Rotating `_paymentToken` or `_compact` retires the active asset under its version; locks and
    ///      bonds taken under it keep withdrawing from it.
    /// @param _intexAuction IntexAuction contract address.
    /// @param _compact The Compact contract address.
    /// @param _paymentToken Active payment-token address.
    function wire(address _intexAuction, address _compact, address _paymentToken) external;

    // --- Auction Integration ---

    /// @notice Lock funds for a bid during the reveal stage. Callable only by the IntexAuction contract.
    /// @dev The bidder must approve this contract to spend `paymentToken` beforehand.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @param amount Amount to lock (`intexQuantity * intexBidPrice`).
    /// @param bidRate Bid rate the amount was computed at.
    /// @param quantity Intex units the bid asked for.
    function lockFunds(uint32 worldwideDay, address bidder, uint128 amount, uint32 bidRate, uint16 quantity) external;

    /// @notice Lock the commit-entry bond at `commitBid`. Callable only by the IntexAuction contract.
    /// @dev The bidder must approve this contract to spend `paymentToken` beforehand. The bond is
    ///      held in The Compact under the same lock id as bid escrow.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address the bond is taken from (and later returned to).
    /// @param amount Bond amount (the series' `commitBondMinor`).
    function lockCommitBond(uint32 worldwideDay, address bidder, uint128 amount) external;

    /// @notice Return a live commit bond to its owner. Callable only by the IntexAuction contract
    ///         (reveal, cancel, and the auction-side stage-aware claim path).
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder whose bond is returned.
    function releaseCommitBond(uint32 worldwideDay, address bidder) external;

    /// @notice Active payment token used for bid escrow (WCOEN).
    function paymentToken() external view returns (IERC20);

    /// @notice Recipient of finalized auction proceeds (the router routing them cross-chain).
    function proceedsRecipient() external view returns (address);

    /// @notice Set the recipient of finalized auction proceeds.
    function setProceedsRecipient(address recipient) external;

    // --- Bridge Finalization ---

    /// @notice Apply one refund chunk: take each winner's payment at the day's clearing terms and mark the
    ///         rest of its lock owed. Nothing is paid out to bidders here; every bidder collects through
    ///         `claimRefund`, and a bidder the day's chunks never name lost.
    /// @dev A winner's lock leaves `Locked` on its first chunk, so none pays twice; `completesDay` closes the day.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param receiveId Inbound bridge message id that carried the chunk; threaded into the emitted events.
    /// @param winners Winners on this chain in the chunk.
    /// @param partialIndex Index of the partially filled winner; read only when `partialWon` is non-zero.
    /// @param partialWon Units the partially filled winner received; zero when the chunk has none.
    /// @param clearingRate The day's clearing rate.
    /// @param basis The day's escrow basis.
    /// @param completesDay Whether this is the day's last chunk.
    /// @return totalPaid Proceeds transferred to the caller for cross-chain routing to creators.
    function finalizeAuction(
        uint32 worldwideDay,
        bytes32 receiveId,
        address[] calldata winners,
        uint16 partialIndex,
        uint16 partialWon,
        uint64 clearingRate,
        uint128 basis,
        bool completesDay
    ) external returns (uint128 totalPaid);

    // --- Recovery ---

    /// @notice Permissionless refund, always paying the stored `bidder` rather than `msg.sender`, and deleting the
    ///         lock: a winner collects the rest of its lock at once, a bidder the finalized day never named
    ///         collects its full principal at once, and a bidder whose day never finalized collects its full
    ///         principal after `UNFINALIZED_REFUND_DELAY`.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address whose locked principal is being claimed.
    function claimRefund(uint32 worldwideDay, address bidder) external;

    /// @notice Escrow-local safety valve for a commit bond stranded past
    ///         `COMMIT_BOND_ABANDON_DELAY` (e.g. the auction contract was rotated away while the
    ///         bond was live). Time-based only - never consults the auction - and pays the stored
    ///         `bidder`, not `msg.sender`. The stage-aware fast path lives on IntexAuction.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder whose bond is being claimed.
    function claimAbandonedCommitBond(uint32 worldwideDay, address bidder) external;

    // --- Views ---

    /// @notice Get bid lock information.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address whose lock is being read.
    /// @return lock The stored `BidLock` record for the series/bidder pair.
    function getBidLock(uint32 worldwideDay, address bidder) external view returns (BidLock memory lock);

    /// @notice What `claimRefund` would pay `bidder` for the day, and from when. Zero once nothing is left to
    ///         claim: a settled or claimed lock is gone.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @return amount Refund the claim would pay.
    /// @return claimableAt Earliest unix-seconds timestamp the claim succeeds at.
    function getClaimableRefund(uint32 worldwideDay, address bidder)
        external
        view
        returns (uint128 amount, uint32 claimableAt);

    /// @notice Get commit bond information. A zero `amount` means no live bond.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address whose bond is being read.
    /// @return bond The stored `CommitBond` record for the series/bidder pair.
    function getCommitBond(uint32 worldwideDay, address bidder) external view returns (CommitBond memory bond);

    /// @notice Get series escrow status.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @return hasLocks True if the series has at least one lock.
    /// @return isFinalized True if the series escrow is finalized.
    /// @return totalLocked Total payment-token currently locked for the series.
    function getAuctionStatus(uint32 worldwideDay)
        external
        view
        returns (bool hasLocks, bool isFinalized, uint128 totalLocked);

    /// @notice True while any lock is still live in The Compact under the active lock id.
    function hasOutstandingLocks() external view returns (bool outstanding);

    /// @notice Version number of the active asset.
    function currentAssetVersion() external view returns (uint8 version);

    /// @notice The asset a version refers to; the active version reads the live wiring.
    /// @param version Asset version.
    /// @return asset The Compact, payment token and lock id of that version.
    function getAssetVersion(uint8 version) external view returns (AssetVersion memory asset);
}
