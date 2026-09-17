// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IERC20Metadata} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {IERC6909} from "@openzeppelin/contracts/interfaces/IERC6909.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";
import {ITheCompact} from "../vendor/the-compact/interfaces/ITheCompact.sol";
import {IAllocator} from "../vendor/the-compact/interfaces/IAllocator.sol";
import {Scope} from "../vendor/the-compact/types/Scope.sol";
import {ResetPeriod} from "../vendor/the-compact/types/ResetPeriod.sol";

/**
 * @title EscrowAdapter
 * @author Outbe
 * @notice Adapter contract for managing auction bid escrow via The Compact protocol.
 * @dev UUPS upgradeable: deployed behind an ERC1967 proxy, configured via `initialize`.
 *      Integrates with The Compact for fund locking and handles auction finalization.
 *      EscrowAdapter acts as SPONSOR (owns ERC6909 in Compact) and ALLOCATOR (attest);
 *      both roles bind to the proxy address.
 *      All escrow state is keyed by `worldwideDay` (uint32).
 */
contract EscrowAdapter is
    AccessControlUpgradeable,
    ReentrancyGuardTransient,
    UUPSUpgradeable,
    IEscrowAdapter,
    IAllocator
{
    using SafeERC20 for IERC20;

    // Roles
    /// @notice Role identifier for bridge operations (finalization).
    bytes32 public constant RELAYER_ROLE = keccak256("RELAYER_ROLE");
    /// @notice Role identifier for auction contract integration.
    bytes32 public constant AUCTION_ROLE = keccak256("AUCTION_ROLE");

    /// @notice Safety window before the bidder of a never-finalized series can `claimRefund` the
    ///         full principal, anchored at the lock's `lockedAt`. MUST exceed the longest
    ///         legitimate finalization latency (settlement window + cross-chain delivery).
    uint32 public constant UNFINALIZED_REFUND_DELAY = 72 hours;

    /// @notice Settling window after `finalizeAuction` for a bidder whose instruction failed, anchored
    ///         at `finalizedAt`. A day finalizes only once every chunk has landed, so a failed
    ///         instruction is final: the wait is a cushion, not a window for anyone to act in.
    uint32 public constant POST_FINALIZE_REFUND_DELAY = 72 hours;

    /// @notice Escrow-local safety window on `claimAbandonedCommitBond`, anchored at the bond's
    ///         `lockedAt`. Deliberately time-only (never consults the auction) so a bond survives
    ///         an auction-contract rotation. MUST exceed the auction-side no-reveal gate
    ///         (`revealEnd + UNREVEALED_BOND_LOCK_PERIOD`), which holds while auction schedules span
    ///         less than 29 days (daily series span ~2).
    uint32 public constant COMMIT_BOND_ABANDON_DELAY = 30 days;

    /// @notice Decimals every payment token must report.
    uint8 public constant PAYMENT_TOKEN_DECIMALS = 18;

    /// @notice Canonical dead address receiving burned proceeds (the payment token has no burn()).
    address public constant BURN_ADDRESS = 0x000000000000000000000000000000000000dEaD;

    /// @custom:storage-location erc7201:outbe.intex.EscrowAdapter
    struct EscrowAdapterStorage {
        /// @dev IntexAuction contract address.
        address intexAuctionContract;
        /// @dev The Compact contract address.
        ITheCompact compact;
        /// @dev Active payment token used for bid escrow.
        IERC20 paymentToken;
        /// @dev The Compact resource lock ID for our deposits.
        uint256 lockId;
        /// @dev Allocator ID from __registerAllocator.
        uint96 allocatorId;
        /// @dev Lock tag (allocatorId + scope + reset period) for deposits.
        bytes12 lockTag;
        /// @dev Bid locks: worldwideDay => bidder => BidLock.
        mapping(uint32 worldwideDay => mapping(address bidder => BidLock)) bidLocks;
        /// @dev Per-series escrow state.
        mapping(uint32 worldwideDay => AuctionEscrowState) auctionEscrowState;
        /// @dev Commit-entry bonds: worldwideDay => bidder => CommitBond.
        mapping(uint32 worldwideDay => mapping(address bidder => CommitBond)) commitBonds;
        /// @dev Recipient of finalized auction proceeds (the router routing them cross-chain).
        address proceedsRecipient;
        /// @dev Assets rotated away from, keyed by the version they were active under.
        mapping(uint8 version => AssetVersion) assetVersions;
        /// @dev Version of the active asset held in `compact`, `paymentToken` and `lockId`.
        uint8 currentAssetVersion;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.EscrowAdapter")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x9dc6707131c30ec20e38ebcfbc4641faad640e3439439d400ea9dd2fe8f83a00;

    function _s() private pure returns (EscrowAdapterStorage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
    }

    /// @notice Initializes the proxy with its role holders.
    /// @param defaultAdmin Receiver of `DEFAULT_ADMIN_ROLE`.
    function initialize(address defaultAdmin) external initializer {
        if (defaultAdmin == address(0)) revert ZeroAddress("defaultAdmin");

        __AccessControl_init();

        _grantRole(DEFAULT_ADMIN_ROLE, defaultAdmin);
    }

    /// @dev Upgrades are gated by the admin role.
    /// @param newImplementation Address of the implementation the proxy switches to.
    // solhint-disable-next-line no-empty-blocks
    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    // --- Storage getters ---
    /// @notice IntexAuction contract address.
    /// @return The wired auction contract.
    function intexAuctionContract() external view returns (address) {
        return _s().intexAuctionContract;
    }

    /// @notice The Compact contract address.
    /// @return The wired Compact instance.
    function compact() external view returns (ITheCompact) {
        return _s().compact;
    }

    /// @notice Active payment token used for bid escrow.
    /// @return The wired payment token.
    function paymentToken() external view override returns (IERC20) {
        return _s().paymentToken;
    }

    /// @inheritdoc IEscrowAdapter
    function proceedsRecipient() external view override returns (address) {
        return _s().proceedsRecipient;
    }

    /// @inheritdoc IEscrowAdapter
    function setProceedsRecipient(address recipient) external override onlyRole(DEFAULT_ADMIN_ROLE) {
        if (recipient == address(0)) revert ZeroAddress("recipient");
        _s().proceedsRecipient = recipient;
        emit ProceedsRecipientSet(recipient);
    }

    /// @notice The Compact resource lock ID for our deposits.
    /// @return The lock id (zero before the first deposit).
    function lockId() external view returns (uint256) {
        return _s().lockId;
    }

    /// @notice Allocator ID from `__registerAllocator`.
    /// @return The registered allocator id (zero before wiring).
    function allocatorId() external view returns (uint96) {
        return _s().allocatorId;
    }

    /// @notice Lock tag (allocatorId + scope + reset period) for deposits.
    /// @return The lock tag derived at allocator registration.
    function lockTag() external view returns (bytes12) {
        return _s().lockTag;
    }

    /// @notice Bid lock record for a bidder within a series. Flattened to match the original
    ///         public-mapping getter ABI.
    function bidLocks(uint32 worldwideDay, address bidder)
        external
        view
        returns (uint128 lockedAmount, uint32 lockedAt, LockStatus status, uint128 failedRefund, bool splitRecorded)
    {
        BidLock storage l = _s().bidLocks[worldwideDay][bidder];
        return (l.lockedAmount, l.lockedAt, l.status, l.failedRefund, l.splitRecorded);
    }

    /// @notice Per-series escrow state. Flattened to match the original public-mapping getter ABI.
    function auctionEscrowState(uint32 worldwideDay)
        external
        view
        returns (uint128 totalLocked, uint32 lockCount, uint32 finalizedAt, bool finalized)
    {
        AuctionEscrowState storage e = _s().auctionEscrowState[worldwideDay];
        return (e.totalLocked, e.lockCount, e.finalizedAt, e.finalized);
    }

    // --- Admin ---
    /// @inheritdoc IEscrowAdapter
    function wire(address _intexAuction, address _compact, address _paymentToken)
        external
        override
        onlyRole(DEFAULT_ADMIN_ROLE)
    {
        if (_intexAuction == address(0)) revert ZeroAddress("intexAuction");
        if (_compact == address(0)) revert ZeroAddress("compact");
        if (_paymentToken == address(0)) revert ZeroAddress("paymentToken");

        uint8 tokenDecimals = IERC20Metadata(_paymentToken).decimals();
        if (tokenDecimals != PAYMENT_TOKEN_DECIMALS) revert PaymentTokenDecimals(tokenDecimals);

        EscrowAdapterStorage storage $ = _s();

        // A rotation retires the active asset under its version: locks and bonds taken under it keep
        // withdrawing from it, and the first deposit under the new token/Compact bootstraps its own lock.
        bool rotatingPaymentToken = address($.paymentToken) != address(0) && _paymentToken != address($.paymentToken);
        bool rotatingCompact = address($.compact) != address(0) && _compact != address($.compact);
        if (rotatingPaymentToken || rotatingCompact) {
            uint8 retired = $.currentAssetVersion;
            $.assetVersions[retired] =
                AssetVersion({compact: address($.compact), paymentToken: $.paymentToken, lockId: $.lockId});
            $.currentAssetVersion = retired + 1;
            emit AssetRetired(retired, address($.compact), address($.paymentToken), $.lockId);
            $.lockId = 0;
            // A new Compact needs its own allocator registration; drop the stale allocatorId/lockTag.
            if (rotatingCompact) {
                $.allocatorId = 0;
                $.lockTag = bytes12(0);
            }
        }

        // Revoke role from the previous auction if rewiring.
        if ($.intexAuctionContract != address(0)) {
            _revokeRole(AUCTION_ROLE, $.intexAuctionContract);
        }

        // Capture the pre-rotation dependencies so `Wired` is log-reconstructible (old+new).
        address intexAuctionOld = $.intexAuctionContract;
        address compactOld = address($.compact);
        address paymentTokenOld = address($.paymentToken);

        $.intexAuctionContract = _intexAuction;
        $.compact = ITheCompact(_compact);
        $.paymentToken = IERC20(_paymentToken);

        _grantRole(AUCTION_ROLE, _intexAuction);
        $.paymentToken.forceApprove(_compact, 0);
        $.paymentToken.forceApprove(_compact, type(uint256).max);

        // CEI deviation: allocatorId / lockTag depend on __registerAllocator's return.
        // Admin-only; a re-entrant `compact` lacks DEFAULT_ADMIN_ROLE, so re-entry can't reach here.
        if ($.allocatorId == 0) {
            // aderyn-fp-next-line(reentrancy-state-change)
            $.allocatorId = $.compact.__registerAllocator(address(this), "");
            $.lockTag = _buildLockTag($.allocatorId, Scope.ChainSpecific, ResetPeriod.OneMinute);
        }

        emit Wired(intexAuctionOld, _intexAuction, compactOld, _compact, paymentTokenOld, _paymentToken);
    }

    // --- IAllocator Implementation ---
    /// @inheritdoc IAllocator
    function attest(address _operator, address _from, address _to, uint256 id, uint256 _amount)
        external
        view
        override
        returns (bytes4)
    {
        if (id != _s().lockId) revert UnexpectedLockId(id);
        return IAllocator.attest.selector;
    }

    /// @inheritdoc IAllocator
    function authorizeClaim(
        bytes32 _claimHash,
        address _arbiter,
        address _sponsor,
        uint256 _nonce,
        uint256 _expires,
        uint256[2][] calldata _idsAndAmounts,
        bytes calldata _allocatorData
    ) external pure override returns (bytes4) {
        revert ClaimAuthorizationUnsupported();
    }

    /// @inheritdoc IAllocator
    function isClaimAuthorized(
        bytes32 _claimHash,
        address _arbiter,
        address _sponsor,
        uint256 _nonce,
        uint256 _expires,
        uint256[2][] calldata _idsAndAmounts,
        bytes calldata _allocatorData
    ) external pure override returns (bool) {
        return false;
    }

    // --- Auction Integration ---
    /// @inheritdoc IEscrowAdapter
    function lockFunds(uint32 worldwideDay, address bidder, uint128 amount)
        external
        override
        onlyRole(AUCTION_ROLE)
        nonReentrant
    {
        _validateLockInputs(worldwideDay, bidder, amount);
        _executeLock(worldwideDay, bidder, amount);
    }

    // --- Commit bonds ---
    /// @inheritdoc IEscrowAdapter
    /// @dev Trust boundary: `bidder` is the original `msg.sender` of `IntexAuction.commitBid`,
    ///      forwarded through the `AUCTION_ROLE`-gated entry point (mirrors `lockFunds`).
    function lockCommitBond(uint32 worldwideDay, address bidder, uint128 amount)
        external
        override
        onlyRole(AUCTION_ROLE)
        nonReentrant
    {
        if (worldwideDay == 0) revert ZeroValue("worldwideDay");
        if (bidder == address(0)) revert ZeroAddress("bidder");
        if (amount == 0) revert ZeroValue("amount");
        EscrowAdapterStorage storage $ = _s();
        if ($.commitBonds[worldwideDay][bidder].amount != 0) revert CommitBondAlreadyLocked();

        // CEI deviation mirrors `_executeLock`: the one-time lockId bootstrap inside
        // `_depositToCompact` needs depositERC20's return; nonReentrant covers the deviation.
        // slither-disable-next-line arbitrary-send-erc20
        $.paymentToken.safeTransferFrom(bidder, address(this), amount);
        _depositToCompact(amount);

        $.commitBonds[worldwideDay][bidder] =
            CommitBond({amount: amount, lockedAt: uint32(block.timestamp), assetVersion: $.currentAssetVersion});
        emit CommitBondLocked(worldwideDay, bidder, amount);
    }

    /// @inheritdoc IEscrowAdapter
    function releaseCommitBond(uint32 worldwideDay, address bidder)
        external
        override
        onlyRole(AUCTION_ROLE)
        nonReentrant
    {
        _releaseCommitBond(worldwideDay, bidder);
    }

    /// @inheritdoc IEscrowAdapter
    function claimAbandonedCommitBond(uint32 worldwideDay, address bidder) external override nonReentrant {
        CommitBond storage bond = _s().commitBonds[worldwideDay][bidder];
        if (bond.amount == 0) revert CommitBondNotFound();
        uint32 claimableAt = bond.lockedAt + COMMIT_BOND_ABANDON_DELAY;
        if (block.timestamp < claimableAt) revert CommitBondNotYetAbandoned(claimableAt, uint32(block.timestamp));
        _releaseCommitBond(worldwideDay, bidder);
    }

    /// @dev Delete the bond record, withdraw from The Compact, and pay the stored bidder.
    ///      CEI: the delete precedes both external calls; a re-claim reverts `CommitBondNotFound`.
    function _releaseCommitBond(uint32 worldwideDay, address bidder) internal {
        EscrowAdapterStorage storage $ = _s();
        CommitBond memory bond = $.commitBonds[worldwideDay][bidder];
        if (bond.amount == 0) revert CommitBondNotFound();
        uint128 amount = bond.amount;

        // Effects
        delete $.commitBonds[worldwideDay][bidder];

        // Interactions
        _withdrawFromCompact(bond.assetVersion, amount);
        _tokenOf(bond.assetVersion).safeTransfer(bidder, amount);
        emit CommitBondReleased(worldwideDay, bidder, amount);
    }

    // --- Bridge Finalization ---
    /// @inheritdoc IEscrowAdapter
    function finalizeAuction(
        uint32 worldwideDay,
        bytes32 receiveId,
        FinalizationInstruction[] calldata instructions,
        bool completesDay
    ) external override onlyRole(RELAYER_ROLE) nonReentrant returns (uint128 totalPaid) {
        EscrowAdapterStorage storage $ = _s();
        // A closed day takes no more instructions; within an open one each bidder is
        // guarded by its own lock leaving `Locked`.
        if ($.auctionEscrowState[worldwideDay].finalized) {
            revert AlreadyFinalized();
        }
        if (instructions.length == 0) revert ZeroValue("instructions");

        // Effects before any external interaction. The completing set closes the day, which
        // anchors the post-finalize `claimRefund` window (POST_FINALIZE_REFUND_DELAY).
        if (completesDay) {
            $.auctionEscrowState[worldwideDay].finalized = true;
            $.auctionEscrowState[worldwideDay].finalizedAt = uint32(block.timestamp);
        }

        uint128 totalRefunded = 0;
        uint128 totalReleased = 0;
        uint32 bidsProcessed = 0;
        uint32 bidsSettled = 0;

        // Per-bidder try/catch: a single failed iteration emits BidderRefundFailed and the loop
        // continues. The failed bidder's lock stays in `Locked` status (the inner revert rolls
        // back its state writes) and is recovered by the bidder through claimRefund.
        for (uint256 i = 0; i < instructions.length; ++i) {
            FinalizationInstruction calldata inst = instructions[i];
            try this.processFinalizationOne(worldwideDay, receiveId, inst) returns (uint128 released) {
                totalRefunded += inst.refundedAmount;
                totalPaid += inst.paidAmount;
                totalReleased += released;
                ++bidsSettled;
            } catch (bytes memory reason) {
                // Record the intended refund split (in the outer frame, since the failing inner
                // call's writes roll back), but only if it is economically valid. A later
                // claimRefund then pays exactly this, never the full principal. A mismatched split
                // records nothing, so claimRefund stays blocked until the relayer retries.
                BidLock storage failed = $.bidLocks[worldwideDay][inst.bidder];
                if (
                    failed.status == LockStatus.Locked
                        && uint256(inst.refundedAmount) + inst.paidAmount == failed.lockedAmount
                ) {
                    failed.failedRefund = inst.refundedAmount;
                    failed.splitRecorded = true;
                }
                emit BidderRefundFailed(receiveId, worldwideDay, inst.bidder, reason);
            }
            ++bidsProcessed;
        }

        // One write for the chunk: the loop only counts locks it actually closed, so a failed instruction
        // leaves the day's total untouched exactly as a per-bidder decrement would have.
        if (totalReleased > 0) $.auctionEscrowState[worldwideDay].totalLocked -= totalReleased;

        emit AuctionEscrowFinalized(receiveId, worldwideDay, totalRefunded, totalPaid, bidsProcessed);
        // Surface a degenerate finalize (every instruction failed) so it is not silently "done".
        if (bidsSettled == 0) emit FinalizationNoOp(worldwideDay, bidsProcessed);

        // Hand proceeds to the configured recipient (the messenger) for cross-chain routing.
        if (totalPaid > 0) {
            address recipient = $.proceedsRecipient;
            if (recipient == address(0)) revert ProceedsRecipientNotSet();
            _tokenOf($.auctionEscrowState[worldwideDay].assetVersion).safeTransfer(recipient, totalPaid);
        }
    }

    /// @notice Self-call helper for `finalizeAuction`'s per-bidder try/catch. Reverts on any
    ///         non-self call. Not part of the public surface - bundled here because Solidity
    ///         `try/catch` only works on external/public function calls.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param receiveId Inbound bridge message id threaded into the emitted events.
    /// @param inst Finalization instruction for the single bidder being processed.
    /// @return released The lock the instruction closed, for the caller to subtract from the day's total.
    function processFinalizationOne(uint32 worldwideDay, bytes32 receiveId, FinalizationInstruction calldata inst)
        external
        returns (uint128 released)
    {
        if (msg.sender != address(this)) revert NotSelf();
        return
            _processFinalizationInstruction(receiveId, worldwideDay, inst.bidder, inst.refundedAmount, inst.paidAmount);
    }

    /// @inheritdoc IEscrowAdapter
    function claimRefund(uint32 worldwideDay, address bidder) external override nonReentrant {
        if (bidder == address(0)) revert ZeroAddress("bidder");

        EscrowAdapterStorage storage $ = _s();
        BidLock storage lock = $.bidLocks[worldwideDay][bidder];
        if (lock.status != LockStatus.Locked) revert LockNotActive();

        AuctionEscrowState storage state = $.auctionEscrowState[worldwideDay];
        (uint128 refund, uint32 claimableAt) = _claimable(state, lock);
        if (block.timestamp < claimableAt) revert RefundNotYetClaimable(claimableAt, uint32(block.timestamp));

        uint128 lockedAmount = lock.lockedAmount;
        uint8 version = state.assetVersion;
        delete $.bidLocks[worldwideDay][bidder];
        state.totalLocked -= lockedAmount;

        _withdrawFromCompact(version, lockedAmount);
        IERC20 token = _tokenOf(version);
        if (refund > 0) {
            token.safeTransfer(bidder, refund);
            emit FundsRefunded(bytes32(0), worldwideDay, bidder, refund);
        }
        // The winning remainder of a recorded split: its proceeds were already routed on Outbe.
        uint128 burn = lockedAmount - refund;
        if (burn > 0) {
            token.safeTransfer(BURN_ADDRESS, burn);
            emit ProceedsBurned(worldwideDay, bidder, burn);
        }
    }

    /// @dev What a live lock is owed and from when. A day that never finalized owes the full principal. On a
    ///      finalized day a lock stays live only when its instruction failed or never came: the recorded split
    ///      where the numbers added up, the full principal where they did not.
    function _claimable(AuctionEscrowState storage state, BidLock storage lock)
        private
        view
        returns (uint128 refund, uint32 claimableAt)
    {
        if (!state.finalized) return (lock.lockedAmount, lock.lockedAt + UNFINALIZED_REFUND_DELAY);
        return
            (lock.splitRecorded ? lock.failedRefund : lock.lockedAmount, state.finalizedAt + POST_FINALIZE_REFUND_DELAY);
    }

    // --- Views ---
    /// @inheritdoc IEscrowAdapter
    function getBidLock(uint32 worldwideDay, address bidder) external view override returns (BidLock memory) {
        return _s().bidLocks[worldwideDay][bidder];
    }

    /// @inheritdoc IEscrowAdapter
    function getClaimableRefund(uint32 worldwideDay, address bidder)
        external
        view
        override
        returns (uint128 amount, uint32 claimableAt)
    {
        EscrowAdapterStorage storage $ = _s();
        BidLock storage lock = $.bidLocks[worldwideDay][bidder];
        if (lock.status != LockStatus.Locked) return (0, 0);
        return _claimable($.auctionEscrowState[worldwideDay], lock);
    }

    /// @inheritdoc IEscrowAdapter
    function getCommitBond(uint32 worldwideDay, address bidder) external view override returns (CommitBond memory) {
        return _s().commitBonds[worldwideDay][bidder];
    }

    /// @inheritdoc IEscrowAdapter
    function getAuctionStatus(uint32 worldwideDay)
        external
        view
        override
        returns (bool hasLocks, bool isFinalized, uint128 totalLocked)
    {
        AuctionEscrowState memory state = _s().auctionEscrowState[worldwideDay];
        return (state.lockCount > 0, state.finalized, state.totalLocked);
    }

    /// @inheritdoc IEscrowAdapter
    function hasOutstandingLocks() external view override returns (bool) {
        EscrowAdapterStorage storage $ = _s();
        if ($.lockId == 0) return false;
        return IERC6909(address($.compact)).balanceOf(address(this), $.lockId) != 0;
    }

    /// @inheritdoc IEscrowAdapter
    function currentAssetVersion() external view override returns (uint8) {
        return _s().currentAssetVersion;
    }

    /// @inheritdoc IEscrowAdapter
    function getAssetVersion(uint8 version) external view override returns (AssetVersion memory) {
        EscrowAdapterStorage storage $ = _s();
        if (version == $.currentAssetVersion) {
            return AssetVersion({compact: address($.compact), paymentToken: $.paymentToken, lockId: $.lockId});
        }
        return $.assetVersions[version];
    }

    // --- Internal helpers ---
    /// @notice Validate lock inputs before any state write.
    /// @dev Rejects a zero `worldwideDay`, zero `bidder`, zero `amount`, and a bidder that already
    ///      holds a non-`None` lock for the series.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @param amount Amount to lock.
    function _validateLockInputs(uint32 worldwideDay, address bidder, uint128 amount) internal view {
        // Cheap sanity floor: the AUCTION_ROLE gate already guarantees a real, stage-gated series,
        // but a zero id is obviously bogus and is rejected before any state write.
        if (worldwideDay == 0) revert ZeroValue("worldwideDay");
        if (bidder == address(0)) revert ZeroAddress("bidder");
        if (amount == 0) revert ZeroValue("amount");
        if (_s().bidLocks[worldwideDay][bidder].status != LockStatus.None) {
            revert BidAlreadyLocked();
        }
    }

    /// @notice Execute the lock operation - transfer from the bidder and deposit to The Compact.
    /// @dev Bootstraps `lockId` and forced withdrawal on the first deposit, then records the
    ///      `BidLock` and bumps the per-series escrow stats.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @param amount Amount to lock.
    /// @dev Trust boundary: `bidder` is the original `msg.sender` of `IntexAuction.revealBid`,
    ///      forwarded through the `AUCTION_ROLE`-gated `lockFunds` entry point. Safety relies
    ///      on `AUCTION_ROLE` only ever being granted to the wired `IntexAuction` contract.
    function _executeLock(uint32 worldwideDay, address bidder, uint128 amount) internal {
        EscrowAdapterStorage storage $ = _s();
        AuctionEscrowState storage state = $.auctionEscrowState[worldwideDay];
        // A day's locks share one asset, so its proceeds leave in one withdrawal.
        uint8 version = $.currentAssetVersion;
        if (state.lockCount != 0 && state.assetVersion != version) {
            revert DayAssetRetired(worldwideDay, state.assetVersion);
        }

        // CEI deviation: only the one-time lockId bootstrap needs depositERC20's return before
        // writing. Per-call bidLocks / auctionEscrowState writes follow for locality and could
        // move above; nonReentrant on every outer entrypoint covers the deviation regardless.
        // slither-disable-next-line arbitrary-send-erc20
        $.paymentToken.safeTransferFrom(bidder, address(this), amount);
        _depositToCompact(amount);

        // Store lock data.
        $.bidLocks[worldwideDay][bidder] = BidLock({
            lockedAmount: amount,
            lockedAt: uint32(block.timestamp),
            status: LockStatus.Locked,
            failedRefund: 0,
            splitRecorded: false
        });

        // Update series escrow stats.
        state.assetVersion = version;
        ++state.lockCount;
        state.totalLocked += amount;

        emit FundsLocked(worldwideDay, bidder, amount);
    }

    /// @notice Process a single finalization instruction: validate the split, mark the lock
    ///         `Finalized`, refund the bidder, and collect the paid portion for the caller to route.
    /// @dev Reverts `AmountMismatch` when `refundedAmount + paidAmount != lockedAmount`.
    /// @param receiveId Inbound bridge message id threaded into the emitted refund/payout events.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @param refundedAmount Amount to refund to the bidder.
    /// @param paidAmount Auction proceeds left in this contract for the caller to route.
    /// @return released The lock the instruction closed. The caller decrements the day's total: the batch
    ///         path accumulates and writes once after its loop, so a chunk pays one write rather than one
    ///         per bidder.
    function _processFinalizationInstruction(
        bytes32 receiveId,
        uint32 worldwideDay,
        address bidder,
        uint128 refundedAmount,
        uint128 paidAmount
    ) internal returns (uint128 released) {
        if (bidder == address(0)) revert ZeroAddress("bidder");

        EscrowAdapterStorage storage $ = _s();
        BidLock storage lock = $.bidLocks[worldwideDay][bidder];
        if (lock.status != LockStatus.Locked) revert LockNotActive();

        // Validate the refund + payout split matches the locked amount. Sum in uint256 so a
        // mismatch is surfaced rather than silently wrapping when the split exceeds the lock.
        uint128 lockedAmount = lock.lockedAmount;
        uint256 total = uint256(refundedAmount) + paidAmount;
        if (total != lockedAmount) {
            revert AmountMismatch(lockedAmount, uint128(total));
        }

        // CEI ok: state writes below precede every external call in this function.
        lock.status = LockStatus.Finalized;
        released = lockedAmount;

        // Interactions
        uint8 version = $.auctionEscrowState[worldwideDay].assetVersion;
        _withdrawFromCompact(version, lockedAmount);

        if (refundedAmount > 0) {
            _tokenOf(version).safeTransfer(bidder, refundedAmount);
            emit FundsRefunded(receiveId, worldwideDay, bidder, refundedAmount);
        }

        // Paid portion stays in this contract; the caller routes it.
    }

    /// @notice Deposit `amount` of the payment token into The Compact (we receive ERC6909 tokens).
    /// @dev Bootstraps `lockId` and enables forced withdrawal on the first-ever deposit. The
    ///      returned `withdrawableAt` is informational; withdrawals invoke forcedWithdrawal directly.
    /// @param amount Amount to deposit.
    function _depositToCompact(uint128 amount) internal {
        EscrowAdapterStorage storage $ = _s();
        uint256 returnedLockId = $.compact.depositERC20(address($.paymentToken), $.lockTag, amount, address(this));
        if ($.lockId == 0) {
            $.lockId = returnedLockId;
            // slither-disable-next-line unused-return
            $.compact.enableForcedWithdrawal(returnedLockId);
        }
    }

    /// @notice Withdraw tokens from The Compact via forced withdrawal, out of the position `version` refers to.
    /// @dev Reverts `NoDeposits` if that position never bootstrapped and `ForcedWithdrawalFailed` if the reset
    ///      period has not elapsed (The Compact returns false).
    /// @param version Asset version the funds were deposited under.
    /// @param amount Amount to withdraw.
    function _withdrawFromCompact(uint8 version, uint128 amount) internal {
        EscrowAdapterStorage storage $ = _s();
        (ITheCompact compactOf, uint256 idOf) = version == $.currentAssetVersion
            ? ($.compact, $.lockId)
            : (ITheCompact($.assetVersions[version].compact), $.assetVersions[version].lockId);
        if (idOf == 0) revert NoDeposits();
        bool success = compactOf.forcedWithdrawal(idOf, address(this), amount);
        if (!success) revert ForcedWithdrawalFailed();
    }

    /// @dev Payment token of the asset `version` refers to.
    function _tokenOf(uint8 version) internal view returns (IERC20) {
        EscrowAdapterStorage storage $ = _s();
        return version == $.currentAssetVersion ? $.paymentToken : $.assetVersions[version].paymentToken;
    }

    /// @dev Build the lock tag for The Compact deposits.
    /// @notice Combines allocatorId, scope, and reset period into a single 12-byte identifier.
    ///         This tag is used by The Compact to identify which resource lock to use for deposits.
    /// @param _allocatorId Our allocator ID from The Compact registration.
    /// @param scope Whether the lock is multichain or chain-specific.
    /// @param resetPeriod Time period before forced withdrawal is allowed.
    /// @return lockTag 12-byte identifier used for all deposits.
    function _buildLockTag(uint96 _allocatorId, Scope scope, ResetPeriod resetPeriod) internal pure returns (bytes12) {
        uint256 packed =
            (uint256(uint8(scope)) << 255) | (uint256(uint8(resetPeriod)) << 252) | (uint256(_allocatorId) << 160);
        // forge-lint: disable-next-line(unsafe-typecast) -- intentional truncation to the tag's top 12 bytes
        return bytes12(uint96(packed >> 160));
    }

    /// @notice Check if the contract supports a given interface.
    /// @dev Returns true for `IAllocator` and any interface advertised by `AccessControl`.
    /// @param interfaceId Interface ID to check.
    /// @return True if the interface is supported.
    function supportsInterface(bytes4 interfaceId) public view override(AccessControlUpgradeable) returns (bool) {
        return interfaceId == type(IAllocator).interfaceId || super.supportsInterface(interfaceId);
    }
}
