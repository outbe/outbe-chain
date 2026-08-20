// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MarkBatchLib} from "../helpers/MarkBatchLib.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {ERC7786MessengerBase} from "@contracts/shared/ERC7786MessengerBase.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {InboundReason} from "@contracts/shared/libs/InboundReason.sol";

/// A lifecycle mark for a series this chain has not seen waits in one slot per series (Called overrides
/// Qualified) and is applied when ISSUANCE creates the series; a mark the series already carries, or one a
/// later mark superseded, is acknowledged without effect.
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
        // The origin-as-target shape: markCalled has no holders to migrate, so the mark is a pure state flip.
        router = DeployProxy.targetRouter(address(bridge), address(this), uint32(block.chainid));

        router.setRemoteMessenger(uint32(block.chainid), _interop(uint32(block.chainid), originSender));
        router.wire(makeAddr("auction"), address(intex), makeAddr("escrow"), makeAddr("nftBridge"));
        intex.grantRole(intex.RELAYER_ROLE(), address(router));
        series = CreateSeriesLib.seriesId(DAY);
    }

    // --- helpers ---

    function _deliver(bytes memory packet) internal {
        _deliver(uint32(block.chainid), originSender, address(router), packet);
    }

    function _qualified() internal pure returns (bytes memory) {
        return BridgeMsgCodec.encodeMarkQualified(DAY, MarkBatchLib.one(CreateSeriesLib.seriesId(DAY)));
    }

    function _called() internal pure returns (bytes memory) {
        return BridgeMsgCodec.encodeMarkCalled(DAY, MarkBatchLib.one(CreateSeriesLib.seriesId(DAY)));
    }

    function _issuance() internal returns (bytes memory) {
        BridgeMsgCodec.IssuanceInstructionsPayload[] memory one = new BridgeMsgCodec.IssuanceInstructionsPayload[](1);
        one[0].seriesId = CreateSeriesLib.seriesId(DAY);
        one[0].worldwideDay = DAY;
        one[0].issuedIntexCount = 10;
        one[0].promisLoadMinor = 1;
        one[0].issuanceCurrency = 840;
        one[0].referenceCurrency = 840;
        one[0].recipients = new address[](1);
        one[0].quantities = new uint256[](1);
        one[0].recipients[0] = makeAddr("winner");
        one[0].quantities[0] = 3;
        return BridgeMsgCodec.encodeIssuanceInstructions(DAY, 0, 1, one);
    }

    function _state() internal view returns (IIntexNFT1155.IntexState) {
        return intex.readData(series).state;
    }

    function _expectIgnored(uint8 msgType, uint8 reason) internal {
        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.InboundMessageIgnored(uint32(block.chainid), msgType, series, reason);
    }

    // --- unknown series: slot, then apply on creation ---

    function test_AMarkForAnUnknownSeriesWaitsInItsSlot() public {
        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.MarkSlotted(series, BridgeMsgCodec.MSG_MARK_QUALIFIED);
        _deliver(_qualified());
        assertEq(router.pendingMark(series), BridgeMsgCodec.MSG_MARK_QUALIFIED, "slotted");
    }

    function test_IssuanceCreatingTheSeriesAppliesTheSlottedMark() public {
        _deliver(_qualified());

        vm.expectEmit(true, true, true, true, address(router));
        emit ITargetRouter.PendingMarkApplied(series, BridgeMsgCodec.MSG_MARK_QUALIFIED);
        _deliver(_issuance());
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Qualified), "applied on creation");
        assertEq(router.pendingMark(series), 0, "slot cleared");
    }

    function test_CalledOverridesAWaitingQualifiedButNotTheReverse() public {
        _deliver(_qualified());
        _deliver(_called());
        assertEq(router.pendingMark(series), BridgeMsgCodec.MSG_MARK_CALLED, "Called wins");
        _deliver(_qualified());
        assertEq(router.pendingMark(series), BridgeMsgCodec.MSG_MARK_CALLED, "Qualified cannot demote it");
    }

    function test_ASlottedCalledWaitsForTheValveSoTheWinnersMintFirst() public {
        _deliver(_called());
        // Issuance creates the series and mints; the Called slot must NOT apply mid-issuance —
        // its holders migration would snapshot the still-empty series.
        _deliver(_issuance());
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Issued), "winners minted into a live series");
        assertEq(router.pendingMark(series), BridgeMsgCodec.MSG_MARK_CALLED, "the Called still waits");

        router.applyPendingMark(series);
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "the valve applies it after the mints");
    }

    function test_OnARemoteTargetTheValveMigratesTheMintedHolders() public {
        // Remote-target shape (OUTBE_CHAIN_ID != this chain): markCalled must bridge the holders minted
        // by the issuance that preceded the valve. The stub bridge rejects the send, so the holders
        // chunk parks — proving the migration ran against the real, non-empty holder set.
        TargetRouter remote = DeployProxy.targetRouter(address(bridge), address(this), OUTBE_CHAIN_ID);
        remote.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, originSender));
        remote.wire(makeAddr("auction"), address(intex), makeAddr("escrow"), makeAddr("nftBridge"));
        intex.grantRole(intex.RELAYER_ROLE(), address(remote));

        _deliver(OUTBE_CHAIN_ID, originSender, address(remote), _called());
        _deliver(OUTBE_CHAIN_ID, originSender, address(remote), _issuance());
        assertEq(remote.pendingMark(series), BridgeMsgCodec.MSG_MARK_CALLED, "Called waits for the valve");

        remote.applyPendingMark(series);
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "applied");
        (uint256 tokenId,,) = remote.pendingHoldersRelays(0);
        assertEq(tokenId, intex.issuedTokenId(series), "the minted holders were snapshotted for migration");
    }

    function test_ApplyPendingMarkIsThePermissionlessValve() public {
        _deliver(_called());
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0)); // the series appears by another path

        vm.prank(makeAddr("anyone"));
        router.applyPendingMark(series);
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "applied");

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.NoPendingMark.selector, series));
        router.applyPendingMark(series);
    }

    function test_ApplyPendingMarkRevertsAndKeepsTheSlotWhileTheSeriesIsMissing() public {
        _deliver(_called());
        vm.expectRevert();
        router.applyPendingMark(series);
        assertEq(router.pendingMark(series), BridgeMsgCodec.MSG_MARK_CALLED, "slot kept");
    }

    // --- already there / superseded ---

    function test_ARepeatedQualifiedIsADuplicate() public {
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0));
        _deliver(_qualified());
        _expectIgnored(BridgeMsgCodec.MSG_MARK_QUALIFIED, InboundReason.DUPLICATE);
        _deliver(_qualified());
        assertEq(router.pendingMark(series), 0, "nothing slotted");
    }

    function test_ARepeatedCalledIsADuplicate() public {
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0));
        _deliver(_called());
        _expectIgnored(BridgeMsgCodec.MSG_MARK_CALLED, InboundReason.DUPLICATE);
        _deliver(_called());
    }

    function test_AQualifiedAfterCalledIsObsolete() public {
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0));
        _deliver(_called());
        _expectIgnored(BridgeMsgCodec.MSG_MARK_QUALIFIED, InboundReason.OBSOLETE);
        _deliver(_qualified());
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "Called stands");
        assertEq(router.pendingMark(series), 0, "nothing slotted");
    }

    function test_ACalledAfterQualifiedApplies() public {
        intex.createSeries(CreateSeriesLib.params(DAY, 10, 0));
        _deliver(_qualified());
        _deliver(_called());
        assertEq(uint8(_state()), uint8(IIntexNFT1155.IntexState.Called), "the normal order still works");
    }
}
