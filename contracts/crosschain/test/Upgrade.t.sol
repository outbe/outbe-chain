// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {Initializable} from "@openzeppelin/contracts-upgradeable/proxy/utils/Initializable.sol";
import {OwnableUpgradeable} from "@openzeppelin/contracts-upgradeable/access/OwnableUpgradeable.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {ERC7786Bridge} from "src/ERC7786Bridge.sol";
import {HyperlaneGatewayAdapter} from "src/adapters/HyperlaneGatewayAdapter.sol";
import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";
import {LoopbackGatewayAdapter} from "src/adapters/LoopbackGatewayAdapter.sol";
import {EndpointV2Mock} from "./mocks/MockLayerZeroEndpoint.sol";
import {MockHyperlaneMailbox} from "./mocks/MockHyperlaneMailbox.sol";
import {Proxies} from "./utils/Proxies.sol";

/// @dev Each contract keeps its owner and state across an upgrade, and only the owner can upgrade.
contract UpgradeTest is Test {
    uint256 internal constant REMOTE_CHAIN_ID = 97;
    uint32 internal constant REMOTE_DOMAIN = 97;

    address internal owner = makeAddr("owner");
    address internal stranger = makeAddr("stranger");
    address internal gateway = makeAddr("gateway");
    address internal remoteBridge = makeAddr("remoteBridge");
    bytes32 internal remotePeer = bytes32(uint256(uint160(makeAddr("remotePeer"))));

    function test_Bridge_KeepsStateAcrossUpgrade() public {
        ERC7786Bridge bridge = Proxies.bridge(owner, gateway);
        bytes memory remote = InteroperableAddress.formatEvmV1(REMOTE_CHAIN_ID, remoteBridge);
        vm.startPrank(owner);
        bridge.registerRemoteBridge(remote);
        bridge.setGateway(REMOTE_CHAIN_ID, stranger);
        bridge.pause();
        vm.stopPrank();

        _upgrade(address(bridge), address(new ERC7786Bridge()));

        assertEq(bridge.owner(), owner, "owner");
        assertEq(bridge.getGateway(), gateway, "default gateway");
        assertEq(bridge.getGateway(REMOTE_CHAIN_ID), stranger, "chain gateway");
        assertEq(bridge.getRemoteBridge(remote), remote, "remote bridge");
        assertTrue(bridge.paused(), "paused");
    }

    function test_Hyperlane_KeepsStateAcrossUpgrade() public {
        address mailbox = address(new MockHyperlaneMailbox(1));
        HyperlaneGatewayAdapter adapter = Proxies.hyperlane(mailbox, owner);
        vm.startPrank(owner);
        adapter.setRouterWithChain(REMOTE_DOMAIN, remotePeer, REMOTE_CHAIN_ID);
        adapter.setDefaultGasLimit(300_000);
        vm.stopPrank();

        _upgrade(address(adapter), address(new HyperlaneGatewayAdapter(mailbox)));

        assertEq(adapter.owner(), owner, "owner");
        assertEq(address(adapter.MAILBOX()), mailbox, "mailbox");
        assertEq(adapter.routers(REMOTE_DOMAIN), remotePeer, "router");
        assertEq(adapter.chainIdToDomain(REMOTE_CHAIN_ID), REMOTE_DOMAIN, "domain");
        assertEq(adapter.domainToChainId(REMOTE_DOMAIN), REMOTE_CHAIN_ID, "chain id");
        assertEq(adapter.defaultGasLimit(), 300_000, "gas limit");
    }

    function test_LayerZero_KeepsStateAcrossUpgrade() public {
        EndpointV2Mock endpoint = new EndpointV2Mock(1);
        LayerZeroGatewayAdapter adapter = Proxies.layerZero(address(endpoint), owner);
        vm.startPrank(owner);
        adapter.setPeerWithChain(REMOTE_DOMAIN, remotePeer, REMOTE_CHAIN_ID);
        adapter.setDefaultGasLimit(300_000);
        vm.stopPrank();

        _upgrade(address(adapter), address(new LayerZeroGatewayAdapter(address(endpoint))));

        assertEq(adapter.owner(), owner, "owner");
        assertEq(endpoint.delegate(), owner, "delegate");
        assertEq(adapter.peers(REMOTE_DOMAIN), remotePeer, "peer");
        assertEq(adapter.chainIdToEid(REMOTE_CHAIN_ID), REMOTE_DOMAIN, "eid");
        assertEq(adapter.eidToChainId(REMOTE_DOMAIN), REMOTE_CHAIN_ID, "chain id");
        assertEq(adapter.defaultGasLimit(), 300_000, "gas limit");
    }

    function test_Loopback_KeepsStateAcrossUpgrade() public {
        LoopbackGatewayAdapter adapter = Proxies.loopback(remoteBridge, owner);
        vm.prank(owner);
        adapter.setDefaultGasLimit(300_000);

        _upgrade(address(adapter), address(new LoopbackGatewayAdapter(remoteBridge)));

        assertEq(adapter.owner(), owner, "owner");
        assertEq(adapter.HUB(), remoteBridge, "hub");
        assertEq(adapter.defaultGasLimit(), 300_000, "gas limit");
    }

    function test_RevertWhen_NotOwnerUpgrades() public {
        address[4] memory proxies = _deployAll();
        address implementation = address(new ERC7786Bridge());
        for (uint256 i = 0; i < proxies.length; i++) {
            vm.prank(stranger);
            vm.expectRevert(abi.encodeWithSelector(OwnableUpgradeable.OwnableUnauthorizedAccount.selector, stranger));
            UUPSUpgradeable(proxies[i]).upgradeToAndCall(implementation, "");
        }
    }

    function test_RevertWhen_InitializedAgain() public {
        address[4] memory proxies = _deployAll();
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        ERC7786Bridge(proxies[0]).initialize(stranger, gateway);
        for (uint256 i = 1; i < proxies.length; i++) {
            vm.expectRevert(Initializable.InvalidInitialization.selector);
            HyperlaneGatewayAdapter(proxies[i]).initialize(stranger);
        }
    }

    function test_RevertWhen_ImplementationInitialized() public {
        ERC7786Bridge implementation = new ERC7786Bridge();
        vm.expectRevert(Initializable.InvalidInitialization.selector);
        implementation.initialize(stranger, gateway);
    }

    function _deployAll() internal returns (address[4] memory) {
        return [
            address(Proxies.bridge(owner, gateway)),
            address(Proxies.hyperlane(address(new MockHyperlaneMailbox(1)), owner)),
            address(Proxies.layerZero(address(new EndpointV2Mock(1)), owner)),
            address(Proxies.loopback(remoteBridge, owner))
        ];
    }

    function _upgrade(address proxy, address implementation) internal {
        vm.prank(owner);
        UUPSUpgradeable(proxy).upgradeToAndCall(implementation, "");
        assertEq(address(uint160(uint256(vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT)))), implementation, "impl");
    }
}
