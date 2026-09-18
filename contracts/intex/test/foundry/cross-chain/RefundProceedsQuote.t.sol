// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ReferenceCurrencyPriceLib} from "../helpers/ReferenceCurrencyPriceLib.sol";
import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";
import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";

/// @dev A chunk is quoted for what it may do: any chunk that can be the one sending the day's proceeds home
///      carries the routing leg, and a chunk with no winners settles nothing.
contract RefundProceedsQuoteTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant DAY = 42;
    bytes4 internal constant GAS_SELECTOR = bytes4(keccak256("executionGasLimit(uint256)"));

    OriginRouter internal outbe;
    address internal admin = address(this);
    address internal desis;

    function setUp() public {
        _setUpBridge();
        outbe = DeployProxy.originRouter(address(bridge), admin);
        outbe.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, makeAddr("targetRouter")));
        outbe.addTarget(BNB_CHAIN_ID);
        vm.deal(address(outbe), 10 ether);

        desis = address(new MockDesis());
        outbe.wire(desis, makeAddr("factory"));

        // Freeze the day's snapshot so an addressed refund send passes membership.
        IOriginRouter.AuctionStageStartParams memory sp;
        sp.prices = ReferenceCurrencyPriceLib.one(840, 1, 2, 3);
        sp.worldwideDay = DAY;
        sp.dayState = 1;
        vm.prank(desis);
        outbe.sendAuctionStageStart(sp);
    }

    function _send(uint16 chunkIndex, uint16 totalChunks, uint256 winners) internal {
        vm.prank(desis);
        outbe.sendRefundInstructions(
            BNB_CHAIN_ID, DAY, chunkIndex, totalChunks, 600_000, 1e6, new address[](winners), 0, 0
        );
    }

    function _assertQuoted(uint256 expectedGas) internal view {
        bytes[] memory attrs = bridge.getLastAttributes();
        assertEq(attrs.length, 1, "expected one attribute");
        assertEq(attrs[0], abi.encodeWithSelector(GAS_SELECTOR, expectedGas), "executionGasLimit mismatch");
    }

    function test_TheOnlyChunkOfADayCarriesTheRoutingLeg() public {
        _send(0, 1, 5);
        _assertQuoted(IntexGas.refund(5, true));
    }

    /// @dev The target routes on the chunk that completes the day, which is the last to land, not the last by
    ///      index: chunk 0 arriving after chunk 1 is the one that sends the proceeds.
    function test_EveryChunkOfARunCarriesIt() public {
        _send(0, 3, 64);
        _assertQuoted(IntexGas.refund(64, true));
        _send(2, 3, 8);
        _assertQuoted(IntexGas.refund(8, true));
    }

    /// @dev A chain whose bids all lost gets one chunk that closes its day and sends nothing home.
    function test_AChunkWithNoWinnersNeitherSettlesNorRoutes() public {
        _send(0, 1, 0);
        _assertQuoted(IntexGas.REFUND_BASE);
    }

    /// @dev The quote grows with what the chunk does, in the order the work appears.
    function test_TheQuoteGrowsWithTheWorkTheChunkDoes() public pure {
        assertLt(IntexGas.refund(0, false), IntexGas.refund(1, false), "settling winners costs more than closing");
        assertLt(IntexGas.refund(1, false), IntexGas.refund(2, false), "each winner adds");
        assertLt(IntexGas.refund(2, false), IntexGas.refund(2, true), "routing the proceeds adds");
    }
}
