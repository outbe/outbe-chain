// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexAuction} from "../interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "../../shared/interfaces/IIntexNFT1155.sol";
import {IEscrowAdapter} from "../interfaces/IEscrowAdapter.sol";
import {ITargetRouter} from "../interfaces/ITargetRouter.sol";
import {ERC7786MessengerBase} from "../../shared/ERC7786MessengerBase.sol";
import {BridgeMsgCodec} from "../../shared/libs/BridgeMsgCodec.sol";
import {InboundReason} from "../../shared/libs/InboundReason.sol";
import {
    TargetRouterStorage,
    PendingBidsRelay,
    PendingIssuanceMint,
    PendingMark,
    PendingProceedsRoute
} from "../TargetRouterStorage.sol";

/// @dev Self-call shims the router exposes for per-item isolation; called on `address(this)` from the
///      delegated library context, so `msg.sender == address(this)` holds inside the shim.
interface ITargetRouterShims {
    function relayBidsToOutbe(uint32 worldwideDay) external;
    function mintIssuanceOne(bytes14 seriesId, address to, uint256 quantity) external;
    function applyMarkOne(bytes14 seriesId, uint8 msgType) external;
    function routeProceedsExt(uint32 worldwideDay, uint128 amount) external;
}

/// @title TargetInbound
/// @author Outbe
/// @notice Inbound message handlers of {TargetRouter}, linked as an external library so their bodies stay off
///         the router's EIP-170 runtime size. Every function runs via DELEGATECALL in the router's context.
library TargetInbound {
    /// @notice Decode AUCTION_STAGE_START and forward the day state, schedule and params to the Auction contract.
    /// @dev An auction the day already has (same terms → duplicate, other terms → conflict), a schedule the day
    ///      can no longer honour, or an unknown day state are acknowledged without effect: no later state makes
    ///      such a START applicable. Anything else propagates so the bridge redelivers.
    function handleAuctionStageStart(TargetRouterStorage storage $, uint32 srcChainId, bytes calldata message)
        external
    {
        (
            uint32 worldwideDay,
            IIntexAuction.WorldwideDayState dayState,
            IIntexAuction.AuctionSchedule memory schedule,
            IIntexAuction.AuctionParams memory params
        ) = BridgeMsgCodec.decodeAuctionParams(message);

        try $.auction.auctionStart(worldwideDay, dayState, schedule, params) {
            emit ITargetRouter.AuctionStageReceived(srcChainId, worldwideDay, BridgeMsgCodec.MSG_AUCTION_STAGE_START);
        } catch (bytes memory reason) {
            bytes4 selector = _selectorOf(reason);
            uint8 why;
            if (selector == IIntexAuction.AuctionAlreadyExists.selector) {
                why = _sameAuction($, worldwideDay, dayState, schedule, params)
                    ? InboundReason.DUPLICATE
                    : InboundReason.CONFLICT;
            } else if (selector == IIntexAuction.InvalidSchedule.selector) {
                bool late = dayState == IIntexAuction.WorldwideDayState.Green && schedule.commitEnd <= block.timestamp;
                why = late ? InboundReason.LATE : InboundReason.INVALID;
            } else if (selector == IIntexAuction.InvalidDayState.selector) {
                why = InboundReason.INVALID;
            } else {
                _bubble(reason);
            }
            _ignore(srcChainId, BridgeMsgCodec.MSG_AUCTION_STAGE_START, bytes32(uint256(worldwideDay)), why);
        }
    }

    /// @notice Decode AUCTION_STAGE_CLEARING, forward to Auction, then relay revealed bids to Outbe.
    /// @dev A day already past clearing (Completed), a cancelled day or a day this chain never opened cannot take
    ///      the transition any more and are acknowledged without effect (and without a relay). A day whose commit
    ///      stage is still running propagates: time alone makes the transition valid, so the bridge redelivers.
    ///      Only the outbound relay is caught (parked on failure).
    function handleAuctionStageClearing(TargetRouterStorage storage $, uint32 srcChainId, bytes calldata message)
        external
    {
        uint32 worldwideDay = BridgeMsgCodec.decodeAuctionStageClearing(message);

        try $.auction.startClearingStage(worldwideDay) {}
        catch (bytes memory reason) {
            bytes4 selector = _selectorOf(reason);
            uint8 why;
            if (selector == IIntexAuction.StageRequired.selector) {
                IIntexAuction.AuctionStage current = _stageRequiredCurrent(reason);
                if (current != IIntexAuction.AuctionStage.Completed && current != IIntexAuction.AuctionStage.Cancelled)
                {
                    _bubble(reason);
                }
                why = InboundReason.OBSOLETE;
            } else if (selector == IIntexAuction.AuctionNotFound.selector) {
                why = InboundReason.UNKNOWN;
            } else {
                _bubble(reason);
            }
            _ignore(srcChainId, BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING, bytes32(uint256(worldwideDay)), why);
            return;
        }

        // Relay the revealed bids exactly once. A redelivered CLEARING must not re-relay under a fresh generation.
        if (!$.clearingRelayed[worldwideDay]) {
            $.clearingRelayed[worldwideDay] = true;
            try ITargetRouterShims(address(this)).relayBidsToOutbe(worldwideDay) {
            // ok — bids forwarded
            }
            catch (bytes memory reason) {
                uint256 idx = $.nextPendingBidsRelayIdx++;
                $.pendingBidsRelays[idx] = PendingBidsRelay({worldwideDay: worldwideDay, exists: true, done: false});
                emit ITargetRouter.BidsRelayDeferred(idx, worldwideDay, reason);
            }
        }

        emit ITargetRouter.AuctionStageReceived(srcChainId, worldwideDay, BridgeMsgCodec.MSG_AUCTION_STAGE_CLEARING);
    }

    /// @notice Decode AUCTION_RESULT and execute auction clearing on the Auction contract.
    /// @dev A result the day already holds (same → duplicate, other → conflict), a cancelled or unknown day and a
    ///      result that fails the auction's permanent sanity bounds are acknowledged without effect. A day whose
    ///      reveal stage has not closed yet (clock skew) propagates so the bridge redelivers.
    function handleAuctionResult(TargetRouterStorage storage $, uint32 srcChainId, bytes calldata message) external {
        (uint32 worldwideDay, uint32 issuedIntexCount, uint64 auctionClearingRate, uint32 wonBidsCount) =
            BridgeMsgCodec.decodeAuctionResult(message);

        try $.auction.executeAuctionClearing(worldwideDay, issuedIntexCount, auctionClearingRate, wonBidsCount) {
            emit ITargetRouter.AuctionResultReceived(srcChainId, worldwideDay, issuedIntexCount, auctionClearingRate);
        } catch (bytes memory reason) {
            bytes4 selector = _selectorOf(reason);
            uint8 why;
            if (selector == IIntexAuction.StageRequired.selector) {
                IIntexAuction.AuctionStage current = _stageRequiredCurrent(reason);
                if (current == IIntexAuction.AuctionStage.Completed) {
                    why = _sameResult($, worldwideDay, issuedIntexCount, auctionClearingRate, wonBidsCount)
                        ? InboundReason.DUPLICATE
                        : InboundReason.CONFLICT;
                } else if (current == IIntexAuction.AuctionStage.Cancelled) {
                    why = InboundReason.OBSOLETE;
                } else {
                    _bubble(reason);
                }
            } else if (selector == IIntexAuction.AuctionNotFound.selector) {
                why = InboundReason.UNKNOWN;
            } else if (
                selector == IIntexAuction.WonBidsExceedRevealed.selector
                    || selector == IIntexAuction.ClearingRateBelowMin.selector
                    || selector == IIntexAuction.ZeroValue.selector
                    || selector == IIntexAuction.IssuedPromisOverflow.selector
            ) {
                why = InboundReason.INVALID;
            } else {
                _bubble(reason);
            }
            _ignore(srcChainId, BridgeMsgCodec.MSG_AUCTION_RESULT, bytes32(uint256(worldwideDay)), why);
        }
    }

    /// @dev Whether the auction this chain holds for `worldwideDay` was opened under the START's terms. The reveal
    ///      end is left out: CLEARING snaps it forward, and a START repeated after that is still the same START.
    function _sameAuction(
        TargetRouterStorage storage $,
        uint32 worldwideDay,
        IIntexAuction.WorldwideDayState dayState,
        IIntexAuction.AuctionSchedule memory schedule,
        IIntexAuction.AuctionParams memory params
    ) private view returns (bool) {
        IIntexAuction.AuctionData memory a = $.auction.getAuctionInfo(worldwideDay);
        return a.worldwideDayState == dayState && a.schedule.commitEnd == schedule.commitEnd
            && a.schedule.issuanceEnd == schedule.issuanceEnd
            && keccak256(abi.encode(a.params)) == keccak256(abi.encode(params));
    }

    /// @dev Whether the result this chain holds for `worldwideDay` is the one the RESULT carries.
    function _sameResult(
        TargetRouterStorage storage $,
        uint32 worldwideDay,
        uint32 issuedIntexCount,
        uint64 auctionClearingRate,
        uint32 wonBidsCount
    ) private view returns (bool) {
        IIntexAuction.AuctionResult memory r = $.auction.getAuctionInfo(worldwideDay).result;
        return r.issuedIntexCount == issuedIntexCount && r.auctionClearingRate == auctionClearingRate
            && r.wonBidsCount == wonBidsCount;
    }

    /// @dev `currentStage` argument of a `StageRequired(requiredStage, currentStage)` revert payload.
    function _stageRequiredCurrent(bytes memory reason) private pure returns (IIntexAuction.AuctionStage current) {
        // [len][selector(4)][requiredStage(32)][currentStage(32)]
        uint256 raw;
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            raw := mload(add(reason, 0x44))
        }
        // forge-lint: disable-next-line(unsafe-typecast) -- the low byte is the enum value
        current = IIntexAuction.AuctionStage(uint8(raw));
    }

    function _selectorOf(bytes memory reason) private pure returns (bytes4 selector) {
        if (reason.length < 4) return bytes4(0);
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            selector := mload(add(reason, 0x20))
        }
    }

    /// @dev Re-raise a caught revert payload unchanged.
    function _bubble(bytes memory reason) private pure {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            revert(add(reason, 0x20), mload(reason))
        }
    }

    /// @notice Decode one ISSUANCE_INSTRUCTIONS chunk, create the series it names, and mint each winner once.
    /// @dev A repeated chunk, a chunk whose header disagrees with the day's run, and a series whose stored
    ///      params differ from the chunk's are acknowledged without effect (`InboundMessageIgnored`); the last
    ///      applied chunk emits `IssuanceCompleted`.
    function handleIssuanceInstructions(TargetRouterStorage storage $, uint32 srcChainId, bytes calldata message)
        external
    {
        (
            uint32 worldwideDay,
            uint16 chunkIndex,
            uint16 totalChunks,
            BridgeMsgCodec.IssuanceInstructionsPayload[] memory series
        ) = BridgeMsgCodec.decodeIssuanceInstructions(message);

        bytes32 chunkKey = bytes32((uint256(worldwideDay) << 16) | chunkIndex);
        if ($.issuanceChunkApplied[worldwideDay][chunkIndex]) {
            _ignore(srcChainId, BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS, chunkKey, InboundReason.DUPLICATE);
            return;
        }
        uint16 knownTotal = $.issuanceTotalChunks[worldwideDay];
        if (knownTotal != 0 && knownTotal != totalChunks) {
            _ignore(srcChainId, BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS, chunkKey, InboundReason.CONFLICT);
            return;
        }
        // A chunk naming a series this chain already holds under other terms is an origin disagreement: none of
        // it is applied, so a corrected resend of the same index can still land.
        for (uint256 s = 0; s < series.length; s++) {
            if ($.intex.seriesExists(series[s].seriesId) && !_sameSeries($, series[s])) {
                _ignore(srcChainId, BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS, chunkKey, InboundReason.CONFLICT);
                return;
            }
        }

        if (knownTotal == 0) $.issuanceTotalChunks[worldwideDay] = totalChunks;
        $.issuanceChunkApplied[worldwideDay][chunkIndex] = true;
        uint16 seen = ++$.issuanceChunksSeen[worldwideDay];

        for (uint256 s = 0; s < series.length; s++) {
            _applyIssuance($, srcChainId, series[s]);
        }

        if (seen == totalChunks) emit ITargetRouter.IssuanceCompleted(worldwideDay, totalChunks);
    }

    /// @dev Create the series if this chain has not seen it and mint its winners, each at most once.
    function _applyIssuance(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        BridgeMsgCodec.IssuanceInstructionsPayload memory payload
    ) private {
        // Create-if-absent: any of a day's chunks may be the first to name a series.
        if (!$.intex.seriesExists(payload.seriesId)) {
            $.intex
                .createSeries(
                    IIntexNFT1155.CreateSeriesParams({
                        seriesId: payload.seriesId,
                        worldwideDay: payload.worldwideDay,
                        issuanceCurrency: payload.issuanceCurrency,
                        referenceCurrency: payload.referenceCurrency,
                        issuedIntexCount: payload.issuedIntexCount,
                        promisLoadMinor: payload.promisLoadMinor,
                        entryPriceMinor: payload.entryPriceMinor,
                        floorPriceMinor: payload.floorPriceMinor,
                        callPriceMinor: payload.callPriceMinor,
                        callTrigger: IIntexNFT1155.IntexCallTrigger({
                            callWindow: payload.callWindow,
                            callThreshold: payload.callThreshold,
                            callNoticePeriod: payload.callNoticePeriod
                        })
                    })
                );
        }

        uint256 recipientsLen = payload.recipients.length;
        for (uint256 i = 0; i < recipientsLen; i++) {
            uint256 quantity = payload.quantities[i];
            if (quantity == 0) continue;
            address recipient = payload.recipients[i];
            if ($.issued[payload.seriesId][recipient]) {
                _ignore(
                    srcChainId,
                    BridgeMsgCodec.MSG_ISSUANCE_INSTRUCTIONS,
                    bytes32(abi.encodePacked(payload.seriesId, recipient)),
                    InboundReason.DUPLICATE
                );
                continue;
            }
            // Marked before the mint: a parked mint is still this winner's one allocation.
            $.issued[payload.seriesId][recipient] = true;
            // Per-recipient self-call: a reverting receiver hook parks only that mint, not the whole batch.
            try ITargetRouterShims(address(this)).mintIssuanceOne(payload.seriesId, recipient, quantity) {}
            catch (bytes memory reason) {
                uint256 idx = $.nextPendingIssuanceMintIdx++;
                $.pendingIssuanceMints[idx] = PendingIssuanceMint({
                    seriesId: payload.seriesId, recipient: recipient, quantity: quantity, exists: true, done: false
                });
                emit ITargetRouter.IssuanceMintDeferred(idx, payload.seriesId, recipient, reason);
            }
        }

        emit ITargetRouter.IssuanceInstructionsReceived(srcChainId, payload.seriesId, recipientsLen);
    }

    /// @dev Whether the series this chain holds was created under exactly the chunk's terms.
    function _sameSeries(TargetRouterStorage storage $, BridgeMsgCodec.IssuanceInstructionsPayload memory payload)
        private
        view
        returns (bool)
    {
        IIntexNFT1155.SeriesData memory d = $.intex.readData(payload.seriesId);
        return d.worldwideDay == payload.worldwideDay && d.issuanceCurrency == payload.issuanceCurrency
            && d.referenceCurrency == payload.referenceCurrency && d.issuedIntexCount == payload.issuedIntexCount
            && d.promisLoadMinor == payload.promisLoadMinor && d.entryPriceMinor == payload.entryPriceMinor
            && d.floorPriceMinor == payload.floorPriceMinor && d.callPriceMinor == payload.callPriceMinor
            && d.callTrigger.callWindow == payload.callWindow && d.callTrigger.callThreshold == payload.callThreshold
            && d.callTrigger.callNoticePeriod == payload.callNoticePeriod;
    }

    function _ignore(uint32 srcChainId, uint8 msgType, bytes32 key, uint8 reason) private {
        emit ERC7786MessengerBase.InboundMessageIgnored(srcChainId, msgType, key, reason);
    }

    /// @notice Decode REFUND_INSTRUCTIONS and forward finalization instructions to the EscrowAdapter.
    /// @dev `receiveId` is the escrow finalization tag; escrow dedups on the series' own `finalized` flag.
    function handleRefundInstructions(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        bytes32 receiveId,
        bytes calldata message
    ) external {
        (
            uint32 worldwideDay,
            uint16 chunkIndex,
            uint16 totalChunks,
            address[] memory bidders,
            uint128[] memory refundedAmounts,
            uint128[] memory paidAmounts
        ) = BridgeMsgCodec.decodeRefundInstructions(message);

        uint256 bit = 1 << chunkIndex;
        if ($.refundChunksApplied[worldwideDay] & bit != 0) {
            // Redelivered: already settled and counted.
            emit ITargetRouter.RefundInstructionsReceived(srcChainId, worldwideDay, 0);
            return;
        }

        IEscrowAdapter.FinalizationInstruction[] memory instructions =
            new IEscrowAdapter.FinalizationInstruction[](bidders.length);

        for (uint256 i = 0; i < bidders.length; i++) {
            instructions[i] = IEscrowAdapter.FinalizationInstruction({
                bidder: bidders[i], refundedAmount: refundedAmounts[i], paidAmount: paidAmounts[i]
            });
        }

        // Counted before settling: the escrow refuses instructions once the day is closed.
        $.refundChunksApplied[worldwideDay] |= bit;
        uint16 seen = $.refundChunksSeen[worldwideDay] + 1;
        $.refundChunksSeen[worldwideDay] = seen;
        // `>=` rather than `==`: an overshoot would otherwise leave the day's proceeds
        // accrued in this contract with nothing left to release them.
        bool completesDay = seen >= totalChunks;

        uint128 totalPaid = $.escrowAdapter.finalizeAuction(worldwideDay, receiveId, instructions, completesDay);
        $.refundProceedsAccrued[worldwideDay] += totalPaid;

        // One transfer per day: the origin counts a chain paid on the first delivery.
        if (completesDay) {
            uint128 proceeds = $.refundProceedsAccrued[worldwideDay];
            if (proceeds > 0) {
                $.refundProceedsAccrued[worldwideDay] = 0;
                _routeOrParkProceeds($, worldwideDay, proceeds);
            }
        }

        emit ITargetRouter.RefundInstructionsReceived(srcChainId, worldwideDay, bidders.length);
    }

    /// @notice Decode MARK_CALLED and apply it to every series it carries, parking the ones that
    ///         will not take the mark yet.
    function handleMarkCalled(TargetRouterStorage storage $, uint32 srcChainId, bytes calldata message) external {
        (, bytes14[] memory seriesIds) = BridgeMsgCodec.decodeMarkCalled(message);
        for (uint256 i = 0; i < seriesIds.length; ++i) {
            _applyMark($, srcChainId, seriesIds[i], BridgeMsgCodec.MSG_MARK_CALLED);
        }
    }

    /// @notice Decode MARK_QUALIFIED and apply it to every series it carries, parking the rest.
    /// @dev A pure status flip, so unlike markCalled there is nothing to bridge back to Outbe.
    function handleMarkQualified(TargetRouterStorage storage $, uint32 srcChainId, bytes calldata message) external {
        (, bytes14[] memory seriesIds) = BridgeMsgCodec.decodeMarkQualified(message);
        for (uint256 i = 0; i < seriesIds.length; ++i) {
            _applyMark($, srcChainId, seriesIds[i], BridgeMsgCodec.MSG_MARK_QUALIFIED);
        }
    }

    /// @dev Apply one lifecycle mark through its self-call shim, parking it on revert. Parking keeps a series the
    ///      target has not seen from rejecting the whole message.
    function _applyMark(TargetRouterStorage storage $, uint32 srcChainId, bytes14 seriesId, uint8 msgType) private {
        // solhint-disable-next-line no-empty-blocks
        try ITargetRouterShims(address(this)).applyMarkOne(seriesId, msgType) {}
        catch (bytes memory reason) {
            uint256 idx = $.nextPendingMarkIdx++;
            $.pendingMarks[idx] = PendingMark({seriesId: seriesId, msgType: msgType, exists: true, done: false});
            emit ITargetRouter.MarkDeferred(idx, seriesId, msgType, reason);
            return;
        }
        if (msgType == BridgeMsgCodec.MSG_MARK_CALLED) {
            emit ITargetRouter.MarkCalledReceived(srcChainId, seriesId);
        } else {
            emit ITargetRouter.MarkQualifiedReceived(srcChainId, seriesId);
        }
    }

    /// @dev Route proceeds to Outbe, parking series+amount on failure so a transport/float hiccup never rolls
    ///      back the finalization (the WCOEN is already held here). Retried via `flushPendingProceedsRoute`.
    function _routeOrParkProceeds(TargetRouterStorage storage $, uint32 worldwideDay, uint128 amount) private {
        try ITargetRouterShims(address(this)).routeProceedsExt(worldwideDay, amount) {
        // ok — proceeds routed
        }
        catch (bytes memory reason) {
            uint256 idx = $.nextPendingProceedsRouteIdx++;
            $.pendingProceedsRoutes[idx] =
                PendingProceedsRoute({worldwideDay: worldwideDay, amount: amount, exists: true, done: false});
            emit ITargetRouter.ProceedsRouteDeferred(idx, worldwideDay, amount, reason);
        }
    }
}
