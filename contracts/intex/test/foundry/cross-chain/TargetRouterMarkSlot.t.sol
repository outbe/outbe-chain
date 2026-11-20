// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Vm} from "forge-std/Vm.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MarkBatchLib} from "../helpers/MarkBatchLib.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";
import {IssuanceBatchLib} from "../helpers/IssuanceBatch.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {ERC7786MessengerBase} from "@contracts/shared/ERC7786MessengerBase.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {InboundReason} from "@contracts/shared/libs/InboundReason.sol";

/// A Called mark for a series that this chain has not seen waits in one slot per series. The router
/// applies it when ISSUANCE creates the series. The router acknowledges a mark that the series
/// already carries, without effect.
contract TargetRouterMarkSlotTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant DAY = 20_250_101;

    TargetRouter internal router;
    IntexNFT1155 internal intex;
    address internal originSender = makeAddr("originSender");
    bytes14 internal series;

    function setUp() public {
        _setUpBridge();
        intex = DeployProxy.intexNFT1155(address(this), address(this));
        // The origin-as-target shape: a mark is a pure state flip here as anywhere.
        router = DeployProxy.targetRouter(address(bridge), address(this), uint32(block.chainid));

        router.registerRemoteMessenger(uint32(block.chainid), _interop(uint32(block.chainid), originSender));
        router.setDependencies(makeAddr("auction"), address(intex), makeAddr("escrow"));
        intex.grantRole(intex.ROUTER_ROLE(), address(router));
        series = CreateSeriesLib.seriesId(DAY);
    }

    // --- helpers ---

    function _deliver(bytes memory packet) internal {
        _deliver(uint32(block.chainid), originSender, address(router), packet);
    }

    function _called() internal view returns (bytes memory) {
        return BridgeMsgCodec.encodeIntexSeriesCallNotice(
            DAY, uint32(block.timestamp), MarkBatchLib.one(CreateSeriesLib.seriesId(DAY))
        );
    }

    function _issuance() internal returns (bytes memory) {
        BridgeMsgCodec.IntexSeriesPayload memory payload;
        payload.seriesId = CreateSeriesLib.seriesId(DAY);
        payload.emissionDay = DAY;
        payload.issuedAt = uint32(block.timestamp);
        payload.issuedUnits = 10;
        payload.promisLoadMinor = 1;
        payload.issuanceCurrency = 840;
        payload.referenceCurrency = 840;
        return BridgeMsgCodec.encodeIntexSeries(DAY, 0, 1, IssuanceBatchLib.one(payload));
    }

    function _state() internal view returns (IIntexNFT1155.IntexState) {
        return intex.readData(series).state;
    }

    function _expectIgnored(uint8 msgType, uint8 reason) internal {
        vm.expectEmit(false, true, false, true, address(router));
        emit ITargetRouter.MessageIgnored(bytes32(0), uint32(block.chainid), reason);
    }

    function _expectZeroCallTimeIgnored() internal {
        vm.expectEmit(false, true, false, true, address(router));
        emit ITargetRouter.MessageIgnored(bytes32(0), uint32(block.chainid), InboundReason.INVALID);
    }

    // --- unknown series: slot, then apply on creation ---

    function test_AMarkForAnUnknownSeriesWaitsInItsSlot() public {
        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.IntexSeriesCallNoticeApplyFailed(series, uint32(block.chainid), "");
        _deliver(_called());
        assertEq(router.failedIntexSeriesCallNoticeApply(series), uint32(block.timestamp), "slotted with its call time");
    }

    function test_ACallNoticeTheTokenRefusesWaitsWithTheRefusal() public {
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0));
        intex.revokeRole(intex.ROUTER_ROLE(), address(router));

        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.IntexSeriesCallNoticeApplyFailed(
            series,
            uint32(block.chainid),
            abi.encodeWithSelector(
                IAccessControl.AccessControlUnauthorizedAccount.selector, address(router), intex.ROUTER_ROLE()
            )
        );
        _deliver(_called());
        assertEq(router.failedIntexSeriesCallNoticeApply(series), uint32(block.timestamp), "the notice waits");
    }

    /// @dev A mark moves no balances, so a waiting Called lands with the issuance that creates the series
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
            assertTrue(logs[i].topics[0] != ITargetRouter.IntexSeriesCallNoticeApplyFailed.selector, "no failure event");
            if (logs[i].emitter == address(intex) && logs[i].topics[0] == IIntexNFT1155.IntexSeriesCalled.selector) {
                ++called;
            }
        }
        assertEq(called, 1, "the token reports the call once");
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

    function test_ARedeliveredMarkThatSettlesTheSlotClearsIt() public {
        _deliver(_called());
        _deliver(_issuance()); // series created, and the waiting Called lands with it
        // The bridge redelivers the mark: it applies directly now, so nothing may keep waiting.
        _deliver(_called());
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "applied on redelivery");
        assertEq(router.failedIntexSeriesCallNoticeApply(series), 0, "the settled slot is cleared");

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.NoFailedIntexSeriesCallNoticeApply.selector, series));
        router.retryIntexSeriesCallNoticeApply(series);
    }

    function test_ApplyParkedMarkIsThePermissionlessValve() public {
        _deliver(_called());
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0)); // the series appears by another path

        vm.prank(makeAddr("anyone"));
        router.retryIntexSeriesCallNoticeApply(series);
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "applied");

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.NoFailedIntexSeriesCallNoticeApply.selector, series));
        router.retryIntexSeriesCallNoticeApply(series);
    }

    function test_ApplyParkedMarkRevertsAndKeepsTheSlotWhileTheSeriesIsMissing() public {
        _deliver(_called());
        vm.expectRevert();
        router.retryIntexSeriesCallNoticeApply(series);
        assertEq(router.failedIntexSeriesCallNoticeApply(series), uint32(block.timestamp), "slot kept");
    }

    // --- a zero call time ---

    function test_AZeroCallTimeForAnUnknownSeriesIsInvalidAndNotSlotted() public {
        _expectZeroCallTimeIgnored();
        _deliver(BridgeMsgCodec.encodeIntexSeriesCallNotice(DAY, 0, MarkBatchLib.one(series)));
        assertEq(router.failedIntexSeriesCallNoticeApply(series), 0, "nothing waits");

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.NoFailedIntexSeriesCallNoticeApply.selector, series));
        router.retryIntexSeriesCallNoticeApply(series);
    }

    function test_AZeroCallTimeForAnExistingSeriesIsInvalidAndLeavesItIssued() public {
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0));
        _expectZeroCallTimeIgnored();
        _deliver(BridgeMsgCodec.encodeIntexSeriesCallNotice(DAY, 0, MarkBatchLib.one(series)));
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Issued), "the series stays Issued");
        assertEq(router.failedIntexSeriesCallNoticeApply(series), 0, "nothing waits");
    }

    // --- already there ---

    function test_ARepeatedCalledIsADuplicate() public {
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0));
        _deliver(_called());
        // A series that is already called gets no event.
        vm.recordLogs();
        _deliver(_called());
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            assertTrue(logs[i].topics[0] != ITargetRouter.MessageIgnored.selector, "no ignore event");
        }
    }
}
