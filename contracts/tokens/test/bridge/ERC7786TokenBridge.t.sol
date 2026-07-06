// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";

import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";
import {USDT} from "../../src/native/USDT.sol";
import {USDT0} from "../../src/synthetic/USDT0.sol";
import {WCOEN as NativeWCOEN} from "../../src/native/WCOEN.sol";
import {WCOEN as SyntheticWCOEN} from "../../src/synthetic/WCOEN.sol";
import {MockERC7786Bridge} from "../mocks/MockERC7786Bridge.sol";

contract ERC7786TokenBridgeTest is Test {
    uint32 internal constant BNB = 97;
    uint32 internal constant OUTBE = 54322345;

    address internal sourceAddr = makeAddr("sourceAddr");
    address internal targetAddr = makeAddr("targetAddr");
    address internal intruder = makeAddr("intruder");

    MockERC7786Bridge internal bnbGateway;
    MockERC7786Bridge internal outbeGateway;

    USDT internal usdt;
    USDT0 internal usdt0;
    ERC7786TokenBridge internal bnbUsdtBridge;
    ERC7786TokenBridge internal outbeUsdt0Bridge;

    NativeWCOEN internal outbeWcoen;
    SyntheticWCOEN internal bnbWcoen;
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
        usdt.mint(sourceAddr, amount);

        vm.startPrank(sourceAddr);
        usdt.approve(address(bnbUsdtBridge), amount);
        bnbUsdtBridge.send(OUTBE, targetAddr, amount);
        vm.stopPrank();

        assertEq(usdt.balanceOf(sourceAddr), 0);
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), amount);
        assertEq(usdt0.balanceOf(targetAddr), amount);
    }

    function test_USDT_OutbeToBNB_BurnAndUnlock() public {
        uint256 amount = 100e6;
        usdt.mint(sourceAddr, amount);

        vm.startPrank(sourceAddr);
        usdt.approve(address(bnbUsdtBridge), amount);
        bnbUsdtBridge.send(OUTBE, targetAddr, amount);
        vm.stopPrank();

        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), 100e6);

        vm.prank(targetAddr);
        outbeUsdt0Bridge.send(BNB, sourceAddr, 40e6);

        assertEq(usdt0.balanceOf(targetAddr), 60e6);
        assertEq(usdt.balanceOf(sourceAddr), 40e6);
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), 60e6);
    }

    function test_WCOEN_OutbeToBNB_LockAndMint() public {
        uint256 amount = 2 ether;
        vm.deal(sourceAddr, amount);

        vm.startPrank(sourceAddr);
        outbeWcoen.deposit{value: amount}();
        outbeWcoen.approve(address(outbeWcoenBridge), amount);
        outbeWcoenBridge.send(BNB, targetAddr, amount);
        vm.stopPrank();

        assertEq(outbeWcoen.balanceOf(sourceAddr), 0);
        assertEq(outbeWcoen.balanceOf(address(outbeWcoenBridge)), amount);
        assertEq(bnbWcoen.balanceOf(targetAddr), amount);
    }

    function test_WCOEN_BNBToOutbe_BurnAndUnlock() public {
        uint256 amount = 2 ether;
        vm.deal(sourceAddr, amount);

        vm.startPrank(sourceAddr);
        outbeWcoen.deposit{value: amount}();
        outbeWcoen.approve(address(outbeWcoenBridge), amount);
        outbeWcoenBridge.send(BNB, targetAddr, amount);
        vm.stopPrank();

        vm.prank(targetAddr);
        bnbWcoenBridge.send(OUTBE, sourceAddr, 1 ether);

        assertEq(bnbWcoen.balanceOf(targetAddr), 1 ether);
        assertEq(outbeWcoen.balanceOf(sourceAddr), 1 ether);
        assertEq(outbeWcoen.balanceOf(address(outbeWcoenBridge)), 1 ether);
    }

    function test_Quote_DelegatesToBridge() public {
        outbeGateway.setFeeQuote(123);
        vm.prank(targetAddr);
        assertEq(outbeUsdt0Bridge.quoteSend(BNB, sourceAddr, 1, "", 0), 123);
    }

    function test_RevertWhen_RemoteBridgeNotSet() public {
        ERC7786TokenBridge bridge = new ERC7786TokenBridge(
            address(usdt), address(bnbGateway), address(this), ERC7786TokenBridge.TokenBridgeMode.LockUnlock
        );
        usdt.mint(sourceAddr, 1);

        vm.startPrank(sourceAddr);
        usdt.approve(address(bridge), 1);
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.RemoteBridgeNotSet.selector, OUTBE));
        bridge.send(OUTBE, targetAddr, 1);
        vm.stopPrank();
    }

    function test_RevertWhen_SendToZeroRecipient() public {
        usdt.mint(sourceAddr, 1);

        vm.startPrank(sourceAddr);
        usdt.approve(address(bnbUsdtBridge), 1);
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.InvalidRecipient.selector, address(0)));
        bnbUsdtBridge.send(OUTBE, address(0), 1);
        vm.stopPrank();

        assertEq(usdt.balanceOf(sourceAddr), 1);
        assertEq(usdt.balanceOf(address(bnbUsdtBridge)), 0);
    }

    function test_RevertWhen_ReceiveFromNonBridge() public {
        vm.prank(intruder);
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.UnauthorizedBridge.selector, intruder));
        outbeUsdt0Bridge.receiveMessage(bytes32(0), _interop(BNB, address(bnbUsdtBridge)), "");
    }

    function test_RevertWhen_ReceiveFromWrongRemoteBridge() public {
        bytes memory wrongSender = _interop(BNB, intruder);

        vm.prank(address(outbeGateway));
        vm.expectRevert(abi.encodeWithSelector(ERC7786TokenBridge.UnauthorizedRemoteBridge.selector, wrongSender));
        outbeUsdt0Bridge.receiveMessage(bytes32(0), wrongSender, "");
    }

    function test_RevertWhen_LockSideHasNoAllowance() public {
        usdt.mint(sourceAddr, 1);

        vm.prank(sourceAddr);
        vm.expectRevert();
        bnbUsdtBridge.send(OUTBE, targetAddr, 1);
    }

    function _setUpUsdtRoute() internal {
        usdt = new USDT();
        usdt0 = new USDT0("USDT0", "USDT0", 6, address(this));

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
        outbeWcoen = new NativeWCOEN();
        bnbWcoen = new SyntheticWCOEN("Wrapped COEN", "WCOEN", 18, address(this));

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
