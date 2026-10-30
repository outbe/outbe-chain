// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {EIP712Upgradeable} from "@openzeppelin/contracts-upgradeable/utils/cryptography/EIP712Upgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {ECDSA} from "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";
import {IIntexNFT1155} from "../shared/interfaces/IIntexNFT1155.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {Iso4217} from "../shared/libs/Iso4217.sol";

/// @title IntexAuction
/// @author Outbe
/// @notice Commit-reveal auction keyed by `emissionDay` (uint32, yyyymmdd).
/// @dev UUPS upgradeable: deployed behind an ERC1967 proxy, configured via `initialize`.
///      The Outbe side computes the schedule and passes it into `auctionStart`.
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

    /// @notice Lock on the commit bond of a bidder who never revealed, anchored at `revealEnd`.
    ///         The bond stays locked until `revealEnd + UNREVEALED_BOND_LOCK_PERIOD` and is then
    ///         reclaimable via `claimCommitBond`. Reveal and cancel return it immediately.
    uint32 public constant UNREVEALED_BOND_LOCK_PERIOD = 24 hours;

    /// @dev EIP-712 type hash for the revealed bid. The currency pair is part of the signed
    ///      struct, so a bidder cannot swap currencies between commit and reveal.
    bytes32 private constant REVEAL_BID_TYPEHASH = keccak256(
        "RevealBid(uint32 emissionDay,address bidder,uint16 quantity,uint32 bidRate,uint16 issuanceCurrency,uint16 referenceCurrency)"
    );

    /// @custom:storage-location erc7201:outbe.intex.IntexAuction
    struct IntexAuctionStorage {
        /// @dev Escrow contract for bid processing.
        IEscrowAdapter escrowContract;
        /// @dev Auction parameters and state, indexed by series id.
        mapping(uint32 emissionDay => IIntexAuction.AuctionData) auctions;
        /// @dev Live bid counters tracked while the auction runs.
        mapping(uint32 emissionDay => IIntexAuction.AuctionRunningCounts) auctionRunningCounts;
        /// @dev Committed bid hashes: emissionDay => bidder => commitHash.
        mapping(uint32 emissionDay => mapping(address bidder => bytes32 commitHash)) committedBidsByHash;
        /// @dev Position of a bidder's revealed bid in `revealedBids`, plus one. Zero: none.
        mapping(uint32 emissionDay => mapping(address bidder => uint256 position)) revealedBidIndex;
        /// @dev Revealed bids per series.
        mapping(uint32 emissionDay => IIntexAuction.SubmittedBidData[]) revealedBids;
        /// @dev Cleared marker per series. `executeAuctionClearing` sets it once. It is the sole
        ///      `Completed`-stage signal, so a no-sale clearing (issuedUnits == 0,
        ///      clearingRate may be 0) also reads as Completed, not just a positive-rate sale.
        mapping(uint32 emissionDay => bool) cleared;
        /// @dev Intex collection winners' units are issued on.
        IIntexNFT1155 intexContract;
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
    function committedBidsByHash(uint32 emissionDay, address bidder) external view returns (bytes32) {
        return _s().committedBidsByHash[emissionDay][bidder];
    }

    /// @notice Whether a bidder has revealed for a series.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @return True when the bid was revealed.
    function revealedBidsByBidder(uint32 emissionDay, address bidder) external view returns (bool) {
        return _s().revealedBidIndex[emissionDay][bidder] != 0;
    }

    /// @notice A bidder's revealed bid for a day, all zero when it has none.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @param bidder Bidder address.
    /// @return bid The revealed bid.
    function revealedBidOf(uint32 emissionDay, address bidder)
        external
        view
        returns (IIntexAuction.SubmittedBidData memory bid)
    {
        IntexAuctionStorage storage $ = _s();
        uint256 position = $.revealedBidIndex[emissionDay][bidder];
        if (position != 0) bid = $.revealedBids[emissionDay][position - 1];
    }

    /// @notice Intex collection winners' units are issued on.
    /// @return The wired collection.
    function intexContract() external view returns (IIntexNFT1155) {
        return _s().intexContract;
    }

    /// @inheritdoc IIntexAuction
    function wire(address _escrow) external override onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_escrow == address(0)) revert ZeroAddress("escrowContract");

        IntexAuctionStorage storage $ = _s();
        IEscrowAdapter current = $.escrowContract;
        // A day's claims go to the escrow that took its locks, so that escrow stays until nothing is left.
        if (address(current) != address(0) && _escrow != address(current) && current.hasOutstandingLocks()) {
            revert EscrowHasLiveLocks();
        }
        $.escrowContract = IEscrowAdapter(_escrow);
        emit EscrowWired(address(current), _escrow);
    }

    /// @inheritdoc IIntexAuction
    function setIntexContract(address _intex) external override onlyRole(DEFAULT_ADMIN_ROLE) {
        if (_intex == address(0)) revert ZeroAddress("intexContract");

        IntexAuctionStorage storage $ = _s();
        IIntexNFT1155 current = $.intexContract;
        // Open WinNotes issue on the collection their series live on.
        IEscrowAdapter escrow = $.escrowContract;
        bool changing = address(current) != address(0) && _intex != address(current);
        if (changing && address(escrow) != address(0) && escrow.hasOutstandingLocks()) {
            revert EscrowHasLiveLocks();
        }
        $.intexContract = IIntexNFT1155(_intex);
        emit IntexWired(address(current), _intex);
    }

    // --- Lifecycle ---
    /// @inheritdoc IIntexAuction
    function auctionStart(
        uint32 emissionDay,
        IIntexAuction.DayState dayState,
        IIntexAuction.AuctionSchedule calldata schedule,
        IIntexAuction.AuctionParams calldata params
    ) external override onlyRole(RELAYER_ROLE) {
        IntexAuctionStorage storage $ = _s();
        // `commitEnd == 0` is the canonical existence sentinel for an auction entry.
        if ($.auctions[emissionDay].schedule.commitEnd != 0) revert AuctionAlreadyExists();
        if (dayState == IIntexAuction.DayState.Unknown) revert InvalidDayState();

        // Schedule timestamps must be strictly increasing. A live auction's
        // commit stage must end in the future. A cancelled day is a cancelled record only.
        if (
            schedule.revealEnd <= schedule.commitEnd || schedule.issuanceEnd <= schedule.revealEnd
                || (dayState == IIntexAuction.DayState.Live && schedule.commitEnd <= block.timestamp)
        ) {
            revert InvalidSchedule();
        }

        $.auctions[emissionDay] = IIntexAuction.AuctionData({
            dayState: dayState,
            schedule: schedule,
            params: params,
            result: IIntexAuction.AuctionResult({
                issuedPromisLoadMinor: 0, auctionClearingRate: 0, issuedUnits: 0, wonBidsCount: 0
            })
        });

        if (dayState == IIntexAuction.DayState.Cancelled) {
            emit AuctionStageUpdated(
                emissionDay, IIntexAuction.AuctionStage.Cancelled, uint32(block.timestamp), "Day cancelled - no auction"
            );
        } else {
            emit AuctionStageUpdated(
                emissionDay, IIntexAuction.AuctionStage.CommittingBids, uint32(block.timestamp), ""
            );
        }
    }

    /// @inheritdoc IIntexAuction
    function startClearingStage(uint32 emissionDay) external override onlyRole(RELAYER_ROLE) {
        IIntexAuction.AuctionData storage a = _s().auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(emissionDay);
        bool alreadyIssuance = currentStage == IIntexAuction.AuctionStage.Issuance;
        if (!alreadyIssuance && currentStage != IIntexAuction.AuctionStage.RevealingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.RevealingBids, currentStage);
        }

        // Snap revealEnd forward only when the signal is early. issuanceEnd never moves.
        uint32 nowTs = uint32(block.timestamp);
        if (nowTs < a.schedule.revealEnd) {
            a.schedule.revealEnd = nowTs;
        }
        emit AuctionStageUpdated(emissionDay, IIntexAuction.AuctionStage.Issuance, nowTs, "");
    }

    /// @inheritdoc IIntexAuction
    function executeAuctionClearing(
        uint32 emissionDay,
        uint32 issuedUnits,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) external override onlyRole(RELAYER_ROLE) nonReentrant {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(emissionDay);
        if (currentStage != IIntexAuction.AuctionStage.Issuance) {
            revert StageRequired(IIntexAuction.AuctionStage.Issuance, currentStage);
        }

        // No-sale (issuedUnits == 0): supply was exhausted/zero, nothing is issued and every bidder
        // is fully refunded via REFUND_INSTRUCTIONS. The clearing rate is then unconstrained. It
        // may be 0 even when minIntexBidRate > 0 (no bid was allocated). A sale (issued > 0) must
        // carry a real clearing rate at or above the floor.
        if (issuedUnits > 0 && auctionClearingRate == 0) revert ZeroValue("auctionClearingRate");

        // Canonical clearing runs on Outbe. This only sanity-bounds the relayer-supplied result
        // against on-chain counters. Winners cannot exceed revealed bids, and a sale's clearing
        // rate cannot fall below the configured minimum. It is not a full re-computation.
        uint32 revealed = $.auctionRunningCounts[emissionDay].revealedBidsCount;
        if (wonBidsCount > revealed) revert WonBidsExceedRevealed(wonBidsCount, revealed);
        if (issuedUnits > 0 && auctionClearingRate < a.params.minIntexBidRate) {
            revert ClearingRateBelowMin(auctionClearingRate, a.params.minIntexBidRate);
        }

        // Outbe provides the final data. `issuedPromisLoadMinor` is derived on-chain.
        a.result.issuedUnits = issuedUnits;
        a.result.auctionClearingRate = auctionClearingRate;
        a.result.wonBidsCount = wonBidsCount;
        // 256-bit product: over-range reverts typed, not Panic(0x11).
        uint256 loadedPromis = uint256(issuedUnits) * a.params.promisLoadMinor;
        if (loadedPromis > type(uint128).max) revert IssuedPromisOverflow(issuedUnits, a.params.promisLoadMinor);
        a.result.issuedPromisLoadMinor = uint128(loadedPromis);
        $.cleared[emissionDay] = true;

        emit AuctionStageUpdated(emissionDay, IIntexAuction.AuctionStage.Completed, uint32(block.timestamp), "");
        emit AuctionClearingExecuted(emissionDay, auctionClearingRate, issuedUnits);
    }

    // --- User Actions ---
    /// @inheritdoc IIntexAuction
    function commitBid(uint32 emissionDay, bytes32 commitHash) external override nonReentrant {
        if (commitHash == bytes32(0)) revert InvalidCommitHash();

        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(emissionDay);
        if (currentStage != IIntexAuction.AuctionStage.CommittingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.CommittingBids, currentStage);
        }
        if ($.committedBidsByHash[emissionDay][msg.sender] != bytes32(0)) revert BidAlreadyCommitted();

        $.committedBidsByHash[emissionDay][msg.sender] = commitHash;
        $.auctionRunningCounts[emissionDay].committedBidsCount += 1;

        emit BidCommitted(emissionDay, msg.sender, commitHash);

        // Interactions: take the entry bond. CEI: the commit state is already recorded, and a
        // lock revert reverts the whole tx. Requires prior payment-token approval on the escrow.
        uint128 bond = a.params.commitBondMinor;
        if (bond > 0) {
            $.escrowContract.lockCommitBond(emissionDay, msg.sender, bond);
        }
    }

    /// @inheritdoc IIntexAuction
    function cancelCommit(uint32 emissionDay) external override nonReentrant {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        IIntexAuction.AuctionStage currentStage = _getAuctionStage(emissionDay);
        if (currentStage != IIntexAuction.AuctionStage.CommittingBids) {
            revert StageRequired(IIntexAuction.AuctionStage.CommittingBids, currentStage);
        }
        if ($.committedBidsByHash[emissionDay][msg.sender] == bytes32(0)) revert BidNotFound();

        delete $.committedBidsByHash[emissionDay][msg.sender];
        $.auctionRunningCounts[emissionDay].committedBidsCount -= 1;

        emit CommitCancelled(emissionDay, msg.sender);

        // Interactions: an un-committed bid owes no bond. Return it immediately.
        if (a.params.commitBondMinor > 0) {
            $.escrowContract.releaseCommitBond(emissionDay, msg.sender);
        }
    }

    /// @inheritdoc IIntexAuction
    function revealBid(
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

        bytes32 committedHash = $.committedBidsByHash[emissionDay][msg.sender];
        // Order matters: a re-reveal must report BidAlreadyRevealed, not BidNotFound, now that
        // the first reveal frees the commit slot.
        if ($.revealedBidIndex[emissionDay][msg.sender] != 0) revert BidAlreadyRevealed();
        if (committedHash == bytes32(0)) revert BidNotFound();
        if (quantity == 0 || bidRate == 0) revert ZeroValue("quantity/bidRate");
        if (quantity < a.params.minIntexBidQuantity) revert BidBelowMinIntexBidQuantity();
        if (bidRate < a.params.minIntexBidRate) revert BidBelowMinIntexBidRate();
        if (bidRate > BridgeMsgCodec.SCALE_1E6) revert BidRateAboveMax(bidRate);
        if (!Iso4217.isIso4217(issuanceCurrency)) revert InvalidIssuanceCurrency(issuanceCurrency);
        _requirePriced(a.params.prices, emissionDay, referenceCurrency);

        // Verify the signature against the stored commit hash.
        _verifyRevealSignature(
            emissionDay, quantity, bidRate, issuanceCurrency, referenceCurrency, signature, committedHash
        );

        // Effects: record the reveal before the external lockFunds call (CEI).
        // If lockFunds reverts, the whole tx reverts, so atomicity is preserved.
        // Free the consumed commit slot. commitBid already rejects any commit past commitEnd.
        delete $.committedBidsByHash[emissionDay][msg.sender];

        $.revealedBids[emissionDay].push(
            IIntexAuction.SubmittedBidData({
                bidderAddress: msg.sender,
                intexBidRate: bidRate,
                timestamp: uint32(block.timestamp),
                intexQuantity: quantity,
                issuanceCurrency: issuanceCurrency,
                referenceCurrency: referenceCurrency
            })
        );

        $.revealedBidIndex[emissionDay][msg.sender] = $.revealedBids[emissionDay].length;
        $.auctionRunningCounts[emissionDay].revealedBidsCount += 1;

        emit BidRevealed(emissionDay, msg.sender, quantity, bidRate, issuanceCurrency, referenceCurrency);

        // Interactions
        // Return the commit bond first so it can fund the bid escrow in the same transaction.
        if (a.params.commitBondMinor > 0) {
            $.escrowContract.releaseCommitBond(emissionDay, msg.sender);
        }
        $.escrowContract.lockFunds(emissionDay, msg.sender, a.params.promisLoadMinor, bidRate, quantity);
    }

    /// @inheritdoc IIntexAuction
    function claimRefund(uint32 emissionDay, address bidder) external override nonReentrant {
        IntexAuctionStorage storage $ = _s();
        uint256 position = $.revealedBidIndex[emissionDay][bidder];
        if (position == 0) revert BidNotFound();
        (bool closed, bool finalized) = $.escrowContract.releaseRefund(emissionDay, bidder);
        if (closed && finalized) {
            delete $.revealedBids[emissionDay][position - 1];
            delete $.revealedBidIndex[emissionDay][bidder];
        }
    }

    /// @inheritdoc IIntexAuction
    function redeemWinNote(uint32 emissionDay, address bidder) external override nonReentrant {
        IntexAuctionStorage storage $ = _s();
        uint256 position = $.revealedBidIndex[emissionDay][bidder];
        if (position == 0) revert BidNotFound();
        IIntexAuction.SubmittedBidData storage bid = $.revealedBids[emissionDay][position - 1];
        uint16 issuanceCurrency = bid.issuanceCurrency;
        uint16 referenceCurrency = bid.referenceCurrency;
        IIntexNFT1155 intex = $.intexContract;
        bytes14 seriesId = intex.seriesIdOf(emissionDay, issuanceCurrency, referenceCurrency);
        if (seriesId == bytes14(0)) revert SeriesNotCreated(emissionDay, issuanceCurrency, referenceCurrency);

        // A WinNote exists only after the day cleared, so the bids relay no longer reads this bid.
        delete $.revealedBids[emissionDay][position - 1];
        delete $.revealedBidIndex[emissionDay][bidder];

        uint16 units = $.escrowContract.redeemWinNote(emissionDay, bidder);
        emit WinNoteRedeemed(emissionDay, bidder, seriesId);
        // A forfeited series took the units with it.
        if (intex.readData(seriesId).state != IIntexNFT1155.IntexState.Forfeited) {
            intex.issueIntex(bidder, units, seriesId);
        }
    }

    /// @inheritdoc IIntexAuction
    function claimCommitBond(uint32 emissionDay, address bidder) external override nonReentrant {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData storage a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();

        // A bond that outlives its reveal window belongs to a no-reveal. It waits until the end of
        // the penalty window anchored at the (possibly snapped-forward) `revealEnd`.
        uint32 claimableAt = a.schedule.revealEnd + UNREVEALED_BOND_LOCK_PERIOD;
        if (uint32(block.timestamp) < claimableAt) {
            revert CommitBondNotYetClaimable(claimableAt, uint32(block.timestamp));
        }

        // Pays the stored bidder. Reverts CommitBondNotFound in the escrow when no bond is live.
        $.escrowContract.releaseCommitBond(emissionDay, bidder);
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
    /// @param committedHash The `keccak256(signature)` previously stored by `commitBid`.
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
    function revealedBidsCount(uint32 emissionDay) external view override returns (uint256) {
        return _s().revealedBids[emissionDay].length;
    }

    /// @inheritdoc IIntexAuction
    function revealedBidsSlice(uint32 emissionDay, uint256 offset, uint256 limit)
        external
        view
        override
        returns (IIntexAuction.SubmittedBidData[] memory slice)
    {
        IIntexAuction.SubmittedBidData[] storage bids = _s().revealedBids[emissionDay];
        uint256 length = bids.length;
        if (offset >= length) return new IIntexAuction.SubmittedBidData[](0);
        uint256 end = offset + limit;
        if (end > length) end = length;
        slice = new IIntexAuction.SubmittedBidData[](end - offset);
        for (uint256 i = 0; i < slice.length; i++) {
            slice[i] = bids[offset + i];
        }
    }

    /// @inheritdoc IIntexAuction
    function getAuctionDetails(uint32 emissionDay)
        external
        view
        override
        returns (IIntexAuction.AuctionData memory auctionData, IIntexAuction.SubmittedBidData[] memory bidsData)
    {
        IntexAuctionStorage storage $ = _s();
        IIntexAuction.AuctionData memory a = $.auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();
        return (a, $.revealedBids[emissionDay]);
    }

    /// @inheritdoc IIntexAuction
    function getAuctionStage(uint32 emissionDay) external view override returns (IIntexAuction.AuctionStage) {
        return _getAuctionStage(emissionDay);
    }

    // --- Internal helpers ---
    /// @notice Compute the current auction stage from the schedule and emission-day state.
    /// @dev Reverts `AuctionNotFound` when the series has no entry. Stage resolution:
    ///      - A cancelled day short-circuits to `Cancelled`.
    ///      - A cleared auction short-circuits to `Completed`. The `cleared` flag, set by
    ///        `executeAuctionClearing`, also covers a no-sale clearing whose rate is 0.
    ///      - In all other cases the stage follows the stored schedule timestamps.
    /// @param emissionDay Emission day (yyyymmdd).
    /// @return Current auction stage.
    function _getAuctionStage(uint32 emissionDay) internal view returns (IIntexAuction.AuctionStage) {
        IIntexAuction.AuctionData storage a = _s().auctions[emissionDay];
        if (a.schedule.commitEnd == 0) revert AuctionNotFound();

        if (a.dayState == IIntexAuction.DayState.Cancelled) {
            return IIntexAuction.AuctionStage.Cancelled;
        }

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
