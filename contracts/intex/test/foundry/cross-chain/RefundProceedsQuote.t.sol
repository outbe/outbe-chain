// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";

/// @dev A chunk is quoted for what it may do. Any chunk that can be the one sending the day's
///      proceeds home carries the routing leg. A chunk with no winners settles nothing.
contract RefundProceedsQuoteTest is Test {
    function test_AChunkWithNoWinnersNeitherSettlesNorRoutes() public pure {
        assertEq(IntexGas.assetReturn(0, false), IntexGas.RETURN_BASE);
    }

    /// @dev The quote grows with what the chunk does, in the order the work appears.
    function test_TheQuoteGrowsWithTheWorkTheChunkDoes() public pure {
        assertLt(
            IntexGas.assetReturn(0, false), IntexGas.assetReturn(1, false), "settling winners costs more than closing"
        );
        assertLt(IntexGas.assetReturn(1, false), IntexGas.assetReturn(2, false), "each winner adds");
        assertLt(IntexGas.assetReturn(2, false), IntexGas.assetReturn(2, true), "routing the proceeds adds");
    }
}
