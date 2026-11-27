// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";

/// @notice Escrow time limits the auction reads too.
library EscrowTimes {
    /// @notice Safety window before the bidder of a never-finalized series can claim the full
    ///         principal, anchored at the lock's `lockedAt`. The earliest lock comes at `commitEnd`,
    ///         so the first refund opens at `commitEnd` plus this window. The auction reads a day
    ///         without a result at that time as failed and takes no later one.
    uint32 internal constant UNFINALIZED_RETURN_DELAY = 48 hours;
}

/**
 * @title EscrowAdapter Contract Interface
 * @author Outbe
 * @notice Public API, events, errors, and data types for escrow operations with The Compact.
 * @dev Integrates with The Compact protocol for locking bid funds and handles auction
 *      finalization. All escrow state is keyed by `emissionDay` (uint32).
 */
interface IEscrowAdapter {
    // --- Types ---

    /// @notice Lock status for a bid. `Won`: the payment left with the day's proceeds. The lock keeps
    ///         the change for `returnIntexAuctionAsset` and the units won for `redeemWinNote`. A redeemed or refunded
    ///         lock is deleted and reads as `None`.
    enum LockStatus {
        None,
        Locked,
        Won
    }

    /// @notice Bid lock data stored per series per bidder.
    /// @dev Slot-packed: `lockedAmount` (16B) + `lockedAt` (4B) + `status` (1B) + `bidRate` (4B) + `quantity` (2B)
    ///      = 27B, one slot.
    struct IntexAuctionBidLock {
        /// @notice Amount of payment-token the lock still holds. For a winner, the change.
        uint128 lockedAmount;
        /// @notice Timestamp when the lock was created (UNIX seconds).
        uint32 lockedAt;
        /// @notice Current status of the lock.
        LockStatus status;
        /// @notice Bid rate the lock was taken at (`1e6` fixed-point, share of the escrow basis).
        uint32 bidRate;
        /// @notice Intex units the bid asked for. For a winner, the units won.
        uint16 quantity;
    }

    /// @notice The clearing terms a day's winners charged at, taken from its first refund chunk.
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
        /// @notice Number of bid locks the series ever took. Claims delete their locks but never
        ///         lower it. This keeps `assetVersion` meaningful for a day whose locks are all
        ///         claimed.
        uint32 lockCount;
        /// @notice Timestamp when `chargeIntexAuctionAsset` flipped `finalized = true` (UNIX seconds).
        ///         It is 0 if the series escrow was never finalized.
        uint32 finalizedAt;
        /// @notice Whether the series escrow is finalized.
        bool finalized;
        /// @notice Asset version the day's locks were taken under. It is meaningful once
        ///         `lockCount > 0`.
        uint8 assetVersion;
    }

    /// @notice Commit-entry bond taken at `commitIntexAuctionBid` and held until reveal/cancel/claim.
    /// @dev Existence sentinel is `amount > 0`. The escrow deletes the record on release, so a
    ///      commit->cancel->commit cycle can re-lock within the same series.
    struct IntexAuctionBidBond {
        /// @notice Amount of payment-token bonded.
        uint128 amount;
        /// @notice Timestamp when the bond was locked (UNIX seconds). Anchors the
        ///         escrow-local `unlockIntexAuctionBidBondAfterTimeout` safety window.
        uint32 lockedAt;
        /// @notice Asset version the bond was taken under.
        uint8 assetVersion;
    }

    /// @notice A payment token and Compact position the escrow rotated away from. Locks and bonds taken
    ///         under it keep withdrawing from it.
    struct IntexAuctionAssetVersion {
        /// @notice The Compact the position lives in.
        address compact;
        /// @notice Payment token the position holds.
        IERC20 asset;
        /// @notice The Compact resource lock id of the position.
        uint256 lockId;
        /// @notice Payment-token units in one protocol unit, fixed when the token was wired.
        uint256 assetScale;
    }

    // --- Events ---

    /// @notice Emitted at the reveal, when the escrow locks the asset of the bid, in the asset scale.
    event IntexAuctionAssetLocked(
        uint32 indexed emissionDay, address indexed bidder, address asset, uint256 amountMinor
    );

    /// @notice Emitted for each winner, when the escrow charges the lock at the clearing rate. The change
    ///         stays locked.
    event IntexAuctionAssetCharged(
        uint32 indexed emissionDay, address indexed bidder, address asset, uint256 amountMinor
    );

    /// @notice Emitted when the escrow gives asset back: the asset of a bid that lost, or the change of a winner.
    /// @param reason 0 a return or a redeem, 1 the escrow timeout after 4 days.
    event IntexAuctionAssetReturned(
        uint32 indexed emissionDay, address indexed bidder, address asset, uint256 amountMinor, uint8 reason
    );

    /// @notice Emitted at the commit, when the escrow locks the bid bond.
    event IntexAuctionBidBondLocked(
        uint32 indexed emissionDay, address indexed bidder, address asset, uint256 amountMinor
    );

    /// @notice Emitted when the escrow gives the whole bond back.
    /// @param reason 0 a reveal or a cancel, 1 an unlock 24 hours after the reveal end with no reveal,
    ///        2 the escrow timeout after 4 days.
    event IntexAuctionBidBondUnlocked(
        uint32 indexed emissionDay, address indexed bidder, address asset, uint256 amountMinor, uint8 reason
    );

    /// @notice Emitted for each winner, when the escrow creates its IntexAuctionWinNote.
    event IntexAuctionWinNoteCreated(uint32 indexed emissionDay, address indexed bidder, uint32 quantity);

    /// @notice Emitted when the escrow cannot create the note of one winner. It charges nothing and keeps
    ///         the lock.
    event IntexAuctionWinNoteCreateFailed(uint32 indexed emissionDay, address indexed bidder, bytes errorData);

    /// @notice Emitted when the owner sets The Compact and the asset, `initialize` included.
    event IntexAuctionAssetSet(address compact, address asset, uint8 assetDecimals);

    /// @notice Emitted when the owner changes the asset or The Compact. Open locks and bonds keep the old version.
    event IntexAuctionAssetVersionRetired(uint8 indexed version, address compact, address asset, uint256 lockId);

    // --- Errors ---

    /// @notice Zero address provided.
    /// @param f Field name.
    error ZeroAddress(string f);
    /// @notice Zero value provided where non-zero is required.
    /// @param f Field name.
    error ZeroValue(string f);
    /// @notice Payment token reports fewer decimals than the protocol amounts carry.
    /// @param actual Decimals the token reports.
    error IntexAuctionAssetDecimals(uint8 actual);
    /// @notice A protocol amount does not fit `uint128` once scaled to the payment token.
    /// @param amount The scaled amount.
    error IntexAuctionAssetAmountOverflow(uint256 amount);
    /// @notice Bidder already has locked funds for this series.
    error IntexAuctionBidAlreadyLocked();
    /// @notice Lock is not in the active state required for this operation.
    error LockNotActive();
    /// @notice Series escrow has already been finalized.
    error AlreadyFinalized();
    /// @notice A winner's payment at the day's clearing terms would exceed its lock.
    /// @param locked Locked amount.
    /// @param charged Charge the clearing terms work out.
    error ChargeExceedsLock(uint128 locked, uint256 charged);
    /// @notice A refund chunk names a partially filled winner outside the winners it carries.
    /// @param partialIndex Index the partial fill points at.
    /// @param winners Winners the chunk carries.
    error PartialFillOutsidePage(uint16 partialIndex, uint256 winners);
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
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param assetVersion Asset version the day is bound to.
    error DayIntexAuctionAssetRetired(uint32 emissionDay, uint8 assetVersion);
    /// @notice A winner's lock holds no change to refund.
    error NothingToReturn();
    /// @notice A refund was claimed before the safety window elapsed.
    /// @param returnableAt Earliest unix-seconds timestamp the refund can be claimed at.
    /// @param nowTs Current block timestamp.
    error IntexAuctionAssetNotYetReturnable(uint32 returnableAt, uint32 nowTs);
    /// @notice `lockIntexAuctionBidBond` called while the bidder already holds a live bond for the series.
    error IntexAuctionBidBondAlreadyLocked();
    /// @notice No live commit bond exists for the series/bidder pair.
    error IntexAuctionBidBondNotFound();
    /// @notice `unlockIntexAuctionBidBondAfterTimeout` was called before the escrow-local safety window elapsed.
    /// @param unlockableAt Earliest unix-seconds timestamp the bond can be claimed at.
    /// @param nowTs Current block timestamp.
    error IntexAuctionBidBondEscrowTimeoutNotOver(uint32 unlockableAt, uint32 nowTs);
    /// @notice `caller` is not the contract this entry point belongs to.
    error UnauthorizedCaller(address caller);

    // --- Admin ---

    /// @notice Set The Compact and the payment token of the locks to come.
    /// @dev Rotating `_asset` or `_compact` retires the active asset under its version.
    ///      Locks and bonds taken under it keep withdrawing from it.
    /// @param _compact The Compact contract address.
    /// @param _asset Active payment-token address.
    function setIntexAuctionAsset(address _compact, address _asset) external;

    // --- Auction Integration ---

    /// @notice Lock funds for a bid during the reveal stage. Callable only by the IntexAuction contract.
    /// @dev The bidder must approve this contract to spend `asset` beforehand. The escrow
    ///      locks `quantity * basis * bidRate / 1e6`, scaled to the active payment token.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @param basis Per-Intex escrow basis: the day's PROMIS load, in protocol units.
    /// @param bidRate Bid rate (1e6 fixed-point).
    /// @param quantity Intex units the bid asked for.
    function lockIntexAuctionAsset(uint32 emissionDay, address bidder, uint128 basis, uint32 bidRate, uint16 quantity)
        external;

    /// @notice Lock the commit-entry bond at `commitIntexAuctionBid`. Callable only by the IntexAuction contract.
    /// @dev The bidder must approve this contract to spend `asset` beforehand. The Compact
    ///      holds the bond under the same lock id as bid escrow.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address the bond is taken from (and later returned to).
    /// @param amount Protocol bond amount (the day's `bidBondMinor`), scaled to the active payment token.
    function lockIntexAuctionBidBond(uint32 emissionDay, address bidder, uint128 amount) external;

    /// @notice Return a live commit bond to its owner. Callable only by the IntexAuction contract
    ///         (reveal, cancel, and the auction-side stage-aware path).
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder whose bond is returned.
    /// @param reason 0 a reveal or a cancel, 1 an unlock with no reveal.
    function unlockIntexAuctionBidBond(uint32 emissionDay, address bidder, uint8 reason) external;

    /// @notice Redeem a winner's WinNote: pay the change it still holds and return the units won.
    ///         Callable only by the IntexAuction contract, which issues the units.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Winner whose WinNote is redeemed.
    /// @return units Units the winner won.
    function redeemWinNote(uint32 emissionDay, address bidder) external returns (uint16 units);

    /// @notice Active payment token used for bid escrow (WCOEN).
    function intexAuctionAsset() external view returns (IERC20);

    /// @notice Payment token of the asset a day locked under. Its proceeds leave in this token.
    /// @param emissionDay Emission day (yyyymmdd).
    function dayIntexAuctionAsset(uint32 emissionDay) external view returns (IERC20);

    /// @notice IntexAuctionTargetRouter: the only caller of `chargeIntexAuctionAsset` and the
    ///         recipient of the proceeds it charges.
    function intexAuctionTargetRouter() external view returns (address);

    // --- Bridge Finalization ---

    /// @notice Apply one refund chunk: take each winner's payment at the day's clearing terms and
    ///         mark the rest of its lock owed. This function pays nothing to bidders. Every
    ///         bidder collects through the auction's `returnIntexAuctionAsset`. A bidder that the day's chunks
    ///         never name lost.
    /// @dev A winner's lock leaves `Locked` on its first chunk, so none pays twice. `completesDay`
    ///      closes the day.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param winners Winners on this chain in the chunk.
    /// @param partialIndex Index of the partially filled winner. The function reads it only when
    ///        `partialWon` is non-zero.
    /// @param partialWon Units the partially filled winner received. Zero when the chunk has none.
    /// @param clearingRate The day's clearing rate.
    /// @param basis The day's escrow basis.
    /// @param completesDay Whether this is the day's last chunk.
    /// @return totalPaid Proceeds transferred to the caller for cross-chain routing to the origin.
    function chargeIntexAuctionAsset(
        uint32 emissionDay,
        address[] calldata winners,
        uint16 partialIndex,
        uint16 partialWon,
        uint64 clearingRate,
        uint128 basis,
        bool completesDay
    ) external returns (uint128 totalPaid);

    // --- Recovery ---

    /// @notice Pay `bidder` what its lock owes. Callable only by the IntexAuction contract.
    ///         - A winner collects its change at once. The lock keeps the units won for `redeemWinNote`.
    ///         - A bidder that the finalized day never named collects its full principal at once.
    ///         - A bidder whose day never finalized collects its full principal after
    ///           `UNFINALIZED_RETURN_DELAY`.
    ///         A lock already gone on a finalized day pays nothing and reports closed.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address whose locked principal is being claimed.
    /// @return closed Whether the lock is gone after the call.
    /// @return finalized Whether the day is finalized.
    function returnIntexAuctionAsset(uint32 emissionDay, address bidder) external returns (bool closed, bool finalized);

    /// @notice Escrow-local safety valve for a lock stranded past `ASSET_ESCROW_TIMEOUT` (e.g. the
    ///         auction contract stopped working). It is time-based only and never consults the
    ///         auction. It pays the stored `bidder`, not `msg.sender`, and only money: a winner's
    ///         units stay in its lock.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder whose lock is being claimed.
    function returnIntexAuctionAssetAfterTimeout(uint32 emissionDay, address bidder) external;

    /// @notice `returnIntexAuctionAssetAfterTimeout` was called before the escrow-local safety window elapsed.
    /// @param returnableAt Earliest unix-seconds timestamp the refund can be claimed at.
    /// @param nowTs Current block timestamp.
    error IntexAuctionAssetEscrowTimeoutNotOver(uint32 returnableAt, uint32 nowTs);

    /// @notice Escrow-local safety valve for a commit bond stranded past
    ///         `BID_BOND_ESCROW_TIMEOUT` (e.g. the auction contract was rotated away while the
    ///         bond was live). It is time-based only and never consults the auction. It pays the
    ///         stored `bidder`, not `msg.sender`. The stage-aware fast path lives on IntexAuction.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder whose bond is being claimed.
    function unlockIntexAuctionBidBondAfterTimeout(uint32 emissionDay, address bidder) external;

    // --- Views ---

    /// @notice Get bid lock information.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address whose lock is being read.
    /// @return lock The stored `IntexAuctionBidLock` record for the series/bidder pair.
    function getIntexAuctionBidLock(uint32 emissionDay, address bidder)
        external
        view
        returns (IntexAuctionBidLock memory lock);

    /// @notice What the auction's `returnIntexAuctionAsset` would pay `bidder` for the day, and from when. Zero once nothing is left to
    ///         claim: a settled or claimed lock is gone.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @return amount Refund the claim would pay.
    /// @return returnableAt Earliest unix-seconds timestamp the claim succeeds at.
    function returnableAmount(uint32 emissionDay, address bidder)
        external
        view
        returns (uint128 amount, uint32 returnableAt);

    /// @notice Get commit bond information. A zero `amount` means no live bond.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address whose bond is being read.
    /// @return bond The stored `IntexAuctionBidBond` record for the series/bidder pair.
    function getIntexAuctionBidBond(uint32 emissionDay, address bidder)
        external
        view
        returns (IntexAuctionBidBond memory bond);

    /// @notice Get series escrow status.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @return hasLocks True if the series ever took a lock. Claimed locks do not lower it.
    /// @return isFinalized True if the series escrow is finalized.
    /// @return totalLocked Total payment-token currently locked for the series.
    function auctionStatus(uint32 emissionDay)
        external
        view
        returns (bool hasLocks, bool isFinalized, uint128 totalLocked);

    /// @notice Version number of the active asset.
    function currentIntexAuctionAssetVersion() external view returns (uint8 version);

    /// @notice The asset a version refers to. The active version reads the live wiring.
    /// @param version Asset version.
    /// @return asset The Compact, payment token, lock id and payment scale of that version.
    function getIntexAuctionAssetVersion(uint8 version) external view returns (IntexAuctionAssetVersion memory asset);
}
