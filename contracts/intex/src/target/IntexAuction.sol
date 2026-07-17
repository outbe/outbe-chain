// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {EIP712Upgradeable} from "@openzeppelin/contracts-upgradeable/utils/cryptography/EIP712Upgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";

/// @title IntexAuction
/// @author Outbe
/// @notice Commit-reveal auction keyed by `worldwideDay` (uint32, yyyymmdd).
/// @dev UUPS upgradeable: deployed behind an ERC1967 proxy, configured via `initialize`.
///      The schedule is computed on the Outbe side and passed into `auctionStart`.
///      Reveal signatures are EIP-712 typed data under the `IntexAuction` v1 domain,
///      binding both `chainId` and `verifyingContract` (the proxy) and so preventing
///      cross-chain and cross-instance replay.
contract IntexAuction is
    AccessControlUpgradeable,
    ReentrancyGuardTransient,
    EIP712Upgradeable,
    UUPSUpgradeable,
    IIntexAuction
{
    // Roles
    /// @notice Role identifier for bridge operations (stage ops driven by the relayer).
    bytes32 public constant RELAYER_ROLE = keccak256("RELAYER_ROLE");

    /// @notice No-reveal penalty window on the commit bond, anchored at `revealEnd`. A committed
    ///         bidder who never reveals on a green day gets the bond back via `claimCommitBond`
    ///         only after this period; reveal/cancel/red-day return it immediately.
    uint32 public constant COMMIT_BOND_LOCK_PERIOD = 21 days;

    /// @dev EIP-712 type hash for `RevealBid(uint32 worldwideDay,address bidder,uint16 quantity,uint32 bidRate)`.
    bytes32 private constant REVEAL_BID_TYPEHASH =
        keccak256("RevealBid(uint32 worldwideDay,address bidder,uint16 quantity,uint32 bidRate)");

    /// @custom:storage-location erc7201:outbe.intex.IntexAuction
    struct IntexAuctionStorage {
        /// @dev Escrow contract for bid processing.
        IEscrowAdapter escrowContract;
        /// @dev Auction parameters and state, indexed by series id.
        mapping(uint32 worldwideDay => IIntexAuction.AuctionData) auctions;
        /// @dev Live bid counters tracked while the auction runs.
        mapping(uint32 worldwideDay => IIntexAuction.AuctionRunningCounts) auctionRunningCounts;
        /// @dev Committed bid hashes: worldwideDay => bidder => commitHash.
        mapping(uint32 worldwideDay => mapping(address bidder => bytes32 commitHash)) committedBidsByHash;
        /// @dev Bid revealed status: worldwideDay => bidder => revealed.
        mapping(uint32 worldwideDay => mapping(address bidder => bool revealed)) revealedBidsByBidder;
        /// @dev Revealed bids per series.
        mapping(uint32 worldwideDay => IIntexAuction.SubmittedBidData[]) revealedBids;
        /// @dev Cleared marker per series. Set once by `executeAuctionClearing` and the sole
        ///      `Completed`-stage signal — so a no-sale clearing (issuedIntexCount == 0,
        ///      clearingRate may be 0) also reads as Completed, not just a positive-rate sale.
        mapping(uint32 worldwideDay => bool) cleared;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.IntexAuction")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x0db73aff7344f42850665630fc90d6dc1080fdcdb5bb8f56a3fd235fc49b1c00;

    function _s() private pure returns (IntexAuctionStorage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor() {
        _disableInitializers();
    }

    /// @notice Initializes the proxy with its role holders and the EIP-712 domain.
    /// @param defaultAdmin Receiver of `DEFAULT_ADMIN_ROLE`.
    function initialize(address defaultAdmin) external initializer {
        if (defaultAdmin == address(0)) revert ZeroAddress("defaultAdmin");

        __AccessControl_init();
        __EIP712_init("IntexAuction", "1");

        _grantRole(DEFAULT_ADMIN_ROLE, defaultAdmin);
    }

    /// @dev Upgrades are gated by the admin role.
    /// @param newImplementation Address of the implementation the proxy switches to.
    // solhint-disable-next-line no-empty-blocks
    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    // --- Storage getters ---
    /// @notice Escrow contract for bid processing.
    /// @return The wired escrow adapter.
    function escrowContract() external view returns (IEscrowAdapter) {
        return _s().escrowContract;
    }

    /// @notice Auction parameters and state, indexed by series id. Flattened to match the
    ///         original public-mapping getter ABI (nested structs returned as tuples).
    function auctions(uint32 worldwideDay)
        external
        view
        returns (
            IIntexAuction.WorldwideDayState worldwideDayState,
            IIntexAuction.AuctionSchedule memory schedule,
            IIntexAuction.AuctionParams memory params,
            IIntexAuction.AuctionResult memory result
        )
    {
        IIntexAuction.AuctionData storage a = _s().auctions[worldwideDay];
        return (a.worldwideDayState, a.schedule, a.params, a.result);
    }

    /// @notice Live bid counters tracked while the auction runs. Flattened to match the
    ///         original public-mapping getter ABI.
    function auctionRunningCounts(uint32 worldwideDay)
        external
        view
        returns (uint32 committedBidsCount, uint32 revealedBidsCount)
    {
        IIntexAuction.AuctionRunningCounts storage c = _s().auctionRunningCounts[worldwideDay];
        return (c.committedBidsCount, c.revealedBidsCount);
    }

    /// @notice Committed bid hash for a bidder.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @return The stored commit hash (zero when absent).
    function committedBidsByHash(uint32 worldwideDay, address bidder) external view returns (bytes32) {
        return _s().committedBidsByHash[worldwideDay][bidder];
    }

    /// @notice Whether a bidder has revealed for a series.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @return True when the bid was revealed.
    function revealedBidsByBidder(uint32 worldwideDay, address bidder) external view returns (bool) {
        return _s().revealedBidsByBidder[worldwideDay][bidder];
    }

    /// @notice Revealed bid at an index within a series. Flattened to match the original
    ///         public-mapping getter ABI.
    function revealedBids(uint32 worldwideDay, uint256 index)
        external
        view
        returns (address bidderAddress, uint32 intexBidRate, uint32 timestamp, uint16 intexQuantity)
    {
        IIntexAuction.SubmittedBidData storage b = _s().revealedBids[worldwideDay][index];
        return (b.bidderAddress, b.intexBidRate, b.timestamp, b.intexQuantity);
    }

    // --- Admin ---
    /// @inheritdoc IIntexAuction
    function wire(address _escrow) external override onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_escrow == address(0)) revert ZeroAddress("escrowContract");

        IntexAuctionStorage storage $ = _s();
        IEscrowAdapter current = $.escrowContract;
        // Don't rotate away from an escrow that still holds live locks.
        if (address(current) != address(0) && current.hasOutstandingLocks()) revert EscrowHasLiveLocks();

        $.escrowContract = IEscrowAdapter(_escrow);
        emit EscrowWired(address(current), _escrow);
    }

    // --- Lifecycle ---
    /// @inheritdoc IIntexAuction
    function auctionStart(
        uint32 worldwideDay,
        IIntexAuction.AuctionSchedule calldata schedule,
        IIntexAuction.AuctionParams calldata params
    ) external override onlyRole(RELAYER_ROLE) {
        IntexAuctionStorage storage $ = _s();
        // `commitEnd == 0` is the canonical existence sentinel for an auction entry.
        if ($.auctions[worldwideDay].schedule.commitEnd != 0) revert AuctionAlreadyExists();

        // Schedule timestamps must be strictly increasing and the commit stage must end in the future.
        if (
            schedule.commitEnd <= block.timestamp || schedule.revealEnd <= schedule.commitEnd
                || schedule.issuanceEnd <= schedule.revealEnd
        ) {
            revert InvalidSchedule();
        }

        $.auctions[worldwideDay] = IIntexAuction.AuctionData({
            worldwideDayState: IIntexAuction.WorldwideDayState.Unknown,
            schedule: schedule,
            params: params,
            result: IIntexAuction.AuctionResult({
                issuedIntexLoadedPromis: 0, auctionClearingRate: 0, issuedIntexCount: 0, wonBidsCount: 0
            })
        });

        emit AuctionStageUpdated(worldwideDay, IIntexAuction.AuctionStage.CommittingBids, uint32(block.timestamp), "");
    }

    /// @inheritdoc IIntexAuction
    function startRevealingBidsStage(uint32 worldwideDay, bool isGreenDay) external override onlyRole(RELAYER_ROLE) {
        IIntexAuction.AuctionData storage a = _s().auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(worldwideDay);
        if (currentStage != IIntexAuction.AuctionStage.CommittingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.CommittingBids, currentStage);
        }

        if (!isGreenDay) {
            // Red day - cancel auction.
            a.worldwideDayState = IIntexAuction.WorldwideDayState.Red;
            emit AuctionStageUpdated(
                worldwideDay,
                IIntexAuction.AuctionStage.Cancelled,
                uint32(block.timestamp),
                "Red day - auction cancelled"
            );
            return;
        }

        // Green day - proceed to the revealing stage.
        a.worldwideDayState = IIntexAuction.WorldwideDayState.Green;
        // Snap commitEnd forward only when the signal is early; revealEnd never moves.
        uint32 nowTs = uint32(block.timestamp);
        if (nowTs < a.schedule.commitEnd) {
            a.schedule.commitEnd = nowTs;
        }
        emit AuctionStageUpdated(worldwideDay, IIntexAuction.AuctionStage.RevealingBids, nowTs, "");
    }

    /// @inheritdoc IIntexAuction
    function startClearingStage(uint32 worldwideDay) external override onlyRole(RELAYER_ROLE) {
        IIntexAuction.AuctionData storage a = _s().auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(worldwideDay);
        bool alreadyIssuance = currentStage == IIntexAuction.AuctionStage.Issuance;
        if (!alreadyIssuance && currentStage != IIntexAuction.AuctionStage.RevealingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.RevealingBids, currentStage);
        }

        // Snap revealEnd forward only when the signal is early; issuanceEnd never moves.
        uint32 nowTs = uint32(block.timestamp);
        if (nowTs < a.schedule.revealEnd) {
            a.schedule.revealEnd = nowTs;
        }
        emit AuctionStageUpdated(worldwideDay, IIntexAuction.AuctionStage.Issuance, nowTs, "");
    }

    /// @inheritdoc IIntexAuction
    function executeAuctionClearing(
        uint32 worldwideDay,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) external override onlyRole(RELAYER_ROLE) nonReentrant {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(worldwideDay);
        if (currentStage != IIntexAuction.AuctionStage.Issuance) {
            revert StageRequired(IIntexAuction.AuctionStage.Issuance, currentStage);
        }

        // No-sale (issuedIntexCount == 0): supply was exhausted/zero, nothing is minted and every
        // bidder is fully refunded via REFUND_INSTRUCTIONS. The clearing rate is then unconstrained
        // — it may be 0 even when minIntexBidRate > 0 (no bid was allocated). A sale (issued > 0)
        // must carry a real clearing rate at or above the floor.
        if (issuedIntexCount > 0 && auctionClearingRate == 0) revert ZeroValue("auctionClearingRate");

        // Canonical clearing runs on Outbe; this only sanity-bounds the relayer-supplied result
        // against on-chain counters — winners cannot exceed revealed bids, and a sale's clearing
        // rate cannot fall below the configured minimum. It is not a full re-computation.
        uint32 revealed = $.auctionRunningCounts[worldwideDay].revealedBidsCount;
        if (wonBidsCount > revealed) revert WonBidsExceedRevealed(wonBidsCount, revealed);
        if (issuedIntexCount > 0 && auctionClearingRate < a.params.minIntexBidRate) {
            revert ClearingRateBelowMin(auctionClearingRate, a.params.minIntexBidRate);
        }

        // Final data provided by Outbe; `issuedIntexLoadedPromis` is derived on-chain.
        a.result.issuedIntexCount = issuedIntexCount;
        a.result.auctionClearingRate = auctionClearingRate;
        a.result.wonBidsCount = wonBidsCount;
        // 256-bit product: over-range reverts typed, not Panic(0x11).
        uint256 loadedPromis = uint256(issuedIntexCount) * a.params.promisLoadMinor;
        if (loadedPromis > type(uint128).max) revert IssuedPromisOverflow(issuedIntexCount, a.params.promisLoadMinor);
        a.result.issuedIntexLoadedPromis = uint128(loadedPromis);
        $.cleared[worldwideDay] = true;

        emit AuctionStageUpdated(worldwideDay, IIntexAuction.AuctionStage.Completed, uint32(block.timestamp), "");
        emit AuctionClearingExecuted(worldwideDay, auctionClearingRate, issuedIntexCount);
    }

    // --- User Actions ---
    /// @inheritdoc IIntexAuction
    function commitBid(uint32 worldwideDay, bytes32 commitHash) external override nonReentrant {
        if (commitHash == bytes32(0)) revert InvalidCommitHash();

        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(worldwideDay);
        if (currentStage != IIntexAuction.AuctionStage.CommittingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.CommittingBids, currentStage);
        }
        // `_getAuctionStage` reports CommittingBids past `commitEnd` while the green-day signal is
        // still Unknown; enforce the published deadline so late commits cannot slip in on relayer
        // latency. Window is `[start, commitEnd)`.
        if (uint32(block.timestamp) >= a.schedule.commitEnd) {
            revert CommitWindowClosed(a.schedule.commitEnd, uint32(block.timestamp));
        }
        if ($.committedBidsByHash[worldwideDay][msg.sender] != bytes32(0)) revert BidAlreadyCommitted();

        $.committedBidsByHash[worldwideDay][msg.sender] = commitHash;
        $.auctionRunningCounts[worldwideDay].committedBidsCount += 1;

        emit BidCommitted(worldwideDay, msg.sender, commitHash);

        // Interactions: take the entry bond (CEI — commit state is already recorded; a lock
        // revert rolls back the whole tx). Requires prior wCOEN approval on the escrow.
        uint128 bond = a.params.commitBondMinor;
        if (bond > 0) {
            $.escrowContract.lockCommitBond(worldwideDay, msg.sender, bond);
        }
    }

    /// @inheritdoc IIntexAuction
    function cancelCommit(uint32 worldwideDay) external override nonReentrant {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(worldwideDay);
        if (currentStage != IIntexAuction.AuctionStage.CommittingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.CommittingBids, currentStage);
        }
        // Mirror of `commitBid`: a sealed commit must not be withdrawable after `commitEnd`,
        // otherwise a bidder could cancel post-deadline once conditions are observed — defeating
        // the commit-reveal seal. Window is `[start, commitEnd)`.
        // Exception: a never-signalled auction pins CommittingBids forever, so stay cancellable once dead.
        bool stuck = a.worldwideDayState == IIntexAuction.WorldwideDayState.Unknown
            && uint32(block.timestamp) > a.schedule.issuanceEnd;
        if (!stuck && uint32(block.timestamp) >= a.schedule.commitEnd) {
            revert CommitWindowClosed(a.schedule.commitEnd, uint32(block.timestamp));
        }
        if ($.committedBidsByHash[worldwideDay][msg.sender] == bytes32(0)) revert BidNotFound();

        delete $.committedBidsByHash[worldwideDay][msg.sender];
        $.auctionRunningCounts[worldwideDay].committedBidsCount -= 1;

        emit CommitCancelled(worldwideDay, msg.sender);

        // Interactions: an un-committed bid owes no bond — return it immediately.
        if (a.params.commitBondMinor > 0) {
            $.escrowContract.releaseCommitBond(worldwideDay, msg.sender);
        }
    }

    /// @inheritdoc IIntexAuction
    function reapAuction(uint32 worldwideDay, uint256 limit) external override {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();

        IIntexAuction.AuctionStage stage = _getAuctionStage(worldwideDay);
        if (stage != IIntexAuction.AuctionStage.Completed && stage != IIntexAuction.AuctionStage.Cancelled) {
            revert StageRequired(IIntexAuction.AuctionStage.Completed, stage);
        }
        if (uint32(block.timestamp) <= a.schedule.issuanceEnd) revert TooEarlyToReap();

        IIntexAuction.SubmittedBidData[] storage bids = $.revealedBids[worldwideDay];
        uint256 remaining = bids.length;
        uint256 toPop = remaining < limit ? remaining : limit;
        for (uint256 i = 0; i < toPop; i++) {
            bids.pop();
        }
        emit AuctionReaped(worldwideDay, remaining - toPop);
    }

    /// @inheritdoc IIntexAuction
    function revealBid(uint32 worldwideDay, uint16 quantity, uint32 bidRate, uint64 chainId, bytes memory signature)
        external
        override
        nonReentrant
    {
        if (chainId != block.chainid) revert WrongChain(block.chainid, chainId);

        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(worldwideDay);
        if (currentStage != IIntexAuction.AuctionStage.RevealingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.RevealingBids, currentStage);
        }

        bytes32 committedHash = $.committedBidsByHash[worldwideDay][msg.sender];
        // Order matters: a re-reveal must report BidAlreadyRevealed, not BidNotFound, now that
        // the commit slot is freed on first reveal.
        if ($.revealedBidsByBidder[worldwideDay][msg.sender]) revert BidAlreadyRevealed();
        if (committedHash == bytes32(0)) revert BidNotFound();
        if (quantity == 0 || bidRate == 0) revert ZeroValue("quantity/bidRate");
        if (quantity < a.params.minIntexBidQuantity) revert BidBelowMinIntexBidQuantity();
        if (bidRate < a.params.minIntexBidRate) revert BidBelowMinIntexBidRate();
        if (bidRate > BridgeMsgCodec.RATE_SCALE) revert BidRateAboveMax(bidRate);

        // Escrow lock in wCOEN = qty * escrowBasis * rate / RATE_SCALE; escrowBasis = promis_load
        // per Intex. 256-bit math so an over-range product reverts typed, not via Panic(0x11).
        uint256 escrowBasis = a.params.promisLoadMinor;
        uint256 lockAmount = uint256(quantity) * escrowBasis * bidRate / BridgeMsgCodec.RATE_SCALE;
        if (lockAmount > type(uint128).max) revert BidAmountOverflow(quantity, bidRate);

        // Verify the signature against the stored commit hash.
        _verifyRevealSignature(worldwideDay, quantity, bidRate, signature, committedHash);

        // Effects: record the reveal before the external lockFunds call (CEI).
        // If lockFunds reverts the whole tx is rolled back, so atomicity is preserved.
        $.revealedBidsByBidder[worldwideDay][msg.sender] = true;
        // Free the consumed commit slot; commitBid already rejects any commit past commitEnd.
        delete $.committedBidsByHash[worldwideDay][msg.sender];

        $.revealedBids[worldwideDay].push(
            IIntexAuction.SubmittedBidData({
                bidderAddress: msg.sender,
                intexBidRate: bidRate,
                timestamp: uint32(block.timestamp),
                intexQuantity: quantity
            })
        );

        $.auctionRunningCounts[worldwideDay].revealedBidsCount += 1;

        emit BidRevealed(worldwideDay, msg.sender, quantity, bidRate);

        // Interactions
        // Return the commit bond first so it can fund the bid escrow in the same transaction.
        if (a.params.commitBondMinor > 0) {
            $.escrowContract.releaseCommitBond(worldwideDay, msg.sender);
        }
        // Lock amount must equal the clearing side's computation bit-for-bit, else finalize reverts.
        // forge-lint: disable-next-line(unsafe-typecast) -- bounded by the type(uint128).max check above
        $.escrowContract.lockFunds(worldwideDay, msg.sender, uint128(lockAmount));
    }

    /// @inheritdoc IIntexAuction
    function claimCommitBond(uint32 worldwideDay, address bidder) external override nonReentrant {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();

        // Red day cancels the auction before anyone can reveal — no fault, immediate return.
        // Every other outcome (revealed bidders have no bond; cancel is the in-window path)
        // is a no-reveal on a live auction: the bond waits out the penalty window anchored
        // at the (possibly snapped-forward) `revealEnd`.
        if (_getAuctionStage(worldwideDay) != IIntexAuction.AuctionStage.Cancelled) {
            uint32 claimableAt = a.schedule.revealEnd + COMMIT_BOND_LOCK_PERIOD;
            if (uint32(block.timestamp) < claimableAt) {
                revert CommitBondNotYetClaimable(claimableAt, uint32(block.timestamp));
            }
        }

        // Pays the stored bidder; reverts CommitBondNotFound in the escrow when no bond is live.
        $.escrowContract.releaseCommitBond(worldwideDay, bidder);
    }

    /// @notice Verify the EIP-712 reveal signature and its binding to the prior commit.
    /// @dev Reverts `RevealHashMismatch` when the recovered signer is not `msg.sender` or when
    ///      `keccak256(signature)` does not equal the stored commit hash.
    /// @param worldwideDay Worldwide day (yyyymmdd, uint32).
    /// @param quantity Requested Intex quantity.
    /// @param bidRate Bid rate (`1e6` fixed-point, % of the escrow basis).
    /// @param signature 65-byte ECDSA signature over the EIP-712 typed data.
    /// @param committedHash The `keccak256(signature)` previously stored by `commitBid`.
    function _verifyRevealSignature(
        uint32 worldwideDay,
        uint16 quantity,
        uint32 bidRate,
        bytes memory signature,
        bytes32 committedHash
    ) internal view {
        bytes32 structHash = keccak256(abi.encode(REVEAL_BID_TYPEHASH, worldwideDay, msg.sender, quantity, bidRate));
        bytes32 digest = _hashTypedDataV4(structHash);
        address signer = ECDSA.recover(digest, signature);
        if (signer != msg.sender || keccak256(signature) != committedHash) revert RevealHashMismatch();
    }

    // --- Views ---
    /// @inheritdoc IIntexAuction
    function getAuctionInfo(uint32 worldwideDay)
        external
        view
        override
        returns (IIntexAuction.AuctionData memory auctionData)
    {
        IIntexAuction.AuctionData memory a = _s().auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        return a;
    }

    /// @inheritdoc IIntexAuction
    function getAuctionDetails(uint32 worldwideDay)
        external
        view
        override
        returns (IIntexAuction.AuctionData memory auctionData, IIntexAuction.SubmittedBidData[] memory bidsData)
    {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData memory a = $.auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        return (a, $.revealedBids[worldwideDay]);
    }

    /// @inheritdoc IIntexAuction
    function getAuctionStage(uint32 worldwideDay) external view override returns (IIntexAuction.AuctionStage) {
        return _getAuctionStage(worldwideDay);
    }

    // --- Internal helpers ---
    /// @notice Compute the current auction stage from the schedule and worldwide-day state.
    /// @dev Reverts `AuctionNotFound` when the series has no entry. Red day short-circuits to
    ///      `Cancelled`; a cleared auction short-circuits to `Completed` (the `cleared` flag, set by
    ///      `executeAuctionClearing` — covers a no-sale clearing whose rate is 0); an `Unknown`
    ///      worldwide-day state stays in `CommittingBids` regardless of `commitEnd`.
    /// @param worldwideDay Worldwide day (yyyymmdd).
    /// @return Current auction stage.
    function _getAuctionStage(uint32 worldwideDay) internal view returns (IIntexAuction.AuctionStage) {
        IIntexAuction.AuctionData storage a = _s().auctions[worldwideDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();

        if (a.worldwideDayState == IIntexAuction.WorldwideDayState.Red) {
            return IIntexAuction.AuctionStage.Cancelled;
        }

        if (_s().cleared[worldwideDay]) {
            return IIntexAuction.AuctionStage.Completed;
        }

        // The reveal stage requires the bridge green-day signal.
        if (a.worldwideDayState == IIntexAuction.WorldwideDayState.Unknown) {
            return IIntexAuction.AuctionStage.CommittingBids;
        }

        uint32 nowTs = uint32(block.timestamp);
        if (nowTs < a.schedule.commitEnd) {
            return IIntexAuction.AuctionStage.CommittingBids;
        }
        if (nowTs < a.schedule.revealEnd) {
            return IIntexAuction.AuctionStage.RevealingBids;
        }
        return IIntexAuction.AuctionStage.Issuance;
    }

    /// @notice Check whether the contract supports a given interface.
    /// @dev Delegates to `AccessControl.supportsInterface` (ERC-165).
    /// @param id The interface ID to check.
    /// @return True if the interface is supported, false otherwise.
    function supportsInterface(bytes4 id) public view override(AccessControlUpgradeable) returns (bool) {
        return super.supportsInterface(id);
    }
}
