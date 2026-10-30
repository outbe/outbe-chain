// SPDX-License-Identifier: MIT

pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {ERC7786Bridge} from "src/ERC7786Bridge.sol";
import {HyperlaneGatewayAdapter} from "src/adapters/HyperlaneGatewayAdapter.sol";
import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";
import {LoopbackGatewayAdapter} from "src/adapters/LoopbackGatewayAdapter.sol";
import {HandOver} from "script/4_HandOver.s.sol";
import {HYPERLANE_CONTROLLER} from "@precompiles/IHyperlaneController.sol";
import {EndpointV2Mock} from "./mocks/MockLayerZeroEndpoint.sol";
import {MockHyperlaneMailbox} from "./mocks/MockHyperlaneMailbox.sol";

/// @dev A test has no Outbe node, so the harness supplies the controller's Interchain Account.
contract HandOverHarness is HandOver {
    address internal constant CONTROLLER_ICA = address(uint160(0x1CA));

    function _controllerAccount(uint32) internal pure override returns (address) {
        return CONTROLLER_ICA;
    }
}

contract HandOverTest is Test {
    address internal constant GOVERNANCE = address(uint160(0x60F));

    HandOverHarness internal script;

    function setUp() public {
        script = new HandOverHarness();
    }

    function test_HandOver_GivesEveryRoleToGovernance() public {
        // The script is the deployer: it owns every contract until the hand-over.
        EndpointV2Mock endpoint = new EndpointV2Mock(1);
        LayerZeroGatewayAdapter lz = new LayerZeroGatewayAdapter(address(endpoint), address(script));
        HyperlaneGatewayAdapter hl = new HyperlaneGatewayAdapter(address(new MockHyperlaneMailbox(1)), address(script));
        ERC7786Bridge bridge = new ERC7786Bridge(address(script), address(hl));
        LoopbackGatewayAdapter lb = new LoopbackGatewayAdapter(address(bridge), address(script));

        script.handOver(GOVERNANCE, address(bridge), address(lz), address(hl), address(lb));

        assertEq(bridge.owner(), GOVERNANCE, "bridge");
        assertEq(lz.owner(), GOVERNANCE, "LayerZero adapter");
        assertEq(endpoint.delegate(), GOVERNANCE, "LayerZero delegate");
        assertEq(hl.owner(), GOVERNANCE, "Hyperlane adapter");
        assertEq(lb.owner(), GOVERNANCE, "loopback adapter");
    }

    function test_GovernanceOwner_IsTheControllerOnOutbeAndItsAccountElsewhere() public {
        bool keepDeployer = vm.envOr("KEEP_DEPLOYER_AS_OWNER", false);
        address deployer = keepDeployer ? vm.addr(vm.envUint("DEPLOYER_PK")) : address(0);

        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(block.chainid));
        assertEq(script.governanceOwner(), keepDeployer ? deployer : HYPERLANE_CONTROLLER, "Outbe");

        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(block.chainid + 1));
        assertEq(script.governanceOwner(), keepDeployer ? deployer : address(uint160(0x1CA)), "other chain");
    }
}
