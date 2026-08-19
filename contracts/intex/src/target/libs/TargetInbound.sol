// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexAuction} from "../interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "../../shared/interfaces/IIntexNFT1155.sol";
import {IEscrowAdapter} from "../interfaces/IEscrowAdapter.sol";
import {ITargetRouter} from "../interfaces/ITargetRouter.sol";
import {BridgeMsgCodec} from "../../shared/libs/BridgeMsgCodec.sol";
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
    function handleAuctionStageStart(TargetRouterStorage storage $, uint32 srcChainId, bytes calldata message)
        external
    {
        (
            uint32 worldwideDay,
            IIntexAuction.WorldwideDayState dayState,
            IIntexAuction.AuctionSchedule memory schedule,
            IIntexAuction.AuctionParams memory params
        ) = BridgeMsgCodec.decodeAuctionParams(message);
        $.auction.auctionStart(worldwideDay, dayState, schedule, params);

        emit ITargetRouter.AuctionStageReceived(srcChainId, worldwideDay, BridgeMsgCodec.MSG_AUCTION_STAGE_START);
    }

    /// @notice Decode AUCTION_STAGE_CLEARING, forward to Auction, then relay revealed bids to Outbe.
    /// @dev Only the outbound relay is caught (parked on failure); a failing inbound transition propagates so the
    ///      bridge redelivers.
    function handleAuctionStageClearing(TargetRouterStorage storage $, uint32 srcChainId, bytes calldata message)
        external
    {
        uint32 worldwideDay = BridgeMsgCodec.decodeAuctionStageClearing(message);
        $.auction.startClearingStage(worldwideDay); // idempotent; a failing transition propagates for redelivery

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
    function handleAuctionResult(TargetRouterStorage storage $, uint32 srcChainId, bytes calldata message) external {
        (uint32 worldwideDay, uint32 issuedIntexCount, uint64 auctionClearingRate, uint32 wonBidsCount) =
            BridgeMsgCodec.decodeAuctionResult(message);

        $.auction.executeAuctionClearing(worldwideDay, issuedIntexCount, auctionClearingRate, wonBidsCount);

        emit ITargetRouter.AuctionResultReceived(srcChainId, worldwideDay, issuedIntexCount, auctionClearingRate);
    }

    /// @notice Decode ISSUANCE_INSTRUCTIONS, create the series, and mint tokens via IntexNFT1155.
    function handleIssuanceInstructions(TargetRouterStorage storage $, uint32 srcChainId, bytes calldata message)
        external
    {
        BridgeMsgCodec.IssuanceInstructionsPayload[] memory series = BridgeMsgCodec.decodeIssuanceInstructions(message);

        for (uint256 s = 0; s < series.length; s++) {
            BridgeMsgCodec.IssuanceInstructionsPayload memory payload = series[s];

            // Create-if-absent: any of a day's messages may be the first to name a series.
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
