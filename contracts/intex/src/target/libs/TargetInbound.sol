// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexAuction} from "../interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "../../shared/interfaces/IIntexNFT1155.sol";
import {ITargetRouter} from "../interfaces/ITargetRouter.sol";
import {BridgeMsgCodec} from "../../shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "../../shared/libs/IntexGas.sol";
import {LowLevelCall} from "@openzeppelin/contracts/utils/LowLevelCall.sol";
import {InboundReason} from "../../shared/libs/InboundReason.sol";
import {
    IntexAuctionBidsSendState,
    IntexIssuanceState,
    FailedIntexAuctionProceedsSend,
    IntexAuctionReturnState,
    TargetRouterStorage
} from "../TargetRouterStorage.sol";

/// @dev Self-call shims the router exposes for per-item isolation. The library calls them on
///      `address(this)` from the delegated library context, so `msg.sender == address(this)` holds
///      inside the shim.
interface ITargetRouterShims {
    function sendIntexAuctionBidsToOutbe(uint32 emissionDay) external;
    function sendIntexAuctionBidsPause(uint32 emissionDay) external;
    function applyIntexSeriesCallNotice(bytes14 seriesId, uint64 calledAt) external;
    function sendIntexAuctionProceedsExt(uint32 emissionDay, uint16 pageIndex, uint128 amount) external;
}

/// @title TargetInbound
/// @author Outbe
/// @notice Inbound message handlers of {TargetRouter}, linked as an external library so their bodies stay off
///         the router's EIP-170 runtime size. Every function runs via DELEGATECALL in the router's context.
library TargetInbound {
    /// @notice Whether a finished round owes the origin a remainder report. A round that sent nothing has
    ///         nothing to recover from: the origin would answer with the same budget for the same outcome,
    ///         and that is a loop. A finished day's completeness marker reports it instead.
    function advanced(IntexAuctionBidsSendState storage intexAuctionBidsSendState, uint16 pageBefore)
        internal
        view
        returns (bool)
    {
        return !intexAuctionBidsSendState.done && intexAuctionBidsSendState.nextPage > pageBefore;
    }

    /// @notice Decode AUCTION_STAGE_START and forward the day state, schedule and params to the Auction
    ///         contract.
    /// @dev The handler acknowledges these cases without effect, because no later state makes such a
    ///      START applicable:
    ///      - an auction the day already has (same terms -> duplicate, other terms -> conflict)
    ///      - a day a cancel reached first (conflict)
    ///      - a schedule the day can no longer honour
    ///      - an unknown day state
    ///      Anything else propagates so the bridge redelivers.
    function handleIntexAuctionStart(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        bytes32 receiveId,
        bytes calldata message
    ) external {
        (uint32 emissionDay, IIntexAuction.AuctionSchedule memory schedule, IIntexAuction.AuctionParams memory params) =
            BridgeMsgCodec.decodeAuctionParams(message);

        try $.intexAuction.startAuction(emissionDay, schedule, params) {}
        catch (bytes memory reason) {
            bytes4 selector = _selectorOf(reason);
            uint8 why;
            if (selector == IIntexAuction.AuctionAlreadyExists.selector) {
                why = _sameAuction($, emissionDay, schedule, params) ? InboundReason.DUPLICATE : InboundReason.CONFLICT;
            } else if (selector == IIntexAuction.AuctionCanceled.selector) {
                why = InboundReason.CONFLICT;
            } else if (selector == IIntexAuction.InvalidSchedule.selector) {
                why = schedule.commitEnd <= block.timestamp ? InboundReason.LATE : InboundReason.INVALID;
            } else {
                LowLevelCall.bubbleRevert(reason);
            }
            _ignoreMessage(receiveId, srcChainId, why);
        }
    }

    /// @notice Decode INTEX_AUCTION_CLEARING_START, forward to Auction, then intexAuctionBidsSendState revealed bids to Outbe.
    /// @dev These days cannot take the transition any more. The handler acknowledges them without
    ///      effect (and without a intexAuctionBidsSendState):
    ///      - a day already past clearing (Completed)
    ///      - a canceled day
    ///      - a failed day, which found no result in time
    ///      - a day this chain never opened
    ///      A day whose commit stage is still running propagates. Time alone makes the transition
    ///      valid, so the bridge redelivers. Only the outbound intexAuctionBidsSendState is caught (parked on failure).
    function handleIntexAuctionClearingStart(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        bytes32 receiveId,
        bytes calldata message
    ) external {
        uint32 emissionDay = BridgeMsgCodec.decodeIntexAuctionClearingStart(message);

        try $.intexAuction.startAuctionClearing(emissionDay) {}
        catch (bytes memory reason) {
            bytes4 selector = _selectorOf(reason);
            uint8 why;
            if (selector == IIntexAuction.StageRequired.selector) {
                if (!_ended(_stageRequiredCurrent(reason))) {
                    LowLevelCall.bubbleRevert(reason);
                }
                why = InboundReason.OBSOLETE;
            } else if (selector == IIntexAuction.AuctionNotFound.selector) {
                why = InboundReason.NOT_FOUND;
            } else {
                LowLevelCall.bubbleRevert(reason);
            }
            _ignoreMessage(receiveId, srcChainId, why);
            return;
        }

        // Carry the bids as far as this delivery's gas allows, holding back enough to report the rest.
        // The self-call keeps a failed round from taking the stage flip with it.
        IntexAuctionBidsSendState storage state = $.intexAuctionBidsSendState[emissionDay];
        if (state.done) return;
        if (gasleft() <= IntexGas.SEND_PAUSE_GAS) {
            emit ITargetRouter.IntexAuctionBidsSendFailed(emissionDay, state.nextPage, state.totalPages, "");
            return;
        }
        uint16 pageBefore = state.nextPage;
        try ITargetRouterShims(address(this)).sendIntexAuctionBidsToOutbe{gas: gasleft() - IntexGas.SEND_PAUSE_GAS}(
            emissionDay
        ) {}
        catch (bytes memory errorData) {
            emit ITargetRouter.IntexAuctionBidsSendFailed(emissionDay, state.nextPage, state.totalPages, errorData);
        }
        if (advanced(state, pageBefore)) {
            try ITargetRouterShims(address(this)).sendIntexAuctionBidsPause(emissionDay) {}
            catch (bytes memory errorData) {
                emit ITargetRouter.IntexAuctionBidsPauseSendFailed(
                    emissionDay, state.nextPage, state.totalPages, errorData
                );
            }
        }
    }

    /// @notice Decode INTEX_AUCTION_RESULT and execute auction clearing on the Auction contract.
    /// @dev The handler acknowledges these cases without effect:
    ///      - a result the day already holds (same -> duplicate, other -> conflict)
    ///      - a canceled, failed or unknown day
    ///      - a result that fails the auction's permanent sanity bounds
    ///      A day whose reveal stage has not closed yet (clock skew) propagates so the bridge
    ///      redelivers.
    function handleIntexAuctionResult(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        bytes32 receiveId,
        bytes calldata message
    ) external {
        (uint32 emissionDay, BridgeMsgCodec.IntexAuctionChainResult memory r) =
            BridgeMsgCodec.decodeIntexAuctionResult(message);

        try $.intexAuction
            .completeAuction(emissionDay, r.wonQuantity, r.wonBidsCount, r.quantity, r.clearingRate, r.totalDemand) {
            $.intexAuctionResultHash[emissionDay] = keccak256(abi.encode(r));
        } catch (bytes memory reason) {
            bytes4 selector = _selectorOf(reason);
            uint8 why;
            if (selector == IIntexAuction.StageRequired.selector) {
                IIntexAuction.AuctionStage current = _stageRequiredCurrent(reason);
                if (current == IIntexAuction.AuctionStage.Completed) {
                    why = _sameResult($, emissionDay, r) ? InboundReason.DUPLICATE : InboundReason.CONFLICT;
                } else if (
                    current == IIntexAuction.AuctionStage.Canceled || current == IIntexAuction.AuctionStage.Failed
                ) {
                    why = InboundReason.OBSOLETE;
                } else {
                    LowLevelCall.bubbleRevert(reason);
                }
            } else if (selector == IIntexAuction.AuctionNotFound.selector) {
                why = InboundReason.NOT_FOUND;
            } else if (
                selector == IIntexAuction.WonIntexAuctionBidsExceedRevealed.selector
                    || selector == IIntexAuction.ClearingRateBelowMin.selector
                    || selector == IIntexAuction.ZeroValue.selector
                    || selector == IIntexAuction.IssuedPromisOverflow.selector
            ) {
                why = InboundReason.INVALID;
            } else {
                LowLevelCall.bubbleRevert(reason);
            }
            _ignoreMessage(receiveId, srcChainId, why);
        }
    }

    /// @notice Decode INTEX_AUCTION_CANCEL, cancel the day on the Auction contract and close it in the escrow as a
    ///         result of zero units does, so every lock and bond is returned at once.
    /// @dev The handler acknowledges these cases without effect:
    ///      - a day that is already canceled (duplicate)
    ///      - a day with a result, or one that failed (obsolete)
    ///      A day this chain has not started is recorded as canceled.
    function handleIntexAuctionCancel(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        bytes32 receiveId,
        bytes calldata message
    ) external {
        (uint32 emissionDay, uint8 reason) = BridgeMsgCodec.decodeIntexAuctionCancel(message);

        try $.intexAuction.cancelAuction(emissionDay, reason) {}
        catch (bytes memory err) {
            bytes4 selector = _selectorOf(err);
            uint8 why;
            if (selector == IIntexAuction.AuctionCanceled.selector) {
                why = InboundReason.DUPLICATE;
            } else if (selector == IIntexAuction.StageRequired.selector) {
                why = InboundReason.OBSOLETE;
            } else {
                LowLevelCall.bubbleRevert(err);
            }
            _ignoreMessage(receiveId, srcChainId, why);
            return;
        }

        (, bool finalized,) = $.escrow.auctionStatus(emissionDay);
        if (!finalized) $.escrow.chargeIntexAuctionAsset(emissionDay, new address[](0), 0, 0, 0, 0, true);
    }

    /// @dev Whether the auction this chain holds for `emissionDay` was opened under the START's terms. The reveal
    ///      end is left out: CLEARING snaps it forward, and a START repeated after that is still the same START.
    function _sameAuction(
        TargetRouterStorage storage $,
        uint32 emissionDay,
        IIntexAuction.AuctionSchedule memory schedule,
        IIntexAuction.AuctionParams memory params
    ) private view returns (bool) {
        IIntexAuction.AuctionData memory a = $.intexAuction.getAuctionInfo(emissionDay);
        return a.schedule.commitEnd == schedule.commitEnd && a.schedule.issuanceEnd == schedule.issuanceEnd
            && keccak256(abi.encode(a.params)) == keccak256(abi.encode(params));
    }

    /// @dev Whether the result this chain holds for `emissionDay` is the one the RESULT carries.
    function _sameResult(
        TargetRouterStorage storage $,
        uint32 emissionDay,
        BridgeMsgCodec.IntexAuctionChainResult memory result
    ) private view returns (bool) {
        return $.intexAuctionResultHash[emissionDay] == keccak256(abi.encode(result));
    }

    /// @dev Whether this chain reads the day as failed. A day it never opened is not.
    function _failed(TargetRouterStorage storage $, uint32 emissionDay) private view returns (bool) {
        if (address($.intexAuction).code.length == 0) return false;
        try $.intexAuction.auctionStage(emissionDay) returns (IIntexAuction.AuctionStage stage) {
            return stage == IIntexAuction.AuctionStage.Failed;
        } catch {
            return false;
        }
    }

    /// @dev Whether the day has ended: no stage message or result can apply to it any more.
    function _ended(IIntexAuction.AuctionStage stage) private pure returns (bool) {
        return stage == IIntexAuction.AuctionStage.Completed || stage == IIntexAuction.AuctionStage.Canceled
            || stage == IIntexAuction.AuctionStage.Failed;
    }

    /// @dev `currentStage` argument of a `StageRequired(requiredStage, currentStage)` revert payload.
    function _stageRequiredCurrent(bytes memory reason) private pure returns (IIntexAuction.AuctionStage current) {
        // [len][selector(4)][requiredStage(32)][currentStage(32)]
        uint256 raw;
        assembly ("memory-safe") {
            raw := mload(add(reason, 0x44))
        }
        // forge-lint: disable-next-line(unsafe-typecast) -- the low byte is the enum value
        current = IIntexAuction.AuctionStage(uint8(raw));
    }

    function _selectorOf(bytes memory reason) private pure returns (bytes4 selector) {
        if (reason.length < 4) return bytes4(0);
        assembly ("memory-safe") {
            selector := mload(add(reason, 0x20))
        }
    }

    /// @notice Decode one ISSUANCE_INSTRUCTIONS chunk and create the series it names.
    /// @dev The handler acknowledges these cases without effect (`MessageIgnored`):
    ///      - a repeated chunk
    ///      - a chunk whose header disagrees with the day's run
    ///      - a series whose stored params differ from the chunk's
    ///      The last applied chunk emits `IssuanceCompleted`.
    function handleIntexSeries(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        bytes32 receiveId,
        bytes calldata message
    ) external {
        (uint32 emissionDay, uint16 pageIndex, uint16 totalPages, BridgeMsgCodec.IntexSeriesPayload[] memory series) =
            BridgeMsgCodec.decodeIntexSeries(message);

        // forge-lint: disable-next-line(incorrect-shift) -- the bit of chunk `pageIndex`
        uint256 bit = 1 << pageIndex;
        if ($.intexSeriesPagesApplied[emissionDay] & bit != 0) {
            _ignoreMessage(receiveId, srcChainId, InboundReason.DUPLICATE);
            return;
        }
        IntexIssuanceState memory progress = $.intexIssuanceState[emissionDay];
        uint16 knownTotal = progress.totalPages;
        if (knownTotal != 0 && knownTotal != totalPages) {
            _ignoreMessage(receiveId, srcChainId, InboundReason.CONFLICT);
            return;
        }
        // Nothing of a chunk is applied when it names one of these:
        // - a series under other terms, held on-chain or earlier in this same chunk
        // - a series with no supply
        // The chunk index then stays free for a corrected resend.
        // `known[s]` also records an in-chunk predecessor, so the second payload does not re-create the series.
        bool[] memory known = new bool[](series.length);
        for (uint256 s = 0; s < series.length; s++) {
            if (series[s].issuedUnits == 0) {
                _ignoreMessage(receiveId, srcChainId, InboundReason.INVALID);
                return;
            }
            known[s] = $.intexContract.seriesExists(series[s].seriesId);
            bool conflict = known[s] && !_sameSeries($, series[s]);
            for (uint256 j = 0; !conflict && j < s; j++) {
                if (series[j].seriesId != series[s].seriesId) continue;
                conflict = !_samePayloadParams(series[j], series[s]);
                known[s] = true;
            }
            if (conflict) {
                _ignoreMessage(receiveId, srcChainId, InboundReason.CONFLICT);
                return;
            }
        }

        uint16 seen = progress.pagesSeen + 1;
        $.intexIssuanceState[emissionDay] = IntexIssuanceState({totalPages: totalPages, pagesSeen: seen});
        $.intexSeriesPagesApplied[emissionDay] |= bit;

        for (uint256 s = 0; s < series.length; s++) {
            _applyIssuance($, srcChainId, series[s], known[s]);
        }

        if (seen == totalPages) emit ITargetRouter.IssuanceCompleted(emissionDay, totalPages);
    }

    /// @dev Create the series if this chain has not seen it.
    function _applyIssuance(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        BridgeMsgCodec.IntexSeriesPayload memory payload,
        bool seriesKnown
    ) private {
        // Create-if-absent: any of a day's chunks may be the first to name a series.
        if (!seriesKnown) {
            $.intexContract
                .createSeries(
                    IIntexNFT1155.CreateSeriesParams({
                        seriesId: payload.seriesId,
                        emissionDay: payload.emissionDay,
                        issuedAt: payload.issuedAt,
                        issuanceCurrency: payload.issuanceCurrency,
                        referenceCurrency: payload.referenceCurrency,
                        issuedUnits: payload.issuedUnits,
                        promisLoadMinor: payload.promisLoadMinor,
                        entryPriceMinor: payload.entryPriceMinor,
                        callPriceMinor: payload.callPriceMinor,
                        callTrigger: IIntexNFT1155.IntexCallTrigger({
                            callWindow: payload.callWindow,
                            callThreshold: payload.callThreshold,
                            callNoticePeriod: payload.callNoticePeriod,
                            transitMargin: payload.transitMargin
                        })
                    })
                );
            _applySlottedIntexSeriesCallNotice($, srcChainId, payload.seriesId);
        }

        emit ITargetRouter.IssuanceInstructionsReceived(srcChainId, payload.seriesId);
    }

    /// @dev Whether two payloads of one chunk name a series under the same terms.
    function _samePayloadParams(BridgeMsgCodec.IntexSeriesPayload memory a, BridgeMsgCodec.IntexSeriesPayload memory b)
        private
        pure
        returns (bool)
    {
        return a.emissionDay == b.emissionDay && a.issuanceCurrency == b.issuanceCurrency
            && a.referenceCurrency == b.referenceCurrency && a.issuedUnits == b.issuedUnits
            && a.promisLoadMinor == b.promisLoadMinor && a.entryPriceMinor == b.entryPriceMinor
            && a.callPriceMinor == b.callPriceMinor && a.callWindow == b.callWindow
            && a.callThreshold == b.callThreshold && a.callNoticePeriod == b.callNoticePeriod
            && a.transitMargin == b.transitMargin;
    }

    /// @dev Whether the series this chain holds was created under exactly the chunk's terms.
    function _sameSeries(TargetRouterStorage storage $, BridgeMsgCodec.IntexSeriesPayload memory payload)
        private
        view
        returns (bool)
    {
        IIntexNFT1155.SeriesData memory d = $.intexContract.readData(payload.seriesId);
        return d.emissionDay == payload.emissionDay && d.issuanceCurrency == payload.issuanceCurrency
            && d.referenceCurrency == payload.referenceCurrency && d.issuedUnits == payload.issuedUnits
            && d.promisLoadMinor == payload.promisLoadMinor && d.entryPriceMinor == payload.entryPriceMinor
            && d.callPriceMinor == payload.callPriceMinor && d.callTrigger.callWindow == payload.callWindow
            && d.callTrigger.callThreshold == payload.callThreshold
            && d.callTrigger.callNoticePeriod == payload.callNoticePeriod
            && d.callTrigger.transitMargin == payload.transitMargin;
    }

    function _ignoreMessage(bytes32 receiveId, uint32 srcChainId, uint8 reason) private {
        emit ITargetRouter.MessageIgnored(receiveId, srcChainId, reason);
    }

    /// @notice Decode REFUND_INSTRUCTIONS and forward finalization instructions to the EscrowAdapter.
    /// @dev The handler acknowledges without effect a page
    ///      already applied, or one arriving after the escrow closed the day or the day failed.
    ///      Bidders it would have settled recover through the auction's `returnIntexAuctionAsset` path.
    function handleIntexAuctionWinners(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        bytes32 receiveId,
        bytes calldata message
    ) external {
        (
            uint32 emissionDay,
            uint16 pageIndex,
            uint16 totalPages,
            uint64 clearingRate,
            uint128 basis,
            address[] memory winners,
            uint16 partialIndex,
            uint16 partialWon
        ) = BridgeMsgCodec.decodeIntexAuctionWinners(message);

        // forge-lint: disable-next-line(incorrect-shift) -- the bit of chunk `pageIndex`
        uint256 bit = 1 << pageIndex;
        if ($.intexAuctionWinnersPagesApplied[emissionDay] & bit != 0) {
            _ignoreMessage(receiveId, srcChainId, InboundReason.DUPLICATE);
            return;
        }
        IntexAuctionReturnState memory progress = $.intexAuctionReturnState[emissionDay];
        uint16 knownTotal = progress.totalPages;
        if (knownTotal != 0 && knownTotal != totalPages) {
            _ignoreMessage(receiveId, srcChainId, InboundReason.CONFLICT);
            return;
        }
        (, bool finalized,) = $.escrow.auctionStatus(emissionDay);
        if (finalized || _failed($, emissionDay)) {
            _ignoreMessage(receiveId, srcChainId, InboundReason.OBSOLETE);
            return;
        }

        // Counted before settling: the escrow refuses instructions once the day is closed.
        $.intexAuctionWinnersPagesApplied[emissionDay] |= bit;
        uint16 seen = progress.pagesSeen + 1;
        // `>=` rather than `==`: an overshoot would otherwise never close the day in the escrow.
        bool completesDay = seen >= totalPages;

        uint128 totalPaid = $.escrow
            .chargeIntexAuctionAsset(emissionDay, winners, partialIndex, partialWon, clearingRate, basis, completesDay);
        $.intexAuctionReturnState[emissionDay] = IntexAuctionReturnState({totalPages: totalPages, pagesSeen: seen});
        // Each chunk sends its own payments, so a chunk that never lands holds back no other.
        if (totalPaid > 0) {
            _sendOrKeepFailedIntexAuctionProceeds($, emissionDay, pageIndex, totalPaid);
        }

        emit ITargetRouter.IntexAuctionWinnersReceived(
            receiveId, emissionDay, srcChainId, pageIndex, totalPages, uint32(winners.length)
        );
    }

    /// @notice Decode a call notice and apply it to every series it carries, keeping the ones that
    ///         will not take it yet.
    /// @dev The handler acknowledges a zero call time without effect. No series can take it, and a
    ///      slot reads zero as empty.
    function handleIntexSeriesCallNotice(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        bytes32 receiveId,
        bytes calldata message
    ) external {
        (, uint64 calledAt, bytes14[] memory seriesIds) = BridgeMsgCodec.decodeIntexSeriesCallNotice(message);
        if (calledAt == 0) {
            _ignoreMessage(receiveId, srcChainId, InboundReason.INVALID);
            return;
        }
        for (uint256 i = 0; i < seriesIds.length; ++i) {
            _applyIntexSeriesCallNotice($, srcChainId, seriesIds[i], calledAt);
        }
    }

    /// @dev An unseen series keeps the mark in its slot. An already called one gets no event. Any
    ///      other failure slots it for `retryIntexSeriesCallNoticeApply`.
    function _applyIntexSeriesCallNotice(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        bytes14 seriesId,
        uint64 calledAt
    ) private {
        if (!$.intexContract.seriesExists(seriesId)) {
            _slotIntexSeriesCallNotice($, srcChainId, seriesId, calledAt, "");
            return;
        }
        try ITargetRouterShims(address(this)).applyIntexSeriesCallNotice{gas: IntexGas.CALL_NOTICE_APPLY_CAP}(
            seriesId, calledAt
        ) {}
        catch (bytes memory reason) {
            // Only a series that is already called refuses the mark by its state.
            if (_selectorOf(reason) == IIntexNFT1155.InvalidState.selector) return;
            _slotIntexSeriesCallNotice($, srcChainId, seriesId, calledAt, reason);
            return;
        }
        delete $.failedIntexSeriesCallNoticeApplies[seriesId];
    }

    function _slotIntexSeriesCallNotice(
        TargetRouterStorage storage $,
        uint32 srcChainId,
        bytes14 seriesId,
        uint64 calledAt,
        bytes memory errorData
    ) private {
        $.failedIntexSeriesCallNoticeApplies[seriesId] = calledAt;
        emit ITargetRouter.IntexSeriesCallNoticeApplyFailed(seriesId, srcChainId, errorData);
    }

    /// @dev Apply the Called mark waiting for a series that has just been created. A failure re-announces
    ///      the slot.
    function _applySlottedIntexSeriesCallNotice(TargetRouterStorage storage $, uint32 srcChainId, bytes14 seriesId)
        private
    {
        uint64 calledAt = $.failedIntexSeriesCallNoticeApplies[seriesId];
        if (calledAt == 0) return;
        delete $.failedIntexSeriesCallNoticeApplies[seriesId];
        try ITargetRouterShims(address(this)).applyIntexSeriesCallNotice{gas: IntexGas.CALL_NOTICE_APPLY_CAP}(
            seriesId, calledAt
        ) {}
        catch (bytes memory reason) {
            _slotIntexSeriesCallNotice($, srcChainId, seriesId, calledAt, reason);
        }
    }

    /// @dev Route a chunk's proceeds to Outbe. On failure, park the chunk and its amount, so a
    ///      transport/float hiccup never rolls back the finalization (the WCOEN is already held here).
    ///      Retried via `retryIntexAuctionProceedsSend`.
    function _sendOrKeepFailedIntexAuctionProceeds(
        TargetRouterStorage storage $,
        uint32 emissionDay,
        uint16 pageIndex,
        uint128 amount
    ) private {
        try ITargetRouterShims(address(this)).sendIntexAuctionProceedsExt(emissionDay, pageIndex, amount) {}
        catch (bytes memory reason) {
            uint256 idx = $.nextFailedIntexAuctionProceedsSendIdx++;
            $.failedIntexAuctionProceedsSend[idx] = FailedIntexAuctionProceedsSend({
                emissionDay: emissionDay, pageIndex: pageIndex, amount: amount, exists: true, done: false
            });
            emit ITargetRouter.IntexAuctionProceedsSendFailed(idx, emissionDay, pageIndex, amount, reason);
        }
    }
}
