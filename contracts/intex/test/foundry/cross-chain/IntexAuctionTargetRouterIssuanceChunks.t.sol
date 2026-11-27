// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IIntexAuctionTargetRouter} from "@contracts/target/interfaces/IIntexAuctionTargetRouter.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {InboundReason} from "@contracts/shared/libs/InboundReason.sol";
import {IssuanceBatchLib} from "../helpers/IssuanceBatch.sol";
import {Vm} from "forge-std/Vm.sol";

/// A day's series reach a chain as numbered chunks. The router acknowledges these without effect:
/// - a repeated chunk
/// - a chunk that disagrees with the day's run
/// - a series named under other terms
/// The last chunk completes the day.
contract IntexAuctionTargetRouterIssuanceChunksTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant DAY = 20_250_101;
    bytes14 internal constant USD = "20250101-USD-U";
    bytes14 internal constant EUR = "20250101-EUR-E";

    IntexAuctionTargetRouter internal router;
    IntexNFT1155 internal intex;
    address internal outbeIca = makeAddr("outbeIca");

    function setUp() public {
        _setUpBridge();
        intex = DeployProxy.intexNFT1155(address(this), address(this));
        router = DeployProxy.targetRouter(address(bridge), address(this), OUTBE_CHAIN_ID);

        _wireOutbeIca(router, outbeIca);
        router.setDependencies(makeAddr("auction"), address(intex), makeAddr("escrow"));
        intex.grantRole(intex.ROUTER_ROLE(), address(router));
    }

    // --- helpers ---

    function _series(bytes14 seriesId) internal view returns (BridgeMsgCodec.IntexSeriesPayload memory payload) {
        payload.seriesId = seriesId;
        payload.emissionDay = DAY;
        payload.issuedAt = uint32(block.timestamp);
        payload.totalUnits = 100;
        payload.promisLoadMinor = 1_000;
        payload.entryPriceMinor = 100e6;
        payload.issuanceCurrency = 840;
        payload.referenceCurrency = 840;
        payload.callWindow = 30;
        payload.callThreshold = 5;
        payload.callPriceMinor = 200e6;
    }

    function _deliver(uint16 pageIndex, uint16 totalPages, BridgeMsgCodec.IntexSeriesPayload[] memory series) internal {
        _deliver(
            OUTBE_CHAIN_ID,
            outbeIca,
            address(router),
            BridgeMsgCodec.encodeIntexSeries(DAY, pageIndex, totalPages, series)
        );
    }

    // --- repeats ---

    function test_ARepeatedChunkIsAcknowledgedAndNotCounted() public {
        _deliver(0, 1, IssuanceBatchLib.one(_series(USD)));
        assertTrue(intex.seriesExists(USD), "first delivery creates");

        vm.expectEmit(true, true, false, true, address(router));
        emit IIntexAuctionTargetRouter.IntexAuctionMessageIgnored(
            BridgeMsgCodec.MSG_INTEX_SERIES, DAY, InboundReason.DUPLICATE
        );
        _deliver(0, 1, IssuanceBatchLib.one(_series(USD)));
        (uint16 seen, uint16 total) = router.getIntexIssuanceState(DAY);
        assertEq(seen, 1, "repeat is not counted");
        assertEq(total, 1, "total unchanged");
    }

    // --- conflicts ---

    function test_AChunkDisagreeingOnTheTotalIsSkipped() public {
        _deliver(0, 2, IssuanceBatchLib.one(_series(USD)));

        vm.expectEmit(true, true, false, true, address(router));
        emit IIntexAuctionTargetRouter.IntexAuctionMessageIgnored(
            BridgeMsgCodec.MSG_INTEX_SERIES, DAY, InboundReason.CONFLICT
        );
        _deliver(1, 3, IssuanceBatchLib.one(_series(EUR)));
        assertFalse(intex.seriesExists(EUR), "nothing of the conflicting chunk applied");
        (uint16 seen,) = router.getIntexIssuanceState(DAY);
        assertEq(seen, 1, "conflicting chunk not counted");
    }

    function test_ASeriesNamedUnderOtherTermsSkipsTheWholeChunk() public {
        _deliver(0, 2, IssuanceBatchLib.one(_series(USD)));

        BridgeMsgCodec.IntexSeriesPayload[] memory chunk = new BridgeMsgCodec.IntexSeriesPayload[](2);
        chunk[0] = _series(EUR);
        chunk[1] = _series(USD);
        chunk[1].entryPriceMinor = 101e6; // disagrees with the series this chain already holds

        vm.expectEmit(true, true, false, true, address(router));
        emit IIntexAuctionTargetRouter.IntexAuctionMessageIgnored(
            BridgeMsgCodec.MSG_INTEX_SERIES, DAY, InboundReason.CONFLICT
        );
        _deliver(1, 2, chunk);
        assertFalse(intex.seriesExists(EUR), "the healthy series of the chunk is not created either");
        assertFalse(router.intexSeriesPageApplied(DAY, 1), "chunk stays unapplied so a corrected resend can land");
    }

    function test_ACorrectedResendOfASkippedChunkLands() public {
        _deliver(0, 2, IssuanceBatchLib.one(_series(USD)));
        BridgeMsgCodec.IntexSeriesPayload[] memory chunk = new BridgeMsgCodec.IntexSeriesPayload[](2);
        chunk[0] = _series(EUR);
        chunk[1] = _series(USD);
        chunk[1].entryPriceMinor = 101e6;
        _deliver(1, 2, chunk);
        assertFalse(intex.seriesExists(EUR), "conflict skipped");

        chunk[1].entryPriceMinor = _series(USD).entryPriceMinor;
        _deliver(1, 2, chunk);
        assertTrue(intex.seriesExists(EUR), "corrected chunk applied");
    }

    function test_ASeriesWithNoSupplyIsInvalidNotALoop() public {
        BridgeMsgCodec.IntexSeriesPayload memory p = _series(USD);
        p.totalUnits = 0;
        vm.expectEmit(true, true, false, true, address(router));
        emit IIntexAuctionTargetRouter.IntexAuctionMessageIgnored(
            BridgeMsgCodec.MSG_INTEX_SERIES, DAY, InboundReason.INVALID
        );
        _deliver(0, 1, IssuanceBatchLib.one(p));
        assertFalse(intex.seriesExists(USD), "nothing created");
        assertFalse(router.intexSeriesPageApplied(DAY, 0), "a corrected resend can still land");
    }

    function test_ADuplicateSeriesWithOtherTermsInsideOneChunkIsAConflict() public {
        BridgeMsgCodec.IntexSeriesPayload[] memory chunk = new BridgeMsgCodec.IntexSeriesPayload[](2);
        chunk[0] = _series(USD);
        chunk[1] = _series(USD);
        chunk[1].entryPriceMinor = 101e6;

        vm.expectEmit(true, true, false, true, address(router));
        emit IIntexAuctionTargetRouter.IntexAuctionMessageIgnored(
            BridgeMsgCodec.MSG_INTEX_SERIES, DAY, InboundReason.CONFLICT
        );
        _deliver(0, 1, chunk);
        assertFalse(intex.seriesExists(USD), "nothing of the chunk applied");
    }

    function test_ASeriesNamedTwiceInsideOneChunkIsCreatedOnce() public {
        BridgeMsgCodec.IntexSeriesPayload[] memory chunk = new BridgeMsgCodec.IntexSeriesPayload[](2);
        chunk[0] = _series(USD);
        chunk[1] = _series(USD);
        _deliver(0, 1, chunk);
        assertTrue(intex.seriesExists(USD));
        assertTrue(router.intexSeriesPageApplied(DAY, 0), "chunk applied");
    }

    // --- completeness ---

    function test_TheLastChunkCompletesTheDayWhateverTheOrder() public {
        vm.recordLogs();
        _deliver(2, 3, IssuanceBatchLib.one(_series(EUR)));
        _deliver(0, 3, IssuanceBatchLib.one(_series(USD)));
        assertEq(_countTopic(IIntexAuctionTargetRouter.IntexSeriesAllReceived.selector), 0, "not complete yet");

        vm.expectEmit(true, true, true, true, address(router));
        emit IIntexAuctionTargetRouter.IntexSeriesAllReceived(DAY, OUTBE_CHAIN_ID, 3);
        _deliver(1, 3, IssuanceBatchLib.one(_series(USD)));
        (uint16 seen, uint16 total) = router.getIntexIssuanceState(DAY);
        assertEq(seen, 3, "every chunk counted");
        assertEq(total, 3, "total as declared");
    }

    function test_intex_series_pages_have_one_sent_and_one_all_received_event() public {
        BridgeMsgCodec.IntexSeriesPayload[] memory two = new BridgeMsgCodec.IntexSeriesPayload[](2);
        two[0] = _series(USD);
        two[1] = _series(EUR);
        vm.recordLogs();
        _deliver(0, 3, two);
        _deliver(1, 3, IssuanceBatchLib.one(_series(USD)));
        _deliver(2, 3, IssuanceBatchLib.one(_series(EUR)));

        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 allReceived;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter != address(router)) continue;
            assertEq(
                logs[i].topics[0],
                IIntexAuctionTargetRouter.IntexSeriesAllReceived.selector,
                "no router event per series"
            );
            allReceived++;
            assertEq(uint256(logs[i].topics[1]), DAY);
            assertEq(uint256(logs[i].topics[2]), OUTBE_CHAIN_ID);
            assertEq(abi.decode(logs[i].data, (uint32)), 3);
        }
        assertEq(allReceived, 1, "one event for the day");
    }

    function test_ASingleChunkCompletesTheDay() public {
        vm.expectEmit(true, true, true, true, address(router));
        emit IIntexAuctionTargetRouter.IntexSeriesAllReceived(DAY, OUTBE_CHAIN_ID, 1);
        _deliver(0, 1, IssuanceBatchLib.one(_series(USD)));
        assertTrue(intex.seriesExists(USD), "series created");
    }
}
