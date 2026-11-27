// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Vm} from "forge-std/Vm.sol";
import {CrossChainTest} from "../utils/CrossChainTest.sol";
import {DeployProxy} from "../utils/DeployProxy.sol";
import {CallNoticeBatchLib} from "../utils/CallNoticeBatchLib.sol";
import {OpenSeriesLib} from "../utils/OpenSeriesLib.sol";
import {IssuanceBatchLib} from "../utils/IssuanceBatch.sol";

import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IIntexAuctionTargetRouter} from "@contracts/target/interfaces/IIntexAuctionTargetRouter.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {InboundReason} from "@contracts/shared/libs/InboundReason.sol";

/// A call notice for a series that this chain has not seen waits in one slot per series. The router
/// applies it when ISSUANCE creates the series. The router acknowledges a call notice that the series
/// already carries, without effect.
contract IntexAuctionTargetRouterCallNoticeSlotTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant DAY = 20_250_101;

    IntexAuctionTargetRouter internal router;
    IntexNFT1155 internal intex;
    address internal outbeIca = makeAddr("outbeIca");
    bytes14 internal series;

    function setUp() public {
        _setUpBridge();
        address routerProxy = DeployProxy.reserve();
        intex = DeployProxy.intexNFT1155(
            address(this), DeployProxy.NftLinks(routerProxy, address(this), address(this), address(this), address(this))
        );
        // The origin-as-target shape: a call notice is a pure state flip here as anywhere.
        router = DeployProxy.targetRouterAt(routerProxy, address(bridge), address(this), _links());
        _wireOutbeIca(router, outbeIca);
        series = OpenSeriesLib.seriesId(DAY);
    }

    // --- helpers ---

    function _links() internal returns (DeployProxy.TargetRouterLinks memory) {
        return DeployProxy.TargetRouterLinks(
            uint32(block.chainid),
            DeployProxy.WCOEN_BRIDGE,
            makeAddr("auction"),
            address(intex),
            makeAddr("escrow"),
            outbeIca
        );
    }

    function _deliver(bytes memory packet) internal {
        _deliver(uint32(block.chainid), outbeIca, address(router), packet);
    }

    function _called() internal view returns (bytes memory) {
        return BridgeMsgCodec.encodeIntexSeriesCallNotice(
            DAY, uint32(block.timestamp), CallNoticeBatchLib.one(OpenSeriesLib.seriesId(DAY))
        );
    }

    function _issuance() internal returns (bytes memory) {
        BridgeMsgCodec.IntexSeriesPayload memory payload;
        payload.seriesId = OpenSeriesLib.seriesId(DAY);
        payload.emissionDay = DAY;
        payload.issuedAt = uint32(block.timestamp);
        payload.issuedUnits = 10;
        payload.promisLoadMinor = 1;
        payload.issuanceCurrency = 840;
        payload.referenceCurrency = 840;
        return BridgeMsgCodec.encodeIntexSeries(DAY, 0, 1, IssuanceBatchLib.one(payload));
    }

    function _state() internal view returns (IIntexNFT1155.IntexState) {
        return intex.getIntexSeries(series).state;
    }

    function _expectIgnored(uint8 msgType, uint8 reason) internal {
        vm.expectEmit(true, true, false, true, address(router));
        emit IIntexAuctionTargetRouter.IntexAuctionMessageIgnored(msgType, DAY, reason);
    }

    function _expectZeroCallTimeIgnored() internal {
        vm.expectEmit(true, true, false, true, address(router));
        emit IIntexAuctionTargetRouter.IntexAuctionMessageIgnored(
            BridgeMsgCodec.MSG_INTEX_SERIES_CALL_NOTICE, DAY, InboundReason.INVALID
        );
    }

    // --- unknown series: slot, then apply on creation ---

    function test_ACallNoticeForAnUnknownSeriesWaitsInItsSlot() public {
        vm.expectEmit(true, true, true, true, address(router));
        emit IIntexAuctionTargetRouter.IntexSeriesCallNoticeApplyFailed(series, uint32(block.chainid), "");
        _deliver(_called());
        assertEq(router.failedIntexSeriesCallNoticeApply(series), uint32(block.timestamp), "slotted with its call time");
    }

    /// @dev The token takes calls only from `router`, so it refuses every call of `refused`.
    function test_ACallNoticeTheTokenRefusesWaitsWithTheRefusal() public {
        vm.prank(address(router));
        intex.openSeries(OpenSeriesLib.params(DAY, 10, 0));
        IntexAuctionTargetRouter refused = DeployProxy.targetRouter(address(bridge), address(this), _links());
        _wireOutbeIca(refused, outbeIca);

        vm.expectEmit(true, true, true, true, address(refused));
        emit IIntexAuctionTargetRouter.IntexSeriesCallNoticeApplyFailed(
            series,
            uint32(block.chainid),
            abi.encodeWithSelector(IIntexNFT1155.UnauthorizedCaller.selector, address(refused))
        );
        _deliver(uint32(block.chainid), outbeIca, address(refused), _called());
        assertEq(refused.failedIntexSeriesCallNoticeApply(series), uint32(block.timestamp), "the notice waits");
    }

    /// @dev A call notice moves no balances, so a waiting Called lands with the issuance that creates the series
    ///      rather than holding for the valve.
    function test_IssuanceCreatingTheSeriesAppliesAWaitingCalled() public {
        _deliver(_called());

        vm.recordLogs();
        _deliver(_issuance());
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "applied with the issuance");
        assertEq(router.failedIntexSeriesCallNoticeApply(series), 0, "nothing waits any more");
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 called;
        for (uint256 i; i < logs.length; ++i) {
            assertTrue(
                logs[i].topics[0] != IIntexAuctionTargetRouter.IntexSeriesCallNoticeApplyFailed.selector,
                "no failure event"
            );
            if (logs[i].emitter == address(intex) && logs[i].topics[0] == IIntexNFT1155.IntexSeriesCalled.selector) {
                ++called;
            }
        }
        assertEq(called, 1, "the token reports the call once");
    }

    function _routerTopics() internal returns (bytes32[] memory topics) {
        Vm.Log[] memory logs = vm.getRecordedLogs();
        topics = new bytes32[](logs.length);
        uint256 n;
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].emitter == address(router)) topics[n++] = logs[i].topics[0];
        }
        assembly ("memory-safe") {
            mstore(topics, n)
        }
    }

    function test_notice_router_emits_only_the_apply_failure() public {
        vm.recordLogs();
        _deliver(_called());
        bytes32[] memory waiting = _routerTopics();
        assertEq(waiting.length, 1, "a waiting notice gives one router event");
        assertEq(waiting[0], IIntexAuctionTargetRouter.IntexSeriesCallNoticeApplyFailed.selector);

        vm.recordLogs();
        _deliver(_issuance());
        bytes32[] memory applied = _routerTopics();
        assertEq(applied.length, 1, "the later apply gives no router event of the notice");
        assertEq(applied[0], IIntexAuctionTargetRouter.IntexSeriesAllReceived.selector);

        vm.warp(block.timestamp + 1);
        bytes14 other = OpenSeriesLib.seriesId(DAY + 1);
        vm.prank(address(router));
        intex.openSeries(OpenSeriesLib.params(DAY + 1, 10, 0));
        vm.recordLogs();
        _deliver(
            BridgeMsgCodec.encodeIntexSeriesCallNotice(DAY + 1, uint32(block.timestamp), CallNoticeBatchLib.one(other))
        );
        assertEq(_routerTopics().length, 0, "a notice the token takes gives no router event");
        assertEq(uint8(intex.getIntexSeries(other).state), uint8(IIntexNFT1155.IntexState.Called));
    }

    /// @dev A series that lands after its deadline is still created, so the chain holds its terms.
    function test_IssuanceAfterTheDeadlineCreatesAForfeitedSeries() public {
        bytes memory called = _called();
        bytes memory issuance = _issuance();
        _deliver(called);
        vm.warp(block.timestamp + 1); // past a zero notice period

        _deliver(issuance);
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Forfeited), "created past its deadline");
    }

    function test_ARedeliveredCallNoticeThatSettlesTheSlotClearsIt() public {
        _deliver(_called());
        _deliver(_issuance()); // series created, and the waiting Called lands with it
        // The bridge redelivers the call notice: it applies directly now, so nothing may keep waiting.
        _deliver(_called());
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "applied on redelivery");
        assertEq(router.failedIntexSeriesCallNoticeApply(series), 0, "the settled slot is cleared");

        vm.expectRevert(
            abi.encodeWithSelector(IIntexAuctionTargetRouter.NoFailedIntexSeriesCallNoticeApply.selector, series)
        );
        router.retryIntexSeriesCallNoticeApply(series);
    }

    function test_RetryIntexSeriesCallNoticeApplyIsPermissionless() public {
        _deliver(_called());
        vm.prank(address(router));
        intex.openSeries(OpenSeriesLib.params(DAY, 10, 0)); // the series appears by another path

        vm.prank(makeAddr("anyone"));
        router.retryIntexSeriesCallNoticeApply(series);
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "applied");

        vm.expectRevert(
            abi.encodeWithSelector(IIntexAuctionTargetRouter.NoFailedIntexSeriesCallNoticeApply.selector, series)
        );
        router.retryIntexSeriesCallNoticeApply(series);
    }

    function test_RetryIntexSeriesCallNoticeApplyRevertsAndKeepsTheSlotWhileTheSeriesIsMissing() public {
        _deliver(_called());
        vm.expectRevert();
        router.retryIntexSeriesCallNoticeApply(series);
        assertEq(router.failedIntexSeriesCallNoticeApply(series), uint32(block.timestamp), "slot kept");
    }

    // --- a zero call time ---

    function test_AZeroCallTimeForAnUnknownSeriesIsInvalidAndNotSlotted() public {
        _expectZeroCallTimeIgnored();
        _deliver(BridgeMsgCodec.encodeIntexSeriesCallNotice(DAY, 0, CallNoticeBatchLib.one(series)));
        assertEq(router.failedIntexSeriesCallNoticeApply(series), 0, "nothing waits");

        vm.expectRevert(
            abi.encodeWithSelector(IIntexAuctionTargetRouter.NoFailedIntexSeriesCallNoticeApply.selector, series)
        );
        router.retryIntexSeriesCallNoticeApply(series);
    }

    function test_AZeroCallTimeForAnExistingSeriesIsInvalidAndLeavesItIssued() public {
        vm.prank(address(router));
        intex.openSeries(OpenSeriesLib.params(DAY, 10, 0));
        _expectZeroCallTimeIgnored();
        _deliver(BridgeMsgCodec.encodeIntexSeriesCallNotice(DAY, 0, CallNoticeBatchLib.one(series)));
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Issued), "the series stays Issued");
        assertEq(router.failedIntexSeriesCallNoticeApply(series), 0, "nothing waits");
    }

    // --- already there ---

    function test_ARepeatedCalledIsADuplicate() public {
        vm.prank(address(router));
        intex.openSeries(OpenSeriesLib.params(DAY, 10, 0));
        _deliver(_called());
        // A series that is already called gets no event.
        vm.recordLogs();
        _deliver(_called());
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            assertTrue(
                logs[i].topics[0] != IIntexAuctionTargetRouter.IntexAuctionMessageIgnored.selector, "no ignore event"
            );
            assertTrue(
                logs[i].topics[0] != IIntexAuctionTargetRouter.IntexSeriesCallNoticeApplyFailed.selector,
                "no failure event"
            );
        }
    }
}
