// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";

import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";
import {USDT} from "../../src/canonical/USDT.sol";
import {MockERC7786Bridge} from "../mocks/MockERC7786Bridge.sol";
import {TokenBridgeProxy} from "../utils/TokenBridgeProxy.sol";

/// @dev The bridge keeps its owner, peers and locked tokens across an upgrade, and only the owner
///      can upgrade.
contract ERC7786TokenBridgeUpgradeTest is Test {
    uint32 internal constant PEER_DOMAIN = 97;
    uint32 internal constant NEW_DOMAIN = 11_155_111;
    ERC7786TokenBridge.TokenBridgeMode internal constant LOCK = ERC7786TokenBridge.TokenBridgeMode.LockUnlock;

    address internal owner = makeAddr("owner");
    address internal stranger = makeAddr("stranger");
    address internal peer = makeAddr("peer");
    address internal newPeer = makeAddr("newPeer");

    USDT internal usdt;
    MockERC7786Bridge internal hub;
    ERC7786TokenBridge internal tokenBridge;

    function setUp() public {
        usdt = new USDT();
        hub = new MockERC7786Bridge(block.chainid);
        ERC7786TokenBridge.Peer[] memory peers = new ERC7786TokenBridge.Peer[](1);
        peers[0] = ERC7786TokenBridge.Peer(PEER_DOMAIN, peer);
        tokenBridge = TokenBridgeProxy.deploy(address(usdt), address(hub), LOCK, owner, peers);
    }

    function test_KeepsStateAcrossUpgrade() public {
        vm.prank(owner);
        tokenBridge.addPeer(NEW_DOMAIN, newPeer);
        usdt.mint(address(tokenBridge), 100e6);

        address implementation = address(new ERC7786TokenBridge(address(usdt), address(hub), LOCK));
        vm.prank(owner);
        tokenBridge.upgradeToAndCall(implementation, "");

        assertEq(
            address(uint160(uint256(vm.load(address(tokenBridge), ERC1967Utils.IMPLEMENTATION_SLOT)))),
            implementation,
            "impl"
        );
        assertEq(tokenBridge.owner(), owner, "owner");
        assertEq(tokenBridge.remoteBridges(PEER_DOMAIN), peer, "peer");
        assertEq(tokenBridge.remoteBridges(NEW_DOMAIN), newPeer, "new peer");
        assertEq(address(tokenBridge.token()), address(usdt), "token");
        assertEq(address(tokenBridge.bridge()), address(hub), "hub");
        assertEq(uint8(tokenBridge.mode()), uint8(LOCK), "mode");
        assertEq(usdt.balanceOf(address(tokenBridge)), 100e6, "locked tokens");
    }

    function test_RevertWhen_NotOwnerUpgrades() public {
        address implementation = address(new ERC7786TokenBridge(address(usdt), address(hub), LOCK));
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, stranger));
        tokenBridge.upgradeToAndCall(implementation, "");
    }

    function test_RevertWhen_InitializedAgain() public {
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        tokenBridge.initialize(stranger, new ERC7786TokenBridge.Peer[](0));
    }

    function test_RevertWhen_ImplementationInitialized() public {
        ERC7786TokenBridge implementation = new ERC7786TokenBridge(address(usdt), address(hub), LOCK);
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        implementation.initialize(stranger, new ERC7786TokenBridge.Peer[](0));
    }
}
