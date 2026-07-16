// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";

import {USDT0Deploy} from "../../script/usdt0/USDT0Deploy.s.sol";
import {WCOENDeploy} from "../../script/wcoen/WCOENDeploy.s.sol";

contract ContractOwnerMock {}

contract USDT0DeployHarness is USDT0Deploy {
    function exposedRequireContractOwnerOnGuardedChain(address owner) external view {
        _requireContractOwnerOnGuardedChain(owner);
    }
}

contract WCOENDeployHarness is WCOENDeploy {
    function exposedRequireContractOwnerOnGuardedChain(address owner) external view {
        _requireContractOwnerOnGuardedChain(owner);
    }
}

contract DeployOwnerGuardTest is Test {
    USDT0DeployHarness internal usdt0Deploy;
    WCOENDeployHarness internal wcoenDeploy;

    function setUp() public {
        vm.setEnv("BSC_CHAIN_ID", "0");
        vm.setEnv("OUTBE_CHAIN_ID", "0");

        usdt0Deploy = new USDT0DeployHarness();
        wcoenDeploy = new WCOENDeployHarness();
    }

    function test_USDT0Guard_RevertsForEOAOwnerOnBscTestnet() public {
        vm.chainId(97);
        address owner = makeAddr("owner");

        vm.expectRevert(abi.encodeWithSelector(USDT0Deploy.OwnerMustBeMultisigContract.selector, owner, uint256(97)));
        usdt0Deploy.exposedRequireContractOwnerOnGuardedChain(owner);
    }

    function test_WCOENGuard_RevertsForEOAOwnerOnBscTestnet() public {
        vm.chainId(97);
        address owner = makeAddr("owner");

        vm.expectRevert(abi.encodeWithSelector(WCOENDeploy.OwnerMustBeMultisigContract.selector, owner, uint256(97)));
        wcoenDeploy.exposedRequireContractOwnerOnGuardedChain(owner);
    }

    function test_Guards_AllowContractOwnerOnBscTestnet() public {
        vm.chainId(97);
        address owner = address(new ContractOwnerMock());

        usdt0Deploy.exposedRequireContractOwnerOnGuardedChain(owner);
        wcoenDeploy.exposedRequireContractOwnerOnGuardedChain(owner);
    }

    function test_Guards_AllowEOAOwnerOnLocalChain() public {
        vm.chainId(31_337);
        address owner = makeAddr("owner");

        usdt0Deploy.exposedRequireContractOwnerOnGuardedChain(owner);
        wcoenDeploy.exposedRequireContractOwnerOnGuardedChain(owner);
    }

    function test_Guards_UseConfiguredOutbeChainId() public {
        vm.setEnv("OUTBE_CHAIN_ID", "54322345");
        vm.chainId(54_322_345);
        address owner = makeAddr("owner");

        vm.expectRevert(
            abi.encodeWithSelector(USDT0Deploy.OwnerMustBeMultisigContract.selector, owner, uint256(54_322_345))
        );
        usdt0Deploy.exposedRequireContractOwnerOnGuardedChain(owner);
    }
}
