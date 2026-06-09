// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {USDT0OFT} from "../src/USDT0OFT.sol";

/// @dev Minimal LayerZero endpoint stub used by OApp/OFT constructors.
contract MockLZEndpoint {
    function setDelegate(address) external {}
}

contract USDT0OFTTest is Test {
    function test_Decimals_IsSix() public {
        MockLZEndpoint endpoint = new MockLZEndpoint();
        USDT0OFT token = new USDT0OFT("USDT0", "USDT0", 6, address(endpoint), address(this));

        assertEq(token.decimals(), 6);
    }

    function test_MetadataAndDecimals_AreConstructorConfigured() public {
        MockLZEndpoint endpoint = new MockLZEndpoint();
        USDT0OFT token = new USDT0OFT("Wrapped COEN", "WCOEN", 18, address(endpoint), address(this));

        assertEq(token.name(), "Wrapped COEN");
        assertEq(token.symbol(), "WCOEN");
        assertEq(token.decimals(), 18);
    }
}
