// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {ERC7786TokenBridge} from "../src/ERC7786TokenBridge.sol";
import {USDT} from "../src/USDT.sol";
import {USDT0OFT} from "../src/USDT0OFT.sol";
import {WCOEN} from "../src/WCOEN.sol";
import {WCOENOFT} from "../src/WCOENOFT.sol";
import {MockERC7786Bridge} from "./mocks/MockERC7786Bridge.sol";

contract ERC7786TokenBridgeTest is Test {
    uint32 internal constant BNB = 56;
    uint32 internal constant OUTBE = 12_121;

    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");
    address internal intruder = makeAddr("intruder");

    MockERC7786Bridge internal bnbGateway;
    MockERC7786Bridge internal outbeGateway;

    USDT internal usdt;
    USDT0OFT internal usdt0;
    ERC7786TokenBridge internal bnbUsdtBridge;
    ERC7786TokenBridge internal outbeUsdt0Bridge;

    WCOEN internal outbeWcoen;
    WCOENOFT internal bnbWcoen;
    ERC7786TokenBridge internal outbeWcoenBridge;
    ERC7786TokenBridge internal bnbWcoenBridge;

    function setUp() public {
        bnbGateway = new MockERC7786Bridge(BNB);
        outbeGateway = new MockERC7786Bridge(OUTBE);
        bnbGateway.setRemoteBridge(OUTBE, outbeGateway);
        outbeGateway.setRemoteBridge(BNB, bnbGateway);

        _setUpUsdtRoute();
        _setUpWcoenRoute();
    }

    function test_USDT_BNBToOutbe_LockAndMint() public {
        uint256 amount = 100e6;
        usdt.mint(alice, amount);

        vm.startPrank(alice);
        usdt.approve(address(bnbUsdtBridge), amount);
        bnbUsdtBridge.send(OUTBE, bob, amount);
        vm.stopPrank();

        assertEq(usdt.balanceOf(alice), 0);
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), amount);
        assertEq(usdt0.balanceOf(bob), amount);
    }

    function test_USDT_OutbeToBNB_BurnAndUnlock() public {
        uint256 amount = 100e6;
        usdt.mint(alice, amount);

        vm.startPrank(alice);
        usdt.approve(address(bnbUsdtBridge), amount);
        bnbUsdtBridge.send(OUTBE, bob, amount);
        vm.stopPrank();

        vm.prank(bob);
        outbeUsdt0Bridge.send(BNB, alice, 40e6);

        assertEq(usdt0.balanceOf(bob), 60e6);
        assertEq(usdt.balanceOf(alice), 40e6);
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), 60e6);
    }

    function test_WCOEN_OutbeToBNB_LockAndMint() public {
        uint256 amount = 2 ether;
        vm.deal(alice, amount);

        vm.startPrank(alice);
        outbeWcoen.deposit{value: amount}();
        outbeWcoen.approve(address(outbeWcoenBridge), amount);
        outbeWcoenBridge.send(BNB, bob, amount);
        vm.stopPrank();

        assertEq(outbeWcoen.balanceOf(alice), 0);
        assertEq(outbeWcoen.balanceOf(address(outbeWcoenBridge)), amount);
        assertEq(bnbWcoen.balanceOf(bob), amount);
    }

    function test_WCOEN_BNBToOutbe_BurnAndUnlock() public {
        uint256 amount = 2 ether;
        vm.deal(alice, amount);

        vm.startPrank(alice);
        outbeWcoen.deposit{value: amount}();
        outbeWcoen.approve(address(outbeWcoenBridge), amount);
        outbeWcoenBridge.send(BNB, bob, amount);
        vm.stopPrank();

        vm.prank(bob);
        bnbWcoenBridge.send(OUTBE, alice, 1 ether);

        assertEq(bnbWcoen.balanceOf(bob), 1 ether);
        assertEq(outbeWcoen.balanceOf(alice), 1 ether);
        assertEq(outbeWcoen.balanceOf(address(outbeWcoenBridge)), 1 ether);
    }

    function test_Quote_DelegatesToBridge() public {
        outbeGateway.setFeeQuote(123);
        assertEq(outbeUsdt0Bridge.quoteSendFrom(bob, BNB, alice, 1), 123);
    }

    function test_RevertWhen_RemoteBridgeNotSet() public {
        ERC7786TokenBridge bridge = new ERC7786TokenBridge(
            address(usdt), address(bnbGateway), address(this), ERC7786TokenBridge.TokenBridgeMode.LockUnlock
        );
        usdt.mint(alice, 1);

        vm.startPrank(alice);
        usdt.approve(address(bridge), 1);
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.RemoteBridgeNotSet.selector, OUTBE));
        bridge.send(OUTBE, bob, 1);
        vm.stopPrank();
    }

    function test_RevertWhen_ReceiveFromNonBridge() public {
        vm.prank(intruder);
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.UnauthorizedBridge.selector, intruder));
        outbeUsdt0Bridge.receiveMessage(bytes32(0), _interop(BNB, address(bnbUsdtBridge)), "");
    }

    function test_RevertWhen_ReceiveFromWrongRemoteBridge() public {
        vm.prank(address(outbeGateway));
        vm.expectRevert(ERC7786TokenBridge.UnauthorizedRemoteBridge.selector);
        outbeUsdt0Bridge.receiveMessage(bytes32(0), _interop(BNB, intruder), "");
    }

    function test_RevertWhen_LockSideHasNoAllowance() public {
        usdt.mint(alice, 1);

        vm.prank(alice);
        vm.expectRevert();
        bnbUsdtBridge.send(OUTBE, bob, 1);
    }

    function _setUpUsdtRoute() internal {
        usdt = new USDT();
        usdt0 = new USDT0OFT("USDT0", "USDT0", 6, address(this));

        bnbUsdtBridge = new ERC7786TokenBridge(
            address(usdt), address(bnbGateway), address(this), ERC7786TokenBridge.TokenBridgeMode.LockUnlock
        );
        outbeUsdt0Bridge = new ERC7786TokenBridge(
            address(usdt0), address(outbeGateway), address(this), ERC7786TokenBridge.TokenBridgeMode.BurnMint
        );

        usdt0.setTokenBridge(address(outbeUsdt0Bridge));
        bnbUsdtBridge.setRemoteBridge(OUTBE, _interop(OUTBE, address(outbeUsdt0Bridge)));
        outbeUsdt0Bridge.setRemoteBridge(BNB, _interop(BNB, address(bnbUsdtBridge)));
    }

    function _setUpWcoenRoute() internal {
        outbeWcoen = new WCOEN();
        bnbWcoen = new WCOENOFT("Wrapped COEN", "WCOEN", 18, address(this));

        outbeWcoenBridge = new ERC7786TokenBridge(
            address(outbeWcoen), address(outbeGateway), address(this), ERC7786TokenBridge.TokenBridgeMode.LockUnlock
        );
        bnbWcoenBridge = new ERC7786TokenBridge(
            address(bnbWcoen), address(bnbGateway), address(this), ERC7786TokenBridge.TokenBridgeMode.BurnMint
        );

        bnbWcoen.setTokenBridge(address(bnbWcoenBridge));
        outbeWcoenBridge.setRemoteBridge(BNB, _interop(BNB, address(bnbWcoenBridge)));
        bnbWcoenBridge.setRemoteBridge(OUTBE, _interop(OUTBE, address(outbeWcoenBridge)));
    }

    function _interop(uint256 chainId, address addr) internal pure returns (bytes memory) {
        return InteroperableAddress.formatEvmV1(chainId, addr);
    }
}
