// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {IERC7802} from "@openzeppelin/contracts/interfaces/draft-IERC7802.sol";

import {ConfigurableERC7802} from "../src/ConfigurableERC7802.sol";
import {ERC7786TokenBridge} from "../src/ERC7786TokenBridge.sol";
import {USDT0OFT} from "../src/USDT0OFT.sol";
import {WCOENOFT} from "../src/WCOENOFT.sol";
import {MockERC7786Bridge} from "./mocks/MockERC7786Bridge.sol";

contract USDT0OFTTest is Test {
    MockERC7786Bridge internal gateway;
    ERC7786TokenBridge internal tokenBridge;
    USDT0OFT internal token;

    function setUp() public {
        gateway = new MockERC7786Bridge(block.chainid);
        token = new USDT0OFT("USDT0", "USDT0", 6, address(this));
        tokenBridge = new ERC7786TokenBridge(
            address(token), address(gateway), address(this), ERC7786TokenBridge.TokenBridgeMode.BurnMint
        );
    }

    function test_Decimals_IsSix() public view {
        assertEq(token.decimals(), 6);
    }

    function test_MetadataAndDecimals_AreConstructorConfigured() public {
        WCOENOFT wcoen = new WCOENOFT("Wrapped COEN", "WCOEN", 18, address(this));

        assertEq(wcoen.name(), "Wrapped COEN");
        assertEq(wcoen.symbol(), "WCOEN");
        assertEq(wcoen.decimals(), 18);
    }

    function test_SupportsERC7802() public view {
        assertTrue(token.supportsInterface(type(IERC7802).interfaceId));
    }

    function test_SetTokenBridge_IsOneTime() public {
        token.setTokenBridge(address(tokenBridge));
        ERC7786TokenBridge nextBridge = new ERC7786TokenBridge(
            address(token), address(gateway), address(this), ERC7786TokenBridge.TokenBridgeMode.BurnMint
        );

        vm.expectRevert(abi.encodeWithSelector(ConfigurableERC7802.TokenBridgeAlreadySet.selector, address(tokenBridge)));
        token.setTokenBridge(address(nextBridge));
    }

    function test_CrosschainMintAndBurn_OnlyTokenBridge() public {
        address alice = makeAddr("alice");

        vm.expectRevert(abi.encodeWithSelector(ConfigurableERC7802.UnauthorizedTokenBridge.selector, address(this)));
        token.crosschainMint(alice, 100);

        token.setTokenBridge(address(tokenBridge));

        vm.prank(address(tokenBridge));
        token.crosschainMint(alice, 100);
        assertEq(token.balanceOf(alice), 100);

        vm.prank(address(tokenBridge));
        token.crosschainBurn(alice, 40);
        assertEq(token.balanceOf(alice), 60);
    }
}
