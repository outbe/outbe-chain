// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

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

/// A Called mark for a series this chain has not seen waits in one slot per series and is applied when
/// ISSUANCE creates the series; a mark the series already carries is acknowledged without effect.
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

        router.setRemoteMessenger(uint32(block.chainid), _interop(uint32(block.chainid), originSender));
        router.wire(makeAddr("auction"), address(intex), makeAddr("escrow"));
        intex.grantRole(intex.RELAYER_ROLE(), address(router));
        series = CreateSeriesLib.seriesId(DAY);
    }

    // --- helpers ---

    function _deliver(bytes memory packet) internal {
        _deliver(uint32(block.chainid), originSender, address(router), packet);
    }

    function _called() internal view returns (bytes memory) {
        return
            BridgeMsgCodec.encodeMarkCalled(
                DAY, uint32(block.timestamp), MarkBatchLib.one(CreateSeriesLib.seriesId(DAY))
            );
    }

    function _issuance() internal returns (bytes memory) {
        BridgeMsgCodec.IssuanceInstructionsPayload memory payload;
        payload.seriesId = CreateSeriesLib.seriesId(DAY);
        payload.worldwideDay = DAY;
        payload.issuedAt = uint32(block.timestamp);
        payload.issuedUnits = 10;
        payload.promisLoadMinor = 1;
        payload.issuanceCurrency = 840;
        payload.referenceCurrency = 840;
        payload.recipients = new address[](1);
        payload.quantities = new uint256[](1);
        payload.recipients[0] = makeAddr("winner");
        payload.quantities[0] = 3;
        return BridgeMsgCodec.encodeIssuanceInstructions(DAY, 0, 1, IssuanceBatchLib.one(payload));
    }

    function _state() internal view returns (IIntexNFT1155.IntexState) {
        return intex.readData(series).state;
    }

    function _expectIgnored(uint8 msgType, uint8 reason) internal {
        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.InboundMessageIgnored(uint32(block.chainid), msgType, series, reason);
    }

    function _expectZeroCallTimeIgnored() internal {
        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.InboundMessageIgnored(
            uint32(block.chainid), BridgeMsgCodec.MSG_MARK_CALLED, bytes32(uint256(DAY)), InboundReason.INVALID
        );
    }

    // --- unknown series: slot, then apply on creation ---

    function test_AMarkForAnUnknownSeriesWaitsInItsSlot() public {
        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.MarkParked(series);
        _deliver(_called());
        assertEq(router.parkedMark(series), uint32(block.timestamp), "slotted with its call time");
    }

    /// @dev A mark moves no balances, so a waiting Called lands with the issuance that creates the series
    ///      rather than holding for the valve.
    function test_IssuanceCreatingTheSeriesAppliesAWaitingCalled() public {
        _deliver(_called());

        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.ParkedMarkApplied(series);
        _deliver(_issuance());
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "applied with the issuance");
        assertEq(router.parkedMark(series), 0, "nothing waits any more");
    }

    function test_ARedeliveredMarkThatSettlesTheSlotClearsIt() public {
        _deliver(_called());
        _deliver(_issuance()); // series created, and the waiting Called lands with it
        // The bridge redelivers the mark: it applies directly now, so nothing may keep waiting.
        _deliver(_called());
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "applied on redelivery");
        assertEq(router.parkedMark(series), 0, "the settled slot is cleared");

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.NoParkedMark.selector, series));
        router.applyParkedMark(series);
    }

    function test_ApplyParkedMarkIsThePermissionlessValve() public {
        _deliver(_called());
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0)); // the series appears by another path

        vm.prank(makeAddr("anyone"));
        router.applyParkedMark(series);
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "applied");

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.NoParkedMark.selector, series));
        router.applyParkedMark(series);
    }

    function test_ApplyParkedMarkRevertsAndKeepsTheSlotWhileTheSeriesIsMissing() public {
        _deliver(_called());
        vm.expectRevert();
        router.applyParkedMark(series);
        assertEq(router.parkedMark(series), uint32(block.timestamp), "slot kept");
    }

    // --- a zero call time ---

    function test_AZeroCallTimeForAnUnknownSeriesIsInvalidAndNotSlotted() public {
        _expectZeroCallTimeIgnored();
        _deliver(BridgeMsgCodec.encodeMarkCalled(DAY, 0, MarkBatchLib.one(series)));
        assertEq(router.parkedMark(series), 0, "nothing waits");

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.NoParkedMark.selector, series));
        router.applyParkedMark(series);
    }

    function test_AZeroCallTimeForAnExistingSeriesIsInvalidAndLeavesItIssued() public {
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0));
        _expectZeroCallTimeIgnored();
        _deliver(BridgeMsgCodec.encodeMarkCalled(DAY, 0, MarkBatchLib.one(series)));
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Issued), "the series stays Issued");
        assertEq(router.parkedMark(series), 0, "nothing waits");
    }

    // --- already there ---

    function test_ARepeatedCalledIsADuplicate() public {
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0));
        _deliver(_called());
        _expectIgnored(BridgeMsgCodec.MSG_MARK_CALLED, InboundReason.DUPLICATE);
        _deliver(_called());
    }
}
