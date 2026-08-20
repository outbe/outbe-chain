// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {MarkBatchLib} from "../helpers/MarkBatchLib.sol";
import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";

/// @dev A lifecycle mark the series will not take yet is parked instead of rejecting the inbound
///      message, and a permissionless flush applies it once the series is ready.
contract TargetRouterMarkParkTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;

    uint32 internal constant WORLDWIDE_DAY = 20250101;
    bytes14 internal constant SERIES_ID = "20250101-USD-U";

    TargetRouter internal bnbRouter;
    OriginRouter internal outbeRouter;
    IntexAuction internal auction;
    IntexNFT1155 internal intex;
    EscrowAdapter internal escrow;
    IntexNFT1155Bridge internal nftBridge;

    address internal admin = address(this);

    function setUp() public {
        _setUpBridge();

        intex = DeployProxy.intexNFT1155(admin, admin);
        auction = DeployProxy.intexAuction(admin, admin);
        bnbRouter = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        outbeRouter = DeployProxy.originRouter(address(bridge), admin);
        nftBridge = DeployProxy.intexNFT1155Bridge(address(intex), address(bridge), admin);
        escrow = DeployProxy.escrowAdapter(admin, admin);

        bnbRouter.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(outbeRouter)));
        bnbRouter.wire(address(auction), address(intex), address(escrow));
        intex.grantRole(intex.RELAYER_ROLE(), address(bnbRouter));
    }

    function _deliver(bytes memory packet) internal {
        _deliver(OUTBE_CHAIN_ID, address(outbeRouter), address(bnbRouter), packet);
    }

    function _createSeries() internal {
        intex.createSeries(CreateSeriesLib.params(WORLDWIDE_DAY, 10_000, 0));
    }

    function test_qualifiedMarkForAnUnknownSeriesIsParked() public {
        _deliver(BridgeMsgCodec.encodeMarkQualified(WORLDWIDE_DAY, MarkBatchLib.one(SERIES_ID)));

        assertEq(bnbRouter.nextPendingMarkIdx(), 1, "one mark parked");
        (bytes14 seriesId, uint8 msgType, bool exists, bool done) = bnbRouter.pendingMarks(0);
        assertEq(seriesId, SERIES_ID, "parked for its series");
        assertEq(msgType, BridgeMsgCodec.MSG_MARK_QUALIFIED, "parked as a qualified mark");
        assertTrue(exists, "slot exists");
        assertFalse(done, "slot still open");
    }

    function test_flushAppliesTheParkedQualifiedMark() public {
        _deliver(BridgeMsgCodec.encodeMarkQualified(WORLDWIDE_DAY, MarkBatchLib.one(SERIES_ID)));
        _createSeries();

        bnbRouter.flushPendingMark(0);

        IIntexNFT1155.SeriesData memory data = intex.readData(SERIES_ID);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Qualified), "series flipped to Qualified");
        (,,, bool done) = bnbRouter.pendingMarks(0);
        assertTrue(done, "slot closed");
    }

    function test_aMarkTheSeriesTakesIsNotParked() public {
        _createSeries();

        _deliver(BridgeMsgCodec.encodeMarkQualified(WORLDWIDE_DAY, MarkBatchLib.one(SERIES_ID)));

        assertEq(bnbRouter.nextPendingMarkIdx(), 0, "nothing parked");
        IIntexNFT1155.SeriesData memory data = intex.readData(SERIES_ID);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Qualified), "series flipped to Qualified");
    }

    /// @notice A series already past the mark's state (Called takes Issued or Qualified, never twice)
    ///         parks rather than rejecting the message.
    function test_aMarkTheSeriesRefusesIsParked() public {
        _createSeries();
        _deliver(BridgeMsgCodec.encodeMarkCalled(WORLDWIDE_DAY, MarkBatchLib.one(SERIES_ID)));

        _deliver(BridgeMsgCodec.encodeMarkCalled(WORLDWIDE_DAY, MarkBatchLib.one(SERIES_ID)));

        assertEq(bnbRouter.nextPendingMarkIdx(), 1, "the repeat was parked");
        (bytes14 seriesId, uint8 msgType,,) = bnbRouter.pendingMarks(0);
        assertEq(seriesId, SERIES_ID, "parked for its series");
        assertEq(msgType, BridgeMsgCodec.MSG_MARK_CALLED, "parked as a called mark");
    }

    function test_flushingATwiceFlushedSlotIsRefused() public {
        _deliver(BridgeMsgCodec.encodeMarkQualified(WORLDWIDE_DAY, MarkBatchLib.one(SERIES_ID)));
        _createSeries();
        bnbRouter.flushPendingMark(0);

        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.AlreadyFlushed.selector, uint256(0)));
        bnbRouter.flushPendingMark(0);
    }

    function test_flushingAnUnknownSlotIsRefused() public {
        vm.expectRevert(abi.encodeWithSelector(ITargetRouter.NoSuchPendingMark.selector, uint256(7)));
        bnbRouter.flushPendingMark(7);
    }

    function test_theShimIsOnlyCallableByTheRouterItself() public {
        vm.expectRevert(ITargetRouter.NotSelf.selector);
        bnbRouter.applyMarkOne(SERIES_ID, BridgeMsgCodec.MSG_MARK_QUALIFIED);
    }
}
