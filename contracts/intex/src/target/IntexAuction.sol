// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {EIP712Upgradeable} from "@openzeppelin/contracts-upgradeable/utils/cryptography/EIP712Upgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IEscrowAdapter, EscrowTimes} from "./interfaces/IEscrowAdapter.sol";
import {IIntexNFT1155} from "../shared/interfaces/IIntexNFT1155.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {Iso4217} from "../shared/libs/Iso4217.sol";

/// @title IntexAuction
/// @author Outbe
/// @notice Commit-reveal auction keyed by `emissionDay` (uint32, yyyymmdd).
/// @dev UUPS upgradeable behind an ERC1967 proxy. The router, the escrow and the collection are
///      implementation immutables, fixed at deploy.
///      The Outbe side computes the schedule and passes it into `startAuction`.
///      Reveal signatures are EIP-712 typed data under the `IntexAuction` v1 domain,
///      binding both `chainId` and `verifyingContract` (the proxy) and so preventing
///      cross-chain and cross-instance replay.
contract IntexAuction is
    OwnableUpgradeable,
    ReentrancyGuardTransient,
    EIP712Upgradeable,
    UUPSUpgradeable,
    IIntexAuction
{
    /// @dev Drives the stages of each day: IntexAuctionTargetRouter.
    address private immutable TARGET_ROUTER;
    /// @dev Holds the locks and bonds of the bidders: EscrowAdapter.
    IEscrowAdapter private immutable ESCROW;
    /// @dev The collection winners' units are issued on: IntexNFT1155.
    IIntexNFT1155 private immutable INTEX_NFT1155;

    /// @notice Lock on the commit bond of a bidder who never revealed, anchored at `revealEnd`.
    ///         The bond stays locked until `revealEnd + UNREVEALED_BOND_LOCK_PERIOD` and is then
    ///         reclaimable via `unlockIntexAuctionBidBond`. Reveal and cancel return it immediately.
    uint32 public constant UNREVEALED_BOND_LOCK_PERIOD = 24 hours;

    /// @dev `reason` of `IEscrowAdapter.IntexAuctionBidBondUnlocked`.
    uint8 private constant BOND_UNLOCK_REVEAL_OR_CANCEL = 0;
    uint8 private constant BOND_UNLOCK_NO_REVEAL = 1;

    /// @dev EIP-712 type hash for the revealed bid. The currency pair is part of the signed
    ///      struct, so a bidder cannot swap currencies between commit and reveal.
    bytes32 private constant REVEAL_BID_TYPEHASH = keccak256(
        "RevealBid(uint32 emissionDay,address bidder,uint16 quantity,uint32 bidRate,uint16 issuanceCurrency,uint16 referenceCurrency)"
    );

    /// @custom:storage-location erc7201:outbe.intex.IntexAuction
    struct IntexAuctionStorage {
        /// @dev Auction parameters and state, indexed by series id.
        mapping(uint32 emissionDay => IIntexAuction.AuctionData) auctions;
        /// @dev Live bid counters tracked while the auction runs.
        mapping(uint32 emissionDay => IIntexAuction.AuctionRunningCounts) auctionRunningCounts;
        /// @dev Committed bid hashes: emissionDay => bidder => commitHash.
        mapping(uint32 emissionDay => mapping(address bidder => bytes32 commitHash)) committedIntexAuctionBidsByHash;
        /// @dev Position of a bidder's revealed bid in `revealedBids`, plus one. Zero: none.
        mapping(uint32 emissionDay => mapping(address bidder => uint256 position)) revealedBidIndex;
        /// @dev Revealed bids per series.
        mapping(uint32 emissionDay => IIntexAuction.SubmittedIntexAuctionBidData[]) revealedBids;
        /// @dev Cleared marker per series. `completeAuction` sets it once. It is the sole
        ///      `Completed`-stage signal, so a no-sale clearing (issuedUnits == 0,
        ///      clearingRate may be 0) also reads as Completed, not just a positive-rate sale.
        mapping(uint32 emissionDay => bool) cleared;
        /// @dev Set by the first clearing start of the day.
        mapping(uint32 emissionDay => bool) clearingStarted;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.IntexAuction")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x0db73aff7344f42850665630fc90d6dc1080fdcdb5bb8f56a3fd235fc49b1c00;

    function _s() private pure returns (IntexAuctionStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address targetRouter_, address escrow_, address intexNFT1155_) {
        if (targetRouter_ == address(0)) revert ZeroAddress("targetRouter");
        if (escrow_ == address(0)) revert ZeroAddress("escrow");
        if (intexNFT1155_ == address(0)) revert ZeroAddress("intexNFT1155");
        TARGET_ROUTER = targetRouter_;
        ESCROW = IEscrowAdapter(escrow_);
        INTEX_NFT1155 = IIntexNFT1155(intexNFT1155_);
        _disableInitializers();
    }

    /// @notice Initializes the proxy with its owner and the EIP-712 domain.
    /// @param owner_ The owner, who upgrades the contract.
    function initialize(address owner_) external initializer {
        if (owner_ == address(0)) revert ZeroAddress("owner");

        __Ownable_init(owner_);
        __EIP712_init("IntexAuction", "1");
    }

    /// @dev The owner gates upgrades.
    /// @param newImplementation Address of the implementation the proxy switches to.
    function _authorizeUpgrade(address newImplementation) internal override onlyOwner {}

    modifier onlyTargetRouter() {
        if (msg.sender != TARGET_ROUTER) revert UnauthorizedCaller(msg.sender);
        _;
    }

    /// @inheritdoc IIntexAuction
    function targetRouter() external view returns (address) {
        return TARGET_ROUTER;
    }

    // --- Storage getters ---
    /// @notice Escrow contract for bid processing.
    /// @return The wired escrow adapter.
    function escrow() external view returns (IEscrowAdapter) {
        return ESCROW;
    }

    /// @notice Live bid counters tracked while the auction runs.
    function auctionRunningCounts(uint32 emissionDay)
        external
        view
        returns (uint32 committedBidsCount, uint32 revealedBidsCount)
    {
        IIntexAuction.AuctionRunningCounts storage c = _s().auctionRunningCounts[emissionDay];
        return (c.committedBidsCount, c.revealedBidsCount);
    }

    /// @notice Committed bid hash for a bidder.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @return The stored commit hash (zero when absent).
    function committedIntexAuctionBidsByHash(uint32 emissionDay, address bidder) external view returns (bytes32) {
        return _s().committedIntexAuctionBidsByHash[emissionDay][bidder];
    }

    /// @notice Whether a bidder has revealed for a series.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @return True when the bid was revealed.
    function revealedIntexAuctionBidsByBidder(uint32 emissionDay, address bidder) external view returns (bool) {
        return _s().revealedBidIndex[emissionDay][bidder] != 0;
    }

    /// @notice A bidder's revealed bid for a day, all zero when it has none.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @return bid The revealed bid.
    function getRevealedIntexAuctionBid(uint32 emissionDay, address bidder)
        external
        view
        returns (IIntexAuction.SubmittedIntexAuctionBidData memory bid)
    {
        IntexAuctionStorage storage $ = _s();
        uint256 position = $.revealedBidIndex[emissionDay][bidder];
        if (position != 0) bid = $.revealedBids[emissionDay][position - 1];
    }

    /// @notice Intex collection winners' units are issued on.
    /// @return The wired collection.
    function intexContract() external view returns (IIntexNFT1155) {
        return INTEX_NFT1155;
    }

    // --- Lifecycle ---
    /// @inheritdoc IIntexAuction
    function startAuction(
        uint32 emissionDay,
        IIntexAuction.AuctionSchedule calldata schedule,
        IIntexAuction.AuctionParams calldata params
    ) external override onlyTargetRouter {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[emissionDay];
        // `commitEnd == 0` is the canonical existence sentinel for an auction entry.
        if (a.schedule.commitEnd != 0) revert AuctionAlreadyExists();
        if (a.dayState == IIntexAuction.DayState.Canceled) revert AuctionCanceled();

        // Schedule timestamps must be strictly increasing, and the commit stage must end in the future.
        if (
            schedule.revealEnd <= schedule.commitEnd || schedule.issuanceEnd <= schedule.revealEnd
                || schedule.commitEnd <= block.timestamp
        ) {
            revert InvalidSchedule();
        }

        $.auctions[emissionDay] = IIntexAuction.AuctionData({
            dayState: IIntexAuction.DayState.Live,
            schedule: schedule,
            params: params,
            result: IIntexAuction.IntexAuctionResult({
                issuedPromisLoadMinor: 0, clearingRate: 0, issuedUnits: 0, wonBidsCount: 0
            })
        });

        emit IntexAuctionStarted(
            emissionDay,
            schedule.commitEnd,
            schedule.revealEnd,
            schedule.issuanceEnd,
            params.promisLoadMinor,
            params.minBidRate,
            params.minBidQuantity,
            params.bidBondMinor
        );
    }

    /// @inheritdoc IIntexAuction
    function cancelAuction(uint32 emissionDay, uint8 reason) external override onlyTargetRouter {
        IIntexAuction.AuctionData storage a = _s().auctions[emissionDay];
        if (a.dayState == IIntexAuction.DayState.Canceled) revert AuctionCanceled();
        if (a.schedule.commitEnd != 0) {
            IIntexAuction.AuctionStage currentStage = _getAuctionStage(emissionDay);
            if (
                currentStage == IIntexAuction.AuctionStage.Completed
                    || currentStage == IIntexAuction.AuctionStage.Failed
            ) {
                revert StageRequired(IIntexAuction.AuctionStage.AwaitingResult, currentStage);
            }
        }
        a.dayState = IIntexAuction.DayState.Canceled;
        emit IntexAuctionCanceled(emissionDay, reason);
    }

    /// @inheritdoc IIntexAuction
    function startAuctionClearing(uint32 emissionDay) external override onlyTargetRouter {
        IIntexAuction.AuctionData storage a = _s().auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(emissionDay);
        if (
            currentStage != IIntexAuction.AuctionStage.AwaitingResult
                && currentStage != IIntexAuction.AuctionStage.RevealingBids
        ) {
            revert StageRequired(IIntexAuction.AuctionStage.RevealingBids, currentStage);
        }

        // Snap revealEnd forward only when the signal is early. issuanceEnd never moves.
        uint32 nowTs = uint32(block.timestamp);
        if (nowTs < a.schedule.revealEnd) {
            a.schedule.revealEnd = nowTs;
        }
        if (!_s().clearingStarted[emissionDay]) {
            _s().clearingStarted[emissionDay] = true;
            emit IntexAuctionClearingStarted(emissionDay);
        }
    }

    /// @inheritdoc IIntexAuction
    function completeAuction(
        uint32 emissionDay,
        uint32 wonQuantity,
        uint32 wonBidsCount,
        uint32 quantity,
        uint64 clearingRate,
        uint64 totalDemand
    ) external override onlyTargetRouter nonReentrant {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(emissionDay);
        if (currentStage != IIntexAuction.AuctionStage.AwaitingResult) {
            revert StageRequired(IIntexAuction.AuctionStage.AwaitingResult, currentStage);
        }

        // No-sale (quantity == 0): supply was exhausted/zero, nothing is issued and every bidder
        // is fully refunded via REFUND_INSTRUCTIONS. The clearing rate is then unconstrained. It
        // may be 0 even when minBidRate > 0 (no bid was allocated). A sale (issued > 0) must
        // carry a real clearing rate at or above the floor.
        if (quantity > 0 && clearingRate == 0) revert ZeroValue("clearingRate");

        // Canonical clearing runs on Outbe. This only sanity-bounds the relayer-supplied result
        // against on-chain counters. Winners cannot exceed revealed bids, and a sale's clearing
        // rate cannot fall below the configured minimum. It is not a full re-computation.
        uint32 revealed = $.auctionRunningCounts[emissionDay].revealedBidsCount;
        if (wonBidsCount > revealed) revert WonIntexAuctionBidsExceedRevealed(wonBidsCount, revealed);
        if (quantity > 0 && clearingRate < a.params.minBidRate) {
            revert ClearingRateBelowMin(clearingRate, a.params.minBidRate);
        }

        // Outbe provides the final data. `issuedPromisLoadMinor` is derived on-chain.
        a.result.issuedUnits = quantity;
        a.result.clearingRate = clearingRate;
        a.result.wonBidsCount = wonBidsCount;
        // 256-bit product: over-range reverts typed, not Panic(0x11).
        uint256 loadedPromis = uint256(quantity) * a.params.promisLoadMinor;
        if (loadedPromis > type(uint128).max) revert IssuedPromisOverflow(quantity, a.params.promisLoadMinor);
        a.result.issuedPromisLoadMinor = uint128(loadedPromis);
        $.cleared[emissionDay] = true;

        emit IntexAuctionCompleted(emissionDay, wonQuantity, wonBidsCount, quantity, clearingRate, totalDemand);
    }

    // --- User Actions ---
    /// @inheritdoc IIntexAuction
    function commitIntexAuctionBid(uint32 emissionDay, bytes32 commitHash) external override nonReentrant {
        if (commitHash == bytes32(0)) revert InvalidCommitHash();

        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(emissionDay);
        if (currentStage != IIntexAuction.AuctionStage.CommittingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.CommittingBids, currentStage);
        }
        if ($.committedIntexAuctionBidsByHash[emissionDay][msg.sender] != bytes32(0)) {
            revert IntexAuctionBidAlreadyCommitted();
        }

        $.committedIntexAuctionBidsByHash[emissionDay][msg.sender] = commitHash;
        $.auctionRunningCounts[emissionDay].committedBidsCount += 1;

        emit IntexAuctionBidCommitted(emissionDay, msg.sender, commitHash);

        // Interactions: take the entry bond. CEI: the commit state is already recorded, and a
        // lock revert reverts the whole tx. Requires prior payment-token approval on the escrow.
        uint128 bond = a.params.bidBondMinor;
        if (bond > 0) {
            ESCROW.lockIntexAuctionBidBond(emissionDay, msg.sender, bond);
        }
    }

    /// @inheritdoc IIntexAuction
    function cancelIntexAuctionBid(uint32 emissionDay) external override nonReentrant {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(emissionDay);
        if (currentStage != IIntexAuction.AuctionStage.CommittingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.CommittingBids, currentStage);
        }
        if ($.committedIntexAuctionBidsByHash[emissionDay][msg.sender] == bytes32(0)) revert IntexAuctionBidNotFound();

        delete $.committedIntexAuctionBidsByHash[emissionDay][msg.sender];
        $.auctionRunningCounts[emissionDay].committedBidsCount -= 1;

        emit IntexAuctionBidCanceled(emissionDay, msg.sender);

        // Interactions: an un-committed bid owes no bond. Return it immediately.
        if (a.params.bidBondMinor > 0) {
            ESCROW.unlockIntexAuctionBidBond(emissionDay, msg.sender, BOND_UNLOCK_REVEAL_OR_CANCEL);
        }
    }

    /// @inheritdoc IIntexAuction
    function revealIntexAuctionBid(
        uint32 emissionDay,
        uint16 quantity,
        uint32 bidRate,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        uint64 chainId,
        bytes memory signature
    ) external override nonReentrant {
        if (chainId != block.chainid) revert WrongChain(block.chainid, chainId);

        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(emissionDay);
        if (currentStage != IIntexAuction.AuctionStage.RevealingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.RevealingBids, currentStage);
        }

        bytes32 committedHash = $.committedIntexAuctionBidsByHash[emissionDay][msg.sender];
        // Order matters: a re-reveal must report IntexAuctionBidAlreadyRevealed, not IntexAuctionBidNotFound, now that
        // the first reveal frees the commit slot.
        if ($.revealedBidIndex[emissionDay][msg.sender] != 0) revert IntexAuctionBidAlreadyRevealed();
        if (committedHash == bytes32(0)) revert IntexAuctionBidNotFound();
        if (quantity == 0 || bidRate == 0) revert ZeroValue("quantity/bidRate");
        if (quantity < a.params.minBidQuantity) revert IntexAuctionBidBelowMinBidQuantity();
        if (bidRate < a.params.minBidRate) revert IntexAuctionBidBelowMinBidRate();
        if (bidRate > BridgeMsgCodec.SCALE_1E6) revert IntexAuctionBidRateAboveMax(bidRate);
        if (!Iso4217.isIso4217(issuanceCurrency)) revert InvalidIssuanceCurrency(issuanceCurrency);
        _requirePriced(a.params.prices, emissionDay, referenceCurrency);

        // Verify the signature against the stored commit hash.
        _verifyRevealSignature(
            emissionDay, quantity, bidRate, issuanceCurrency, referenceCurrency, signature, committedHash
        );

        // Effects: record the reveal before the external lockIntexAuctionAsset call (CEI).
        // If lockIntexAuctionAsset reverts, the whole tx reverts, so atomicity is preserved.
        // Free the consumed commit slot. commitIntexAuctionBid already rejects any commit past commitEnd.
        delete $.committedIntexAuctionBidsByHash[emissionDay][msg.sender];

        $.revealedBids[emissionDay].push(
            IIntexAuction.SubmittedIntexAuctionBidData({
                bidder: msg.sender,
                intexBidRate: bidRate,
                timestamp: uint32(block.timestamp),
                intexQuantity: quantity,
                issuanceCurrency: issuanceCurrency,
                referenceCurrency: referenceCurrency
            })
        );

        $.revealedBidIndex[emissionDay][msg.sender] = $.revealedBids[emissionDay].length;
        $.auctionRunningCounts[emissionDay].revealedBidsCount += 1;

        emit IntexAuctionBidRevealed(emissionDay, msg.sender, quantity, bidRate, issuanceCurrency, referenceCurrency);

        // Interactions
        // Return the commit bond first so it can fund the bid escrow in the same transaction.
        if (a.params.bidBondMinor > 0) {
            ESCROW.unlockIntexAuctionBidBond(emissionDay, msg.sender, BOND_UNLOCK_REVEAL_OR_CANCEL);
        }
        ESCROW.lockIntexAuctionAsset(emissionDay, msg.sender, a.params.promisLoadMinor, bidRate, quantity);
    }

    /// @inheritdoc IIntexAuction
    function returnIntexAuctionAsset(uint32 emissionDay, address bidder) external override nonReentrant {
        IntexAuctionStorage storage $ = _s();
        uint256 position = $.revealedBidIndex[emissionDay][bidder];
        if (position == 0) revert IntexAuctionBidNotFound();
        (bool closed, bool finalized) = ESCROW.returnIntexAuctionAsset(emissionDay, bidder);
        if (closed && finalized) {
            delete $.revealedBids[emissionDay][position - 1];
            delete $.revealedBidIndex[emissionDay][bidder];
        }
    }

    /// @inheritdoc IIntexAuction
    function redeemWinNote(uint32 emissionDay, address bidder) external override nonReentrant {
        IntexAuctionStorage storage $ = _s();
        uint256 position = $.revealedBidIndex[emissionDay][bidder];
        if (position == 0) revert IntexAuctionBidNotFound();
        IIntexAuction.SubmittedIntexAuctionBidData storage bid = $.revealedBids[emissionDay][position - 1];
        uint16 issuanceCurrency = bid.issuanceCurrency;
        uint16 referenceCurrency = bid.referenceCurrency;
        IIntexNFT1155 intex = INTEX_NFT1155;
        bytes14 seriesId = intex.seriesIdOf(emissionDay, issuanceCurrency, referenceCurrency);
        if (seriesId == bytes14(0)) revert SeriesNotCreated(emissionDay, issuanceCurrency, referenceCurrency);

        // A WinNote exists only after the day cleared, so the bids relay no longer reads this bid.
        delete $.revealedBids[emissionDay][position - 1];
        delete $.revealedBidIndex[emissionDay][bidder];

        uint16 units = ESCROW.redeemWinNote(emissionDay, bidder);
        // A forfeited series took the units with it.
        if (intex.getIntexSeries(seriesId).state == IIntexNFT1155.IntexState.Forfeited) {
            emit IntexAuctionWinNoteExpired(emissionDay, bidder, seriesId);
            return;
        }
        emit IntexAuctionWinNoteRedeemed(emissionDay, bidder, seriesId);
        intex.issueIntex(bidder, units, seriesId);
    }

    /// @inheritdoc IIntexAuction
    function unlockIntexAuctionBidBond(uint32 emissionDay, address bidder) external override nonReentrant {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();

        // A bond that outlives its reveal window belongs to a no-reveal. It waits until the end of
        // the penalty window anchored at the (possibly snapped-forward) `revealEnd`. A canceled
        // day has no reveal to miss.
        if (a.dayState != IIntexAuction.DayState.Canceled) {
            uint32 unlockableAt = a.schedule.revealEnd + UNREVEALED_BOND_LOCK_PERIOD;
            if (uint32(block.timestamp) < unlockableAt) {
                revert IntexAuctionBidBondNotYetUnlockable(unlockableAt, uint32(block.timestamp));
            }
            // Pays the stored bidder. Reverts IntexAuctionBidBondNotFound in the escrow when no bond is live.
            ESCROW.unlockIntexAuctionBidBond(emissionDay, bidder, BOND_UNLOCK_NO_REVEAL);
            return;
        }
        ESCROW.unlockIntexAuctionBidBond(emissionDay, bidder, BOND_UNLOCK_REVEAL_OR_CANCEL);
    }

    /// @dev Reverts unless the day carries a price for `isoCode`.
    function _requirePriced(IIntexAuction.ReferenceCurrencyPrice[] storage rows, uint32 emissionDay, uint16 isoCode)
        private
        view
    {
        for (uint256 i = 0; i < rows.length; ++i) {
            if (rows[i].isoCode == isoCode) return;
        }
        revert ReferenceCurrencyNotPriced(emissionDay, isoCode);
    }

    /// @notice Verify the EIP-712 reveal signature and its binding to the prior commit.
    /// @dev Reverts `RevealHashMismatch` when the recovered signer is not `msg.sender` or when
    ///      `keccak256(signature)` does not equal the stored commit hash.
    /// @param emissionDay Emission day (yyyymmdd, uint32).
    /// @param quantity Requested Intex quantity.
    /// @param bidRate Bid rate (`1e6` fixed-point, % of the escrow basis).
    /// @param signature 65-byte ECDSA signature over the EIP-712 typed data.
    /// @param committedHash The `keccak256(signature)` previously stored by `commitIntexAuctionBid`.
    function _verifyRevealSignature(
        uint32 emissionDay,
        uint16 quantity,
        uint32 bidRate,
        uint16 issuanceCurrency,
        uint16 referenceCurrency,
        bytes memory signature,
        bytes32 committedHash
    ) internal view {
        bytes32 structHash = keccak256(
            abi.encode(
                REVEAL_BID_TYPEHASH, emissionDay, msg.sender, quantity, bidRate, issuanceCurrency, referenceCurrency
            )
        );
        bytes32 digest = _hashTypedDataV4(structHash);
        address signer = ECDSA.recover(digest, signature);
        if (signer != msg.sender || keccak256(signature) != committedHash) revert RevealHashMismatch();
    }

    // --- Views ---
    /// @inheritdoc IIntexAuction
    function getAuctionInfo(uint32 emissionDay)
        external
        view
        override
        returns (IIntexAuction.AuctionData memory auctionData)
    {
        IIntexAuction.AuctionData memory a = _s().auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        return a;
    }

    /// @inheritdoc IIntexAuction
    function revealedIntexAuctionBidsCount(uint32 emissionDay) external view override returns (uint256) {
        return _s().revealedBids[emissionDay].length;
    }

    /// @inheritdoc IIntexAuction
    function revealedIntexAuctionBidsSlice(uint32 emissionDay, uint256 offset, uint256 limit)
        external
        view
        override
        returns (IIntexAuction.SubmittedIntexAuctionBidData[] memory slice)
    {
        IIntexAuction.SubmittedIntexAuctionBidData[] storage bids = _s().revealedBids[emissionDay];
        uint256 length = bids.length;
        if (offset >= length) return new IIntexAuction.SubmittedIntexAuctionBidData[](0);
        uint256 end = offset + limit;
        if (end > length) end = length;
        slice = new IIntexAuction.SubmittedIntexAuctionBidData[](end - offset);
        for (uint256 i = 0; i < slice.length; i++) {
            slice[i] = bids[offset + i];
        }
    }

    /// @inheritdoc IIntexAuction
    function getAuctionDetails(uint32 emissionDay)
        external
        view
        override
        returns (
            IIntexAuction.AuctionData memory auctionData,
            IIntexAuction.SubmittedIntexAuctionBidData[] memory bidsData
        )
    {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData memory a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        return (a, $.revealedBids[emissionDay]);
    }

    /// @inheritdoc IIntexAuction
    function auctionStage(uint32 emissionDay) external view override returns (IIntexAuction.AuctionStage) {
        return _getAuctionStage(emissionDay);
    }

    // --- Internal helpers ---
    /// @notice Compute the current auction stage from the schedule and emission-day state.
    /// @dev Reverts `AuctionNotFound` when the series has no entry. Stage resolution:
    ///      - A canceled day short-circuits to `Canceled`.
    ///      - A cleared auction short-circuits to `Completed`. The `cleared` flag, set by
    ///        `completeAuction`, also covers a no-sale clearing whose rate is 0.
    ///      - A day with no result by the time its first bid can be refunded is `Failed`:
    ///        a result arriving later would meet bids already taken back.
    ///      - In all other cases the stage follows the stored schedule timestamps.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @return Current auction stage.
    function _getAuctionStage(uint32 emissionDay) internal view returns (IIntexAuction.AuctionStage) {
        IIntexAuction.AuctionData storage a = _s().auctions[emissionDay];
        if (a.dayState == IIntexAuction.DayState.Canceled) {
            return IIntexAuction.AuctionStage.Canceled;
        }
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();

        if (_s().cleared[emissionDay]) {
            return IIntexAuction.AuctionStage.Completed;
        }

        uint32 nowTs = uint32(block.timestamp);
        if (nowTs < a.schedule.commitEnd) {
            return IIntexAuction.AuctionStage.CommittingBids;
        }
        if (nowTs < a.schedule.revealEnd) {
            return IIntexAuction.AuctionStage.RevealingBids;
        }
        if (uint256(nowTs) >= uint256(a.schedule.commitEnd) + EscrowTimes.UNFINALIZED_RETURN_DELAY) {
            return IIntexAuction.AuctionStage.Failed;
        }
        return IIntexAuction.AuctionStage.AwaitingResult;
    }
}
