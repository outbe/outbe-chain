// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControl} from "@openzeppelin/contracts/access/AccessControl.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import {EIP712} from "@openzeppelin/contracts/utils/cryptography/EIP712.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";

/// @title IntexAuction
/// @author Outbe
/// @notice Commit-reveal auction keyed by `seriesId` (uint32, yyyymmdd).
/// @dev The schedule is computed on the Outbe side and passed into `auctionStart`.
///      Reveal signatures are EIP-712 typed data under the `IntexAuction` v1 domain,
///      binding both `chainId` and `verifyingContract` and so preventing cross-chain
///      and cross-instance replay.
contract IntexAuction is AccessControl, ReentrancyGuard, EIP712, IIntexAuction {
    // Roles
    /// @notice Role identifier for bridge operations (stage ops driven by the relayer).
    bytes32 public constant RELAYER_ROLE = keccak256("RELAYER_ROLE");

    /// @dev EIP-712 type hash for `RevealBid(uint32 seriesId,address bidder,uint16 quantity,uint64 bidPrice)`.
    bytes32 private constant REVEAL_BID_TYPEHASH =
        keccak256("RevealBid(uint32 seriesId,address bidder,uint16 quantity,uint64 bidPrice)");

    // External contract dependencies
    /// @notice Escrow contract for bid processing.
    IEscrowAdapter public escrowContract;

    // Storage mappings (all keyed by seriesId)
    /// @notice Auction parameters and state, indexed by series id.
    mapping(uint32 => IIntexAuction.AuctionData) public auctions;
    /// @notice Live bid counters tracked while the auction runs.
    mapping(uint32 => IIntexAuction.AuctionRunningCounts) public auctionRunningCounts;
    /// @notice Committed bid hashes: seriesId => bidder => commitHash.
    mapping(uint32 => mapping(address => bytes32)) public committedBidsByHash;
    /// @notice Bid revealed status: seriesId => bidder => revealed.
    mapping(uint32 => mapping(address => bool)) public revealedBidsByBidder;
    /// @notice Revealed bids per series.
    mapping(uint32 => IIntexAuction.SubmittedBidData[]) public revealedBids;

    constructor(address defaultAdmin, address bridger) EIP712("IntexAuction", "1") {
        _grantRole(DEFAULT_ADMIN_ROLE, defaultAdmin);
        _grantRole(RELAYER_ROLE, bridger);
    }

    // --- Admin ---
    /// @inheritdoc IIntexAuction
    function wire(address _escrow) external override onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_escrow == address(0)) revert ZeroAddress("escrowContract");

        address previousEscrow = address(escrowContract);
        escrowContract = IEscrowAdapter(_escrow);

        emit EscrowWired(previousEscrow, _escrow);
    }

    // --- Lifecycle ---
    /// @inheritdoc IIntexAuction
    function auctionStart(
        uint32 seriesId,
        IIntexAuction.AuctionSchedule calldata schedule,
        IIntexAuction.AuctionParams calldata params
    ) external override onlyRole(RELAYER_ROLE) {
        // `commitEnd == 0` is the canonical existence sentinel for an auction entry.
        if (auctions[seriesId].schedule.commitEnd != 0) revert AuctionAlreadyExists();

        // Schedule timestamps must be strictly increasing and the commit stage must end in the future.
        if (
            schedule.commitEnd <= block.timestamp || schedule.revealEnd <= schedule.commitEnd
                || schedule.issuanceEnd <= schedule.revealEnd
        ) {
            revert InvalidSchedule();
        }

        auctions[seriesId] = IIntexAuction.AuctionData({
            worldwideDayState: IIntexAuction.WorldwideDayState.Unknown,
            schedule: schedule,
            params: params,
            result: IIntexAuction.AuctionResult({
                issuedIntexLoadedPromis: 0, auctionIntexClearingPrice: 0, issuedIntexCount: 0, wonBidsCount: 0
            })
        });

        emit AuctionStageUpdated(seriesId, IIntexAuction.AuctionStage.CommittingBids, uint32(block.timestamp), "");
    }

    /// @inheritdoc IIntexAuction
    function startRevealingBidsStage(uint32 seriesId, bool isGreenDay) external override onlyRole(RELAYER_ROLE) {
        IIntexAuction.AuctionData storage a = auctions[seriesId];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(seriesId);
        if (currentStage != IIntexAuction.AuctionStage.CommittingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.CommittingBids, currentStage);
        }

        if (!isGreenDay) {
            // Red day - cancel auction.
            a.worldwideDayState = IIntexAuction.WorldwideDayState.Red;
            emit AuctionStageUpdated(
                seriesId, IIntexAuction.AuctionStage.Cancelled, uint32(block.timestamp), "Red day - auction cancelled"
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
        emit AuctionStageUpdated(seriesId, IIntexAuction.AuctionStage.RevealingBids, nowTs, "");
    }

    /// @inheritdoc IIntexAuction
    function startClearingStage(uint32 seriesId) external override onlyRole(RELAYER_ROLE) {
        IIntexAuction.AuctionData storage a = auctions[seriesId];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(seriesId);
        bool alreadyIssuance = currentStage == IIntexAuction.AuctionStage.Issuance;
        if (!alreadyIssuance && currentStage != IIntexAuction.AuctionStage.RevealingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.RevealingBids, currentStage);
        }

        // Snap revealEnd forward only when the signal is early; issuanceEnd never moves.
        uint32 nowTs = uint32(block.timestamp);
        if (nowTs < a.schedule.revealEnd) {
            a.schedule.revealEnd = nowTs;
        }
        emit AuctionStageUpdated(seriesId, IIntexAuction.AuctionStage.Issuance, nowTs, "");
    }

    /// @inheritdoc IIntexAuction
    function executeAuctionClearing(
        uint32 seriesId,
        uint32 issuedIntexCount,
        uint64 auctionIntexClearingPrice,
        uint32 wonBidsCount
    ) external override onlyRole(RELAYER_ROLE) nonReentrant {
        IIntexAuction.AuctionData storage a = auctions[seriesId];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(seriesId);
        if (currentStage != IIntexAuction.AuctionStage.Issuance) {
            revert StageRequired(IIntexAuction.AuctionStage.Issuance, currentStage);
        }

        if (auctionIntexClearingPrice == 0) revert ZeroValue("auctionIntexClearingPrice");

        // Canonical clearing runs on Outbe; this only sanity-bounds the relayer-supplied result
        // against on-chain counters — winners cannot exceed revealed bids, and the clearing price
        // cannot fall below the configured minimum. It is not a full re-computation.
        uint32 revealed = auctionRunningCounts[seriesId].revealedBidsCount;
        if (wonBidsCount > revealed) revert WonBidsExceedRevealed(wonBidsCount, revealed);
        if (auctionIntexClearingPrice < a.params.minIntexBidPrice) {
            revert ClearingPriceBelowMin(auctionIntexClearingPrice, a.params.minIntexBidPrice);
        }

        // Final data provided by Outbe; `issuedIntexLoadedPromis` is derived on-chain.
        a.result.issuedIntexCount = issuedIntexCount;
        a.result.auctionIntexClearingPrice = auctionIntexClearingPrice;
        a.result.wonBidsCount = wonBidsCount;
        a.result.issuedIntexLoadedPromis = uint128(issuedIntexCount) * a.params.intexSize;

        emit AuctionStageUpdated(seriesId, IIntexAuction.AuctionStage.Completed, uint32(block.timestamp), "");
        emit AuctionClearingExecuted(seriesId, auctionIntexClearingPrice, issuedIntexCount);
    }

    // --- User Actions ---
    /// @inheritdoc IIntexAuction
    function commitBid(uint32 seriesId, bytes32 commitHash) external override {
        if (commitHash == bytes32(0)) revert InvalidCommitHash();

        IIntexAuction.AuctionData storage a = auctions[seriesId];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(seriesId);
        if (currentStage != IIntexAuction.AuctionStage.CommittingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.CommittingBids, currentStage);
        }
        // `_getAuctionStage` reports CommittingBids past `commitEnd` while the green-day signal is
        // still Unknown; enforce the published deadline so late commits cannot slip in on relayer
        // latency. Window is `[start, commitEnd)`.
        if (uint32(block.timestamp) >= a.schedule.commitEnd) {
            revert CommitWindowClosed(a.schedule.commitEnd, uint32(block.timestamp));
        }
        if (committedBidsByHash[seriesId][msg.sender] != bytes32(0)) revert BidAlreadyCommitted();

        committedBidsByHash[seriesId][msg.sender] = commitHash;
        auctionRunningCounts[seriesId].committedBidsCount += 1;

        emit BidCommitted(seriesId, msg.sender, commitHash);
    }

    /// @inheritdoc IIntexAuction
    function cancelCommit(uint32 seriesId) external override {
        IIntexAuction.AuctionData storage a = auctions[seriesId];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(seriesId);
        if (currentStage != IIntexAuction.AuctionStage.CommittingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.CommittingBids, currentStage);
        }
        // Mirror of `commitBid`: a sealed commit must not be withdrawable after `commitEnd`,
        // otherwise a bidder could cancel post-deadline once conditions are observed — defeating
        // the commit-reveal seal. Window is `[start, commitEnd)`.
        if (uint32(block.timestamp) >= a.schedule.commitEnd) {
            revert CommitWindowClosed(a.schedule.commitEnd, uint32(block.timestamp));
        }
        if (committedBidsByHash[seriesId][msg.sender] == bytes32(0)) revert BidNotFound();

        delete committedBidsByHash[seriesId][msg.sender];
        auctionRunningCounts[seriesId].committedBidsCount -= 1;

        emit CommitCancelled(seriesId, msg.sender);
    }

    /// @inheritdoc IIntexAuction
    function revealBid(
        uint32 seriesId,
        uint16 quantity,
        uint64 bidPrice,
        uint64 chainId,
        bytes memory signature
    ) external override nonReentrant {
        if (chainId != block.chainid) revert WrongChain(block.chainid, chainId);

        IIntexAuction.AuctionData storage a = auctions[seriesId];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(seriesId);
        if (currentStage != IIntexAuction.AuctionStage.RevealingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.RevealingBids, currentStage);
        }

        bytes32 committedHash = committedBidsByHash[seriesId][msg.sender];
        if (committedHash == bytes32(0)) revert BidNotFound();
        if (revealedBidsByBidder[seriesId][msg.sender]) revert BidAlreadyRevealed();
        if (quantity == 0 || bidPrice == 0) revert ZeroValue("quantity/bidPrice");
        if (quantity < a.params.minIntexBidQuantity) revert BidBelowMinIntexBidQuantity();
        if (bidPrice < a.params.minIntexBidPrice) revert BidBelowMinIntexBidPrice();

        // Compute the lock amount in 256-bit space so an over-range product surfaces as a typed
        // error rather than a bare arithmetic Panic(0x11); lockFunds takes a uint64 amount.
        uint256 lockAmount = uint256(quantity) * bidPrice;
        if (lockAmount > type(uint64).max) revert BidAmountOverflow(quantity, bidPrice);

        // Verify the signature against the stored commit hash.
        _verifyRevealSignature(seriesId, quantity, bidPrice, signature, committedHash);

        // Effects: record the reveal before the external lockFunds call (CEI).
        // If lockFunds reverts the whole tx is rolled back, so atomicity is preserved.
        revealedBidsByBidder[seriesId][msg.sender] = true;

        revealedBids[seriesId].push(
            IIntexAuction.SubmittedBidData({
                bidderAddress: msg.sender,
                intexBidPrice: bidPrice,
                timestamp: uint32(block.timestamp),
                intexQuantity: quantity
            })
        );

        auctionRunningCounts[seriesId].revealedBidsCount += 1;

        emit BidRevealed(seriesId, msg.sender, quantity, bidPrice);

        // Interactions
        // Lock amount must equal the clearing side's computation bit-for-bit, else finalize reverts.
        escrowContract.lockFunds(seriesId, msg.sender, uint64(lockAmount));
    }

    /// @notice Verify the EIP-712 reveal signature and its binding to the prior commit.
    /// @dev Reverts `RevealHashMismatch` when the recovered signer is not `msg.sender` or when
    ///      `keccak256(signature)` does not equal the stored commit hash.
    /// @param seriesId Auction series id (yyyymmdd as uint32).
    /// @param quantity Requested Intex quantity.
    /// @param bidPrice Bid price per unit (payment-token decimals).
    /// @param signature 65-byte ECDSA signature over the EIP-712 typed data.
    /// @param committedHash The `keccak256(signature)` previously stored by `commitBid`.
    function _verifyRevealSignature(
        uint32 seriesId,
        uint16 quantity,
        uint64 bidPrice,
        bytes memory signature,
        bytes32 committedHash
    ) internal view {
        bytes32 structHash = keccak256(abi.encode(REVEAL_BID_TYPEHASH, seriesId, msg.sender, quantity, bidPrice));
        bytes32 digest = _hashTypedDataV4(structHash);
        address signer = ECDSA.recover(digest, signature);
        if (signer != msg.sender || keccak256(signature) != committedHash) revert RevealHashMismatch();
    }

    // --- Views ---
    /// @inheritdoc IIntexAuction
    function getAuctionInfo(uint32 seriesId)
        external
        view
        override
        returns (IIntexAuction.AuctionData memory auctionData)
    {
        IIntexAuction.AuctionData memory a = auctions[seriesId];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        return a;
    }

    /// @inheritdoc IIntexAuction
    function getAuctionDetails(uint32 seriesId)
        external
        view
        override
        returns (IIntexAuction.AuctionData memory auctionData, IIntexAuction.SubmittedBidData[] memory bidsData)
    {
        IIntexAuction.AuctionData memory a = auctions[seriesId];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        return (a, revealedBids[seriesId]);
    }

    /// @inheritdoc IIntexAuction
    function getAuctionStage(uint32 seriesId) external view override returns (IIntexAuction.AuctionStage) {
        return _getAuctionStage(seriesId);
    }

    // --- Internal helpers ---
    /// @notice Compute the current auction stage from the schedule and worldwide-day state.
    /// @dev Reverts `AuctionNotFound` when the series has no entry. Red day short-circuits to
    ///      `Cancelled`; a non-zero clearing price short-circuits to `Completed`; an `Unknown`
    ///      worldwide-day state stays in `CommittingBids` regardless of `commitEnd`.
    /// @param seriesId Auction series id.
    /// @return Current auction stage.
    function _getAuctionStage(uint32 seriesId) internal view returns (IIntexAuction.AuctionStage) {
        IIntexAuction.AuctionData storage a = auctions[seriesId];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();

        if (a.worldwideDayState == IIntexAuction.WorldwideDayState.Red) {
            return IIntexAuction.AuctionStage.Cancelled;
        }

        if (a.result.auctionIntexClearingPrice > 0) {
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
    function supportsInterface(bytes4 id) public view override(AccessControl) returns (bool) {
        return super.supportsInterface(id);
    }
}
