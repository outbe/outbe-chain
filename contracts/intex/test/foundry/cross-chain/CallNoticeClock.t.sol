// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CallNoticeBatchLib} from "../helpers/CallNoticeBatchLib.sol";
import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {OpenSeriesLib} from "../helpers/OpenSeriesLib.sol";

/// @dev INTEX_SERIES_CALL_NOTICE carries the origin's stamp, so a slow delivery still yields the deadline
///      settlement honours rather than a later one.
contract CallNoticeClockTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant EMISSION_DAY = 20260501;
    bytes14 internal constant SERIES_ID = "20260501-USD-U";

    IntexAuctionTargetRouter internal router;
    IntexNFT1155 internal intex;
    address internal admin = address(this);
    address internal outbeIca = address(0x0B1);

    function setUp() public {
        _setUpBridge();
        intex = DeployProxy.intexNFT1155(admin, admin);
        router = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        _wireOutbeIca(router, outbeIca);
        router.setDependencies(makeAddr("auction"), address(intex), makeAddr("escrow"));
        intex.grantRole(intex.ROUTER_ROLE(), address(router));
        // A real notice period: with zero the window closes the instant the series
        // is called, and the reads below would report the series forfeited.
        intex.openSeries(OpenSeriesLib.params(EMISSION_DAY, 10_000, 21 days));
    }

    function _deliverCalled(uint32 calledAt) internal {
        _deliver(
            OUTBE_CHAIN_ID,
            outbeIca,
            address(router),
            BridgeMsgCodec.encodeIntexSeriesCallNotice(EMISSION_DAY, calledAt, CallNoticeBatchLib.one(SERIES_ID))
        );
    }

    function test_LateDeliveryKeepsTheOriginDeadline() public {
        uint32 originCalledAt = uint32(block.timestamp);
        skip(6 hours);

        _deliverCalled(originCalledAt);

        IIntexNFT1155.SeriesData memory data = intex.getIntexSeries(SERIES_ID);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Called), "series Called");
        assertEq(data.calledAt, originCalledAt, "stamp is the origin's, not the delivery time");
    }

    function test_AStampFromTheFutureIsRefused() public {
        uint32 ahead = uint32(block.timestamp) + 1;

        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.CalledAtInvalid.selector, ahead, uint32(block.timestamp)));
        vm.prank(address(router));
        intex.callSeries(SERIES_ID, ahead);
    }

    /// @dev Zero is what an uncalled series reads, so taking it would set a deadline in 1970 and brick
    ///      every bridge and settle on the series.
    function test_AZeroStampIsRefused() public {
        vm.expectRevert(
            abi.encodeWithSelector(IIntexNFT1155.CalledAtInvalid.selector, uint32(0), uint32(block.timestamp))
        );
        vm.prank(address(router));
        intex.callSeries(SERIES_ID, 0);
    }
}
