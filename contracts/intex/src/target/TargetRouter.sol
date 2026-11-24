// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {AccessControlUpgradeable} from "@openzeppelin/contracts-upgradeable/access/AccessControlUpgradeable.sol";
import {ReentrancyGuardTransient} from "@openzeppelin/contracts/utils/ReentrancyGuardTransient.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {SafeCast} from "@openzeppelin/contracts/utils/math/SafeCast.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";

import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "../shared/interfaces/IIntexNFT1155.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";
import {IERC7786TokenBridge} from "@tokens/interfaces/IERC7786TokenBridge.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {DESIS} from "@precompiles/IDesis.sol";
import {IIntexFactory, INTEX_FACTORY} from "@precompiles/IIntexFactory.sol";
import {ITargetRouter} from "./interfaces/ITargetRouter.sol";
import {ERC7786MessengerBase} from "../shared/ERC7786MessengerBase.sol";
import {BridgeMsgCodec} from "../shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "../shared/libs/IntexGas.sol";
import {TargetInbound} from "./libs/TargetInbound.sol";
import {
    IntexIssuanceState,
    IntexAuctionBidsSendState,
    FailedIntexAuctionProceedsSend,
    IntexAuctionReturnState,
    TargetRouterStorage
} from "./TargetRouterStorage.sol";

/// @title TargetRouter
/// @author Outbe
/// @notice BNB-side router: sends BIDS_BATCH to Outbe and receives auction/series messages from
///         Outbe over the protocol-agnostic ERC-7786 bridge (the `crosschain` hub). The bridge
///         selects the active transport.
/// @dev UUPS upgradeable behind an ERC1967 proxy. The bridge is an implementation immutable (from
///      {ERC7786MessengerBase}), so every upgrade must pass the same bridge to the constructor.
///      All auction/series auction messages are keyed by `emissionDay`. Series messages
///      (issuance/mark) are keyed by `seriesId`.
contract TargetRouter is
    ITargetRouter,
    ERC7786MessengerBase,
    AccessControlUpgradeable,
    ReentrancyGuardTransient,
    UUPSUpgradeable
{
    using SafeERC20 for IERC20;

    /// @notice Max BIDS_BATCH count per day's relay. The receiver's 256-bit arrival mask bounds it.
    uint16 internal constant MAX_BIDS_PAGES = 256;

    /// @notice Gates `receiveFromOutbe`: the Interchain Account of Outbe on this chain.
    bytes32 public constant OUTBE_ICA_ROLE = keccak256("OUTBE_ICA_ROLE");

    /// @notice Destination chainId of Outbe. It is the sole peer for every outbound send and the
    ///         only accepted source.
    uint32 public immutable OUTBE_CHAIN_ID;

    /// @notice The WCOEN token bridge, which carries the proceeds to Outbe. CREATE3 puts it at the
    ///         same address on every chain.
    IERC7786TokenBridge public immutable WCOEN_BRIDGE;

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.TargetRouter")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _STORAGE_SLOT = 0x69b6aeeb915a7ddfacf9fc7eeda850d126d37a2c760f56ea4c74fddcae77ba00;

    function _ts() private pure returns (TargetRouterStorage storage $) {
        assembly ("memory-safe") {
            $.slot := _STORAGE_SLOT
        }
    }

    /// @custom:oz-upgrades-unsafe-allow constructor
    constructor(address bridge_, uint32 outbeChainId_, address wcoenBridge_) ERC7786MessengerBase(bridge_) {
        if (wcoenBridge_ == address(0)) revert ZeroAddress("wcoenBridge");
        OUTBE_CHAIN_ID = outbeChainId_;
        WCOEN_BRIDGE = IERC7786TokenBridge(wcoenBridge_);
        _disableInitializers();
    }

    /// @notice Initializes the proxy: contract admin.
    /// @param _delegate Receiver of `DEFAULT_ADMIN_ROLE`.
    function initialize(address _delegate) external initializer {
        if (_delegate == address(0)) revert ZeroAddress("delegate");
        __AccessControl_init();
        _grantRole(DEFAULT_ADMIN_ROLE, _delegate);
    }

    /// @dev Upgrades are gated by the admin role.
    function _authorizeUpgrade(address newImplementation) internal override onlyRole(DEFAULT_ADMIN_ROLE) {}

    // --- Storage getters ---
    /// @notice Auction contract that originates outbound bids and receives inbound stage
    ///         transitions.
    function intexAuction() external view returns (IIntexAuction) {
        return _ts().intexAuction;
    }

    /// @notice IntexNFT1155 contract that issuance and mark-called messages apply to.
    function intexContract() external view returns (IIntexNFT1155) {
        return _ts().intexContract;
    }

    /// @notice EscrowAdapter contract that refund instructions are forwarded to for finalization.
    function escrow() external view returns (IEscrowAdapter) {
        return _ts().escrow;
    }

    /// @notice Parked proceeds route by enqueue index.
    function getFailedIntexAuctionProceedsSend(uint256 idx)
        external
        view
        returns (uint32 emissionDay, uint16 pageIndex, uint128 amount, bool exists, bool done)
    {
        FailedIntexAuctionProceedsSend storage p = _ts().failedIntexAuctionProceedsSend[idx];
        return (p.emissionDay, p.pageIndex, p.amount, p.exists, p.done);
    }

    /// @notice How many proceeds sends have ever failed here. `done` in the view tells which are
    ///         resolved.
    function failedIntexAuctionProceedsSendCount() external view returns (uint256) {
        return _ts().nextFailedIntexAuctionProceedsSendIdx;
    }

    /// @notice How far the day's bids relay has got.
    /// @param emissionDay Emission day (yyyymmdd).
    function getIntexAuctionBidsSendState(uint32 emissionDay)
        external
        view
        returns (uint16 nextPage, uint16 totalPages, bool done)
    {
        IntexAuctionBidsSendState storage p = _ts().intexAuctionBidsSendState[emissionDay];
        return (p.nextPage, p.totalPages, p.done);
    }

    /// @notice Issuance chunk progress of `emissionDay` on this chain: applied so far and the
    ///         declared total (0 until the first chunk lands).
    function getIntexIssuanceState(uint32 emissionDay) external view returns (uint16 seen, uint16 total) {
        TargetRouterStorage storage $ = _ts();
        IntexIssuanceState memory progress = $.intexIssuanceState[emissionDay];
        return (progress.pagesSeen, progress.totalPages);
    }

    /// @notice Refund chunk progress of `emissionDay` on this chain: applied so far and the
    ///         declared total (0 until the first chunk lands).
    function getIntexAuctionReturnState(uint32 emissionDay) external view returns (uint16 seen, uint16 total) {
        TargetRouterStorage storage $ = _ts();
        IntexAuctionReturnState memory progress = $.intexAuctionReturnState[emissionDay];
        return (progress.pagesSeen, progress.totalPages);
    }

    /// @notice Whether issuance chunk `pageIndex` of `emissionDay` has been applied here.
    function intexSeriesPageApplied(uint32 emissionDay, uint16 pageIndex) external view returns (bool) {
        // forge-lint: disable-next-line(incorrect-shift) -- the bit of chunk `pageIndex`
        return _ts().intexSeriesPagesApplied[emissionDay] & (1 << pageIndex) != 0;
    }

    /// @notice Call time of the Called mark waiting for `seriesId` to land here (0 = none).
    function failedIntexSeriesCallNoticeApply(bytes14 seriesId) external view returns (uint64 calledAt) {
        return _ts().failedIntexSeriesCallNoticeApplies[seriesId];
    }

    // --- Admin ---
    /// @inheritdoc ITargetRouter
    function setDependencies(address _intexAuction, address _intexContract, address _escrow)
        external
        onlyRole(DEFAULT_ADMIN_ROLE)
    {
        if (_intexAuction == address(0)) revert ZeroAddress("intexAuction");
        if (_intexContract == address(0)) revert ZeroAddress("intexContract");
        if (_escrow == address(0)) revert ZeroAddress("escrow");

        TargetRouterStorage storage $ = _ts();
        IEscrowAdapter current = $.escrow;
        // A day in flight finalizes on the escrow that took its locks, so the venue stays until nothing is left.
        bool changing = _intexAuction != address($.intexAuction) || _intexContract != address($.intexContract)
            || _escrow != address(current);
        if (address(current) != address(0) && changing && current.hasOutstandingLocks()) {
            revert EscrowHasLiveLocks();
        }
        $.intexAuction = IIntexAuction(_intexAuction);
        $.intexContract = IIntexNFT1155(_intexContract);
        $.escrow = IEscrowAdapter(_escrow);
        emit TargetRouterDependenciesSet(_intexAuction, _intexContract, _escrow);
    }

    // --- Receive ---
    /// @inheritdoc ITargetRouter
    /// @dev nonReentrant guards against re-entry through downstream
    ///      `intexAuction`/`escrow`/`intexContract` calls. The hash of the message stands for its
    ///      id in the events of a handler.
    function receiveFromOutbe(bytes calldata message) external onlyRole(OUTBE_ICA_ROLE) nonReentrant {
        _dispatch(OUTBE_CHAIN_ID, keccak256(message), message);
    }

    /// @dev Dispatch by msgType. A premature message (prerequisite stage not applied) reverts. The
    ///      bridge call reverts with it, and the transport redelivers once the prerequisite lands.
    function _dispatch(uint32 srcChainId, bytes32 receiveId, bytes calldata message) internal override {
        if (srcChainId != OUTBE_CHAIN_ID) revert UnexpectedSourceChain(srcChainId);
        uint8 msgType = BridgeMsgCodec.readHeader(message);
        BridgeMsgCodec.assertMinLength(message, msgType);

        if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_START) {
            TargetInbound.handleIntexAuctionStart(_ts(), srcChainId, receiveId, message);
        } else if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_CLEARING_START) {
            TargetInbound.handleIntexAuctionClearingStart(_ts(), srcChainId, receiveId, message);
        } else if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_RESULT) {
            TargetInbound.handleIntexAuctionResult(_ts(), srcChainId, receiveId, message);
        } else if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_CANCEL) {
            TargetInbound.handleIntexAuctionCancel(_ts(), srcChainId, receiveId, message);
        } else if (msgType == BridgeMsgCodec.MSG_INTEX_SERIES) {
            TargetInbound.handleIntexSeries(_ts(), srcChainId, receiveId, message);
        } else if (msgType == BridgeMsgCodec.MSG_INTEX_AUCTION_WINNERS) {
            TargetInbound.handleIntexAuctionWinners(_ts(), srcChainId, receiveId, message);
        } else if (msgType == BridgeMsgCodec.MSG_INTEX_SERIES_CALL_NOTICE) {
            TargetInbound.handleIntexSeriesCallNotice(_ts(), srcChainId, receiveId, message);
        } else {
            revert BridgeMsgCodec.UnknownMsgType(msgType);
        }
    }

    /// @notice Self-call shim around the relay: lets an inbound delivery bound its gas and keep the
    ///         stage flip even when the relay cannot finish.
    /// @param emissionDay Emission day (yyyymmdd) whose revealed bids are relayed to Outbe.
    function sendIntexAuctionBidsToOutbe(uint32 emissionDay) external {
        if (msg.sender != address(this)) revert NotSelf();
        _sendIntexAuctionBids(emissionDay);
    }

    /// @notice Self-call shim reporting an unfinished day home, isolated so a failed report cannot
    ///         take the round that just succeeded with it.
    /// @param emissionDay Emission day (yyyymmdd) whose remainder is reported.
    function sendIntexAuctionBidsPause(uint32 emissionDay) external {
        if (msg.sender != address(this)) revert NotSelf();
        IntexAuctionBidsSendState storage p = _ts().intexAuctionBidsSendState[emissionDay];
        bytes memory message =
            BridgeMsgCodec.encodeBidsPause(emissionDay, uint32(block.chainid), p.nextPage, p.totalPages);
        bytes32 sendId = _sendTo(_desis(), message, IntexGas.BIDS_PAUSE);
        emit IntexAuctionBidsPauseSent(sendId, emissionDay, p.nextPage, p.totalPages);
    }

    /// @notice Permissionless push for a day whose bids have not all left, for a relay float that
    ///         ran dry. Only a day past its reveal accepts it, and only the inbound CLEARING sets
    ///         that stage.
    /// @param emissionDay Emission day (yyyymmdd) to continue relaying.
    function sendIntexAuctionBids(uint32 emissionDay) external nonReentrant {
        TargetRouterStorage storage $ = _ts();
        if ($.intexAuctionBidsSendState[emissionDay].done) revert NoIntexAuctionBidsToSend(emissionDay);
        if ($.intexAuction.auctionStage(emissionDay) != IIntexAuction.AuctionStage.AwaitingResult) {
            revert NoIntexAuctionBidsToSend(emissionDay);
        }
        uint16 pageBefore = $.intexAuctionBidsSendState[emissionDay].nextPage;
        // Sends go to the immutable bridge. The writes after them are the relay's own progress.
        // slither-disable-next-line reentrancy-eth
        _sendIntexAuctionBids(emissionDay);
        _sendIntexAuctionBidsPauseIfAdvanced(emissionDay, pageBefore);
    }

    function _sendIntexAuctionBidsPauseIfAdvanced(uint32 emissionDay, uint16 pageBefore) internal {
        IntexAuctionBidsSendState storage p = _ts().intexAuctionBidsSendState[emissionDay];
        if (!TargetInbound.advanced(p, pageBefore)) return;
        try this.sendIntexAuctionBidsPause(emissionDay) {}
        catch (bytes memory errorData) {
            emit IntexAuctionBidsPauseSendFailed(emissionDay, p.nextPage, p.totalPages, errorData);
        }
    }

    /// @notice Relay the day's revealed bids in chunked BIDS_BATCH sends, resuming where the last
    ///         round stopped.
    /// @dev The first round freezes the span. Reveals are closed by then, so a bid's chunk never
    ///      moves. Every chunk carries `pageIndex`/`totalPages` for the unordered bridge. The
    ///      marker goes with the last chunk. No bids -> one empty batch (0 of 1).
    function _sendIntexAuctionBids(uint32 emissionDay) internal {
        TargetRouterStorage storage $ = _ts();
        IntexAuctionBidsSendState storage progress = $.intexAuctionBidsSendState[emissionDay];
        if (progress.done) return;

        uint256 bidsCount = $.intexAuction.revealedIntexAuctionBidsCount(emissionDay);
        uint16 totalPages = progress.totalPages;
        if (totalPages == 0) {
            uint256 maxPage = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN;
            totalPages = bidsCount == 0 ? 1 : SafeCast.toUint16((bidsCount + maxPage - 1) / maxPage);
            // The receiver tracks page arrival in a 256-bit mask, so it rejects any relay with
            // more than 256 pages. Fail here instead of sending a doomed relay it drops page by page.
            if (totalPages > MAX_BIDS_PAGES) revert TooManyIntexAuctionBidsPages(emissionDay, totalPages);
            progress.totalPages = totalPages;
        }

        uint16 firstPage = progress.nextPage;
        uint16 page = firstPage;
        while (page < totalPages) {
            // The last chunk has to leave room for the marker that follows it in the same round.
            uint256 need =
                page + 1 == totalPages ? IntexGas.SEND_PAGE_GAS + IntexGas.SEND_TOTALS_GAS : IntexGas.SEND_PAGE_GAS;
            if (gasleft() <= need) break;
            _sendBidsPage(emissionDay, page, totalPages, bidsCount);
            ++page;
        }
        progress.nextPage = page;

        if (page < totalPages) {
            // A run that sent some pages is reported by its pause message instead.
            if (page == firstPage) emit IntexAuctionBidsSendFailed(emissionDay, page, totalPages, "");
            return;
        }

        progress.done = true;
        // Completeness marker in the same round as the last chunk, so it can never outrun a lost
        // sibling.
        // slither-disable-next-line reentrancy-eth
        _sendIntexAuctionBidsTotals(emissionDay, totalPages, SafeCast.toUint32(bidsCount));
    }

    /// @dev Read and send one chunk: pull from the auction only the bids it carries.
    function _sendBidsPage(uint32 emissionDay, uint16 pageIndex, uint16 totalPages, uint256 bidsCount) private {
        uint256 maxPage = BridgeMsgCodec.MAX_PAYLOAD_ARRAY_LEN;
        uint256 offset = uint256(pageIndex) * maxPage;
        uint256 pageLen = bidsCount > offset ? bidsCount - offset : 0;
        if (pageLen > maxPage) pageLen = maxPage;

        address[] memory bidders = new address[](pageLen);
        uint256[] memory packedBids = new uint256[](pageLen);
        if (pageLen != 0) {
            IIntexAuction.SubmittedIntexAuctionBidData[] memory bids =
                _ts().intexAuction.revealedIntexAuctionBidsSlice(emissionDay, offset, pageLen);
            for (uint256 i = 0; i < pageLen; i++) {
                IIntexAuction.SubmittedIntexAuctionBidData memory bid = bids[i];
                bidders[i] = bid.bidder;
                packedBids[i] = BridgeMsgCodec.packBid(
                    bid.intexQuantity, bid.intexBidRate, bid.timestamp, bid.issuanceCurrency, bid.referenceCurrency
                );
            }
        }

        _sendOneIntexAuctionBidsPage(emissionDay, pageIndex, totalPages, bidders, packedBids);
    }

    /// @dev Encode and `_send` the BIDS_TOTALS completeness marker for a day. Carries this chain's
    ///      chainId as its source. The receiver cross-checks it against the authenticated source.
    function _sendIntexAuctionBidsTotals(uint32 emissionDay, uint16 totalPages, uint32 totalBids) internal {
        bytes memory message =
            BridgeMsgCodec.encodeBidsTotals(emissionDay, uint32(block.chainid), totalPages, totalBids);
        bytes32 sendId = _sendTo(_desis(), message, IntexGas.BIDS_TOTALS);
        emit IntexAuctionBidsTotalsSent(sendId, emissionDay, totalPages, totalBids);
    }

    /// @dev Encode and `_send` a single BIDS_BATCH to Outbe. The body carries this chain's chainId
    ///      as its source (cross-checked by the receiver against the authenticated source). The
    ///      relay float funds the send.
    function _sendOneIntexAuctionBidsPage(
        uint32 emissionDay,
        uint16 pageIndex,
        uint16 totalPages,
        address[] memory bidders,
        uint256[] memory packedBids
    ) internal returns (bytes32 sendId) {
        bytes memory message = BridgeMsgCodec.encodeBidsPage(
            emissionDay, uint32(block.chainid), pageIndex, totalPages, bidders, packedBids
        );
        sendId = _sendTo(_desis(), message, IntexGas.bidsPage(bidders.length));
        emit IntexAuctionBidsSent(sendId, emissionDay, pageIndex, totalPages, uint32(bidders.length));
    }

    /// @notice Self-call shim around one Called mark. It isolates a series that will not take it.
    /// @param seriesId Series the mark applies to.
    /// @param calledAt Origin's call timestamp.
    function applyIntexSeriesCallNotice(bytes14 seriesId, uint64 calledAt) external {
        if (msg.sender != address(this)) revert NotSelf();
        _ts().intexContract.markCalled(seriesId, calledAt);
    }

    /// @notice Permissionless apply of the mark waiting in `seriesId`'s slot. Reverts if nothing
    ///         waits or the series still will not take it, leaving the slot in place.
    /// @param seriesId Series whose slotted mark to apply.
    function retryIntexSeriesCallNoticeApply(bytes14 seriesId) external nonReentrant {
        TargetRouterStorage storage $ = _ts();
        uint64 calledAt = $.failedIntexSeriesCallNoticeApplies[seriesId];
        if (calledAt == 0) revert NoFailedIntexSeriesCallNoticeApply(seriesId);
        delete $.failedIntexSeriesCallNoticeApplies[seriesId];
        $.intexContract.markCalled(seriesId, calledAt);
    }

    /// @notice Self-call shim around `_sendIntexAuctionProceeds`. Only callable by this contract itself.
    function sendIntexAuctionProceedsExt(uint32 emissionDay, uint16 pageIndex, uint128 amount) external {
        if (msg.sender != address(this)) revert NotSelf();
        _sendIntexAuctionProceeds(emissionDay, pageIndex, amount);
    }

    /// @notice Permissionless retry of a previously deferred proceeds route.
    /// @param idx Index of the parked route to flush.
    function retryIntexAuctionProceedsSend(uint256 idx) external nonReentrant {
        FailedIntexAuctionProceedsSend storage p = _ts().failedIntexAuctionProceedsSend[idx];
        if (!p.exists) revert NoSuchFailedIntexAuctionProceedsSend(idx);
        if (p.done) revert AlreadyResolved(idx);
        p.done = true;
        _sendIntexAuctionProceeds(p.emissionDay, p.pageIndex, p.amount);
    }

    /// @dev Desis on Outbe, the recipient of the bids.
    function _desis() private view returns (bytes memory) {
        return InteroperableAddress.formatEvmV1(OUTBE_CHAIN_ID, DESIS);
    }

    /// @dev Route `amount` WCOEN of a chunk to IntexFactory on Outbe, which burns it.
    function _sendIntexAuctionProceeds(uint32 emissionDay, uint16 pageIndex, uint128 amount) internal {
        bytes32 sendId;
        if (OUTBE_CHAIN_ID == block.chainid) _handOverProceeds(emissionDay, pageIndex, amount);
        else sendId = _bridgeProceeds(emissionDay, pageIndex, amount);
        emit IntexAuctionProceedsSent(sendId, emissionDay, pageIndex, amount);
    }

    /// @dev The WCOEN the escrow paid out for the day; the token bridge carries only its own token.
    function _proceedsToken(uint32 emissionDay) private view returns (IERC20 token) {
        token = _ts().escrow.dayIntexAuctionAsset(emissionDay);
        IERC20 bridgeToken = WCOEN_BRIDGE.token();
        if (token != bridgeToken) revert ProceedsTokenNotBridged(address(token), address(bridgeToken));
    }

    /// @dev On Outbe itself IntexFactory sits on this chain and takes the WCOEN in one call.
    function _handOverProceeds(uint32 emissionDay, uint16 pageIndex, uint128 amount) private {
        _proceedsToken(emissionDay).forceApprove(INTEX_FACTORY, amount);
        IIntexFactory(INTEX_FACTORY).receiveLocalProceeds(emissionDay, pageIndex, amount);
    }

    /// @dev The token bridge carries the WCOEN with the day and the page, and the relay float pays its fee.
    function _bridgeProceeds(uint32 emissionDay, uint16 pageIndex, uint128 amount) private returns (bytes32 sendId) {
        bytes memory extraData = abi.encode(emissionDay, uint32(pageIndex));
        _proceedsToken(emissionDay).forceApprove(address(WCOEN_BRIDGE), amount);
        uint256 fee =
            WCOEN_BRIDGE.quoteSend(OUTBE_CHAIN_ID, INTEX_FACTORY, amount, extraData, IntexGas.PROCEEDS_COMPOSE);
        // slither-disable-next-line arbitrary-send-eth
        sendId = WCOEN_BRIDGE.sendAndCall{value: fee}(
            OUTBE_CHAIN_ID, INTEX_FACTORY, amount, extraData, IntexGas.PROCEEDS_COMPOSE
        );
    }

    /// @notice ERC-165 support check, resolving the AccessControl interface ids.
    function supportsInterface(bytes4 interfaceId) public view override(AccessControlUpgradeable) returns (bool) {
        return super.supportsInterface(interfaceId);
    }
}
