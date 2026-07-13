// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";

import {USDT0Deploy} from "../../script/usdt0/USDT0Deploy.s.sol";

contract USDT0DeployHarness is USDT0Deploy {
    function exposedRequireMockUSDTDeploymentAllowed() external view {
        _requireMockUSDTDeploymentAllowed();
    }
}

contract USDT0DeployTest is Test {
    USDT0DeployHarness internal deployScript;

    function setUp() public {
        deployScript = new USDT0DeployHarness();
    }

    function test_MockUSDTDeploymentGuard_AllowsBscTestnet() public {
        vm.chainId(97);

        deployScript.exposedRequireMockUSDTDeploymentAllowed();
    }

    function test_MockUSDTDeploymentGuard_AllowsLocalChain() public {
        vm.chainId(31_337);

        deployScript.exposedRequireMockUSDTDeploymentAllowed();
    }

    function test_MockUSDTDeploymentGuard_RevertsOnBscMainnet() public {
        vm.chainId(56);

        vm.expectRevert(abi.encodeWithSelector(USDT0Deploy.MockUSDTDeploymentNotAllowed.selector, uint256(56)));
        deployScript.exposedRequireMockUSDTDeploymentAllowed();
    }
}
