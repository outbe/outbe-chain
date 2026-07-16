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

    /// @notice Lock status for a bid.
    /// @dev `RefundClaimed` is reached only when a post-finalize `claimRefund` paid the bidder
    ///      their refund portion but could not settle the vault portion in the same transaction
    ///      (the vault deposit reverted). The vault portion stays in The Compact and is recoverable
    ///      via the permissionless `settleVaultOwed`, which then advances the lock to `Finalized`.
    enum LockStatus {
        None,
        Locked,
        Finalized,
        RefundClaimed
    }

    /// @notice Bid lock data stored per series per bidder.
    /// @dev Slot-packed: `lockedAmount` (16B) + `lockedAt` (4B) + `status` (1B) = 21B, one slot;
    ///      `failedRefund` (16B) + `splitRecorded` (1B) = 17B, a second slot.
    struct BidLock {
        /// @notice Amount of payment-token locked.
        uint128 lockedAmount;
        /// @notice Timestamp when the lock was created (UNIX seconds).
        uint32 lockedAt;
        /// @notice Current status of the lock.
        LockStatus status;
        /// @notice Refund-portion of the finalization instruction that failed for this bidder.
        /// @dev Valid only when `splitRecorded` is true. Drives the post-finalize `claimRefund`
        ///      payout so a stranded winner is refunded only what they are owed, not the full lock.
        uint128 failedRefund;
        /// @notice Whether a validated failed split was recorded for this bidder.
        bool splitRecorded;
    }

    /// @notice Finalization instruction for a single bid.
    struct FinalizationInstruction {
        /// @notice Bidder address.
        address bidder;
        /// @notice Amount to refund to the bidder.
        uint128 refundedAmount;
        /// @notice Amount paid out to the vault (winning portion).
        uint128 paidAmount;
    }

    /// @notice Per-series escrow state.
    struct AuctionEscrowState {
        /// @notice Total payment-token currently locked for the series.
        uint128 totalLocked;
        /// @notice Number of bid locks created for the series.
        uint32 lockCount;
        /// @notice Timestamp when `finalizeAuction` flipped `finalized = true` (UNIX seconds).
        /// @dev Drives the post-finalize 7-day window on `claimRefund`. 0 if never finalized.
        uint32 finalizedAt;
        /// @notice Whether the series escrow has been finalized.
        bool finalized;
    }

    /// @notice Commit-entry bond taken at `commitBid` and held until reveal/cancel/claim.
    /// @dev Existence sentinel is `amount > 0`; the record is deleted on release so a
    ///      commit→cancel→commit cycle can re-lock within the same series.
    struct CommitBond {
        /// @notice Amount of payment-token bonded.
        uint128 amount;
        /// @notice Timestamp when the bond was locked (UNIX seconds). Anchors the
        ///         escrow-local `claimAbandonedCommitBond` safety window.
        uint32 lockedAt;
    }

    // --- Events ---

    /// @notice Emitted when funds are locked for a bid during reveal.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose funds were locked.
    /// @param amount Amount of payment-token locked.
    event FundsLocked(uint32 indexed worldwideDay, address indexed bidder, uint128 amount);

    /// @notice Emitted when a commit-entry bond is locked at `commitBid`.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose bond was taken.
    /// @param amount Amount of payment-token bonded.
    event CommitBondLocked(uint32 indexed worldwideDay, address indexed bidder, uint128 amount);

    /// @notice Emitted when a commit-entry bond is returned to its owner (reveal, cancel,
    ///         auction-side claim, or the escrow-local abandoned-bond claim).
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder the bond was returned to.
    /// @param amount Amount of payment-token returned.
    event CommitBondReleased(uint32 indexed worldwideDay, address indexed bidder, uint128 amount);

    /// @notice Emitted when funds are refunded to a bidder.
    /// @param receiveId Inbound bridge message that triggered the refund, or `bytes32(0)` for a
    ///        permissionless `claimRefund` (not bridge-triggered).
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder who received the refund.
    /// @param amount Amount refunded to the bidder.
    event FundsRefunded(bytes32 indexed receiveId, uint32 indexed worldwideDay, address indexed bidder, uint128 amount);

    /// @notice Emitted when funds are paid out to the vault for a winning bid.
    /// @param receiveId Inbound bridge message that triggered the payout.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose winning portion was paid out.
    /// @param amount Amount routed to the vault provider.
    event FundsClaimed(bytes32 indexed receiveId, uint32 indexed worldwideDay, address indexed bidder, uint128 amount);

    /// @notice Emitted when a series escrow is finalized.
    /// @param receiveId Inbound bridge message that triggered finalization.
    /// @param worldwideDay Series identifier.
    /// @param totalRefunded Total refunded to bidders.
    /// @param totalPaid Total paid out to the vault.
    /// @param bidsProcessed Number of bids processed.
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
    /// @param vaultProviderOld Outbe-vault `VaultProvider` address before this wire.
    /// @param vaultProviderNew Outbe-vault `VaultProvider` address after this wire.
    /// @param paymentTokenOld Active payment-token address before this wire.
    /// @param paymentTokenNew Active payment-token address after this wire.
    event Wired(
        address intexAuctionOld,
        address intexAuctionNew,
        address compactOld,
        address compactNew,
        address vaultProviderOld,
        address vaultProviderNew,
        address paymentTokenOld,
        address paymentTokenNew
    );

    /// @notice Emitted when a single bidder's finalization step fails. The lock stays in
    ///         `Locked` status and can be recovered via `retryFinalize` (RELAYER) or `claimRefund`
    ///         (permissionless, after the post-finalize safety window).
    /// @param receiveId Inbound bridge message that triggered the failed finalization.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose finalization step failed.
    /// @param reason Raw revert data from the failed per-bidder finalization call.
    event BidderRefundFailed(bytes32 indexed receiveId, uint32 indexed worldwideDay, address indexed bidder, bytes reason);

    /// @notice Emitted on a successful `retryFinalize` call.
    /// @param receiveId Original inbound bridge message the relayer is retrying for.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose finalization was retried.
    /// @param refundedAmount Amount refunded to the bidder on retry.
    /// @param paidAmount Amount paid out to the vault on retry.
    event BidderRetried(
        bytes32 indexed receiveId,
        uint32 indexed worldwideDay,
        address indexed bidder,
        uint128 refundedAmount,
        uint128 paidAmount
    );

    /// @notice Emitted when a post-finalize `claimRefund` refunds the failed bidder their refund
    ///         portion but cannot settle the vault portion in the same transaction (the vault
    ///         deposit reverted). The lock is left in `RefundClaimed` and the payout portion stays
    ///         in The Compact, recoverable via the permissionless `settleVaultOwed`.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose vault portion could not be settled.
    /// @param vaultOwed Payout portion left parked in The Compact.
    event VaultOwedUnsettled(uint32 indexed worldwideDay, address indexed bidder, uint128 vaultOwed);

    /// @notice Emitted when `settleVaultOwed` routes a previously-parked payout portion into the
    ///         vault and advances the lock from `RefundClaimed` to `Finalized`.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose parked vault portion was settled.
    /// @param vaultOwed Payout portion deposited into the vault provider.
    event VaultOwedSettled(uint32 indexed worldwideDay, address indexed bidder, uint128 vaultOwed);

    /// @notice Emitted when `finalizeAuction` settled zero bidders (every instruction failed). The
    ///         series is finalized but degenerate; bidders are recoverable only via `retryFinalize`.
    /// @param worldwideDay Series identifier.
    /// @param bidsProcessed Number of instructions processed, all of which failed.
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
    /// @notice Bidder already has locked funds for this series.
    error BidAlreadyLocked();
    /// @notice Lock is not in the active state required for this operation.
    error LockNotActive();
    /// @notice Series escrow has already been finalized.
    error AlreadyFinalized();
    /// @notice Refund + payout amounts do not match the locked amount.
    /// @param locked Locked amount.
    /// @param requested Requested total.
    error AmountMismatch(uint128 locked, uint128 requested);
    /// @notice `attest` was called for a lock id that does not match this escrow's `lockId`.
    /// @param id The unexpected lock id passed to `attest`.
    error UnexpectedLockId(uint256 id);
    /// @notice `authorizeClaim` is not a supported allocator operation on this escrow.
    error ClaimAuthorizationUnsupported();
    /// @notice The Compact forced withdrawal returned false (e.g. the reset period has not elapsed).
    error ForcedWithdrawalFailed();
    /// @notice No deposits made yet (lock id not set).
    error NoDeposits();
    /// @notice Cannot rotate the active payment token (or Compact) while funds remain locked.
    /// @dev The ERC6909 balance returned by The Compact is `uint256`; surfacing the full width
    ///      avoids silent truncation in the revert payload if the balance ever exceeds `uint128`.
    /// @param outstanding Total balance still held in The Compact for live locks.
    error LiveLocksOutstanding(uint256 outstanding);
    /// @notice Self-call helper invoked by an external caller (only `address(this)` is allowed).
    error NotSelf();
    /// @notice Finalization produced proceeds but no recipient is configured.
    error ProceedsRecipientNotSet();
    /// @notice `retryFinalize` invoked before the series was finalized at least once.
    /// @param worldwideDay Series identifier.
    error NotFinalizedYet(uint32 worldwideDay);
    /// @notice `claimRefund` was called before the safety window elapsed.
    /// @param claimableAt Earliest unix-seconds timestamp the refund can be claimed at.
    /// @param now_ Current block timestamp.
    error RefundNotYetClaimable(uint32 claimableAt, uint32 now_);
    /// @notice Post-finalize `claimRefund` has no validated split (bidder omitted or mismatched).
    ///         Reverts only until `ABANDON_DELAY`, after which the full principal is refundable.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose split was never recorded.
    error SplitNotRecorded(uint32 worldwideDay, address bidder);
    /// @notice `settleVaultOwed` called for a lock that has no parked vault portion pending (the
    ///         lock is not in `RefundClaimed` state).
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose lock was targeted.
    error NoPendingVaultOwed(uint32 worldwideDay, address bidder);
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
    /// @dev After the first wiring, rotating `_paymentToken` or `_compact` reverts with
    ///      `LiveLocksOutstanding` while any locked balance remains in The Compact.
    /// @dev Deployment-order requirement (handled by the outbe-vault owner, not this contract):
    ///      `VaultProvider.addVault(vaultV2)` + `addLiquiditySource(this, IntexBidPrice)` must
    ///      land before our `wire(...)` and any subsequent `finalizeAuction()` paid-portion call.
    /// @param _intexAuction IntexAuction contract address.
    /// @param _compact The Compact contract address.
    /// @param _vaultProvider Outbe-vault `VaultProvider` address (router for liquidity into the
    ///        underlying `VaultV2`). Winner principal at finalization is routed through
    ///        `vaultProvider.depositLiquidity(paymentToken, paidAmount)`.
    /// @param _paymentToken Active payment-token address.
    function wire(address _intexAuction, address _compact, address _vaultProvider, address _paymentToken) external;

    // --- Auction Integration ---

    /// @notice Lock funds for a bid during the reveal stage. Callable only by the IntexAuction contract.
    /// @dev The bidder must approve this contract to spend `paymentToken` beforehand.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder address.
    /// @param amount Amount to lock (`intexQuantity * intexBidPrice`).
    function lockFunds(uint32 worldwideDay, address bidder, uint128 amount) external;

    /// @notice Lock the commit-entry bond at `commitBid`. Callable only by the IntexAuction contract.
    /// @dev The bidder must approve this contract to spend `paymentToken` beforehand. The bond is
    ///      held in The Compact under the same lock id as bid escrow.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder address the bond is taken from (and later returned to).
    /// @param amount Bond amount (the series' `commitBondMinor`).
    function lockCommitBond(uint32 worldwideDay, address bidder, uint128 amount) external;

    /// @notice Return a live commit bond to its owner. Callable only by the IntexAuction contract
    ///         (reveal, cancel, and the auction-side stage-aware claim path).
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose bond is returned.
    function releaseCommitBond(uint32 worldwideDay, address bidder) external;

    /// @notice Active payment token used for bid escrow (WCOEN).
    function paymentToken() external view returns (IERC20);

    /// @notice Recipient of finalized auction proceeds (the router routing them cross-chain).
    function proceedsRecipient() external view returns (address);

    /// @notice Set the recipient of finalized auction proceeds.
    function setProceedsRecipient(address recipient) external;

    // --- Bridge Finalization ---

    /// @notice Finalize a series escrow with per-bidder refund/payout instructions.
    /// @param worldwideDay Series identifier.
    /// @param receiveId Inbound bridge message id that carried the refund instructions; threaded into the
    ///        emitted events so an indexer can attribute each fund movement to its source packet.
    /// @param instructions Array of finalization instructions per bidder.
    /// @return totalPaid Proceeds transferred to the caller for cross-chain routing to creators.
    function finalizeAuction(uint32 worldwideDay, bytes32 receiveId, FinalizationInstruction[] calldata instructions)
        external
        returns (uint128 totalPaid);

    // --- Recovery ---

    /// @notice Permissionless principal refund: when the relayer never finalizes, or — for a finalized
    ///         series — once `ABANDON_DELAY` elapses for an omitted/mismatched `Locked` bidder. Pays the
    ///         stored `bidder`, not `msg.sender`.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder address whose locked principal is being claimed.
    function claimRefund(uint32 worldwideDay, address bidder) external;

    /// @notice Per-bidder retry after `finalizeAuction` left a bidder in `BidderRefundFailed`.
    ///         Gated by `RELAYER_ROLE` (operational, not admin). Lets the relayer deliver the
    ///         correct refund/payout split for a failed bidder once the upstream issue is fixed.
    /// @param worldwideDay Series identifier (must be already finalized).
    /// @param receiveId Original inbound bridge message id being retried; threaded into the emitted events.
    /// @param inst Finalization instruction for the single bidder being retried.
    function retryFinalize(uint32 worldwideDay, bytes32 receiveId, FinalizationInstruction calldata inst) external;

    /// @notice Permissionless settlement of a payout portion left parked by a post-finalize
    ///         `claimRefund` (lock in `RefundClaimed`). Withdraws the parked amount from The Compact
    ///         and deposits it into the vault provider, advancing the lock to `Finalized`. The
    ///         amount and destination are fixed by stored lock state — the caller chooses only when.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose parked vault portion is being settled.
    function settleVaultOwed(uint32 worldwideDay, address bidder) external;

    /// @notice Escrow-local safety valve for a commit bond stranded past
    ///         `COMMIT_BOND_ABANDON_DELAY` (e.g. the auction contract was rotated away while the
    ///         bond was live). Time-based only — never consults the auction — and pays the stored
    ///         `bidder`, not `msg.sender`. The stage-aware fast path lives on IntexAuction.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder whose bond is being claimed.
    function claimAbandonedCommitBond(uint32 worldwideDay, address bidder) external;

    // --- Views ---

    /// @notice Get bid lock information.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder address whose lock is being read.
    /// @return lock The stored `BidLock` record for the series/bidder pair.
    function getBidLock(uint32 worldwideDay, address bidder) external view returns (BidLock memory lock);

    /// @notice Get commit bond information. A zero `amount` means no live bond.
    /// @param worldwideDay Series identifier.
    /// @param bidder Bidder address whose bond is being read.
    /// @return bond The stored `CommitBond` record for the series/bidder pair.
    function getCommitBond(uint32 worldwideDay, address bidder) external view returns (CommitBond memory bond);

    /// @notice Get series escrow status.
    /// @param worldwideDay Series identifier.
    /// @return hasLocks True if the series has at least one lock.
    /// @return isFinalized True if the series escrow is finalized.
    /// @return totalLocked Total payment-token currently locked for the series.
    function getAuctionStatus(uint32 worldwideDay)
        external
        view
        returns (bool hasLocks, bool isFinalized, uint128 totalLocked);

    /// @notice True while any lock is still live in The Compact under the active lock id.
    function hasOutstandingLocks() external view returns (bool outstanding);
}
