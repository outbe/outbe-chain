// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";

import {USDTDeploy} from "../../script/usdt/USDTDeploy.s.sol";
import {WCOENDeploy} from "../../script/wcoen/WCOENDeploy.s.sol";

contract ContractOwnerMock {}

contract USDTDeployHarness is USDTDeploy {
    function exposedRequireContractOwnerOnGuardedChain(address owner, bool allowEoaOwner) external view {
        _requireContractOwnerOnGuardedChain(owner, allowEoaOwner);
    }

    function exposedRequireMockUSDTDeploymentAllowed() external view {
        _requireMockUSDTDeploymentAllowed();
    }
}

contract WCOENDeployHarness is WCOENDeploy {
    function exposedRequireContractOwnerOnGuardedChain(address owner, bool allowEoaOwner) external view {
        _requireContractOwnerOnGuardedChain(owner, allowEoaOwner);
    }
}

/// @dev The deploy guards decide which chains are part of the route purely from env, so a route is declared here once
///      and every test varies only `vm.chainId()`. `vm.setEnv` writes the process environment while forge runs tests
///      concurrently, so a test that sets its own env values races every sibling test — the route below is written
///      identically by every `setUp`, which makes that race harmless.
///      Sepolia is the declared external chain on purpose: it proves a new network needs env only, no code change.
contract DeployGuardsTest is Test {
    uint256 internal constant SOURCE_CHAIN = 11_155_111; // Sepolia, declared via EXTERNAL_CHAIN_ID
    uint256 internal constant TARGET_CHAIN = 54_322_345;
    uint256 internal constant UNDECLARED_CHAIN = 97; // BSC testnet — no longer privileged by hardcoded chain id
    uint256 internal constant LOCAL_CHAIN = 31_337;

    USDTDeployHarness internal usdtDeploy;
    WCOENDeployHarness internal wcoenDeploy;

    function setUp() public {
        vm.setEnv("EXTERNAL_CHAIN_ID", "11155111");
        vm.setEnv("OUTBE_CHAIN_ID", "54322345");

        usdtDeploy = new USDTDeployHarness();
        wcoenDeploy = new WCOENDeployHarness();
    }

    // === Owner guard ===
    // The mint-trust root must sit behind a multisig on every chain the route declares. An undeclared chain (a local
    // node, a scratch fork) stays unguarded so dev flows are not blocked.

    function test_Guards_RevertForEOAOwnerOnDeclaredExternalChain() public {
        vm.chainId(SOURCE_CHAIN);
        address owner = makeAddr("owner");

        vm.expectRevert(abi.encodeWithSelector(USDTDeploy.OwnerMustBeMultisigContract.selector, owner, SOURCE_CHAIN));
        usdtDeploy.exposedRequireContractOwnerOnGuardedChain(owner, false);

        vm.expectRevert(abi.encodeWithSelector(WCOENDeploy.OwnerMustBeMultisigContract.selector, owner, SOURCE_CHAIN));
        wcoenDeploy.exposedRequireContractOwnerOnGuardedChain(owner, false);
    }

    function test_Guards_RevertForEOAOwnerOnOutbeChain() public {
        vm.chainId(TARGET_CHAIN);
        address owner = makeAddr("owner");

        vm.expectRevert(abi.encodeWithSelector(USDTDeploy.OwnerMustBeMultisigContract.selector, owner, TARGET_CHAIN));
        usdtDeploy.exposedRequireContractOwnerOnGuardedChain(owner, false);

        vm.expectRevert(abi.encodeWithSelector(WCOENDeploy.OwnerMustBeMultisigContract.selector, owner, TARGET_CHAIN));
        wcoenDeploy.exposedRequireContractOwnerOnGuardedChain(owner, false);
    }

    function test_Guards_AllowContractOwnerOnGuardedChain() public {
        vm.chainId(SOURCE_CHAIN);
        address owner = address(new ContractOwnerMock());

        usdtDeploy.exposedRequireContractOwnerOnGuardedChain(owner, false);
        wcoenDeploy.exposedRequireContractOwnerOnGuardedChain(owner, false);
    }

    /// @dev BSC testnet used to be guarded by a hardcoded chain id; once it is not part of the declared route it must
    ///      behave like any other undeclared chain.
    function test_Guards_AllowEOAOwnerOnUndeclaredChain() public {
        vm.chainId(UNDECLARED_CHAIN);
        address owner = makeAddr("owner");

        usdtDeploy.exposedRequireContractOwnerOnGuardedChain(owner, false);
        wcoenDeploy.exposedRequireContractOwnerOnGuardedChain(owner, false);
    }

    function test_Guards_AllowEOAOwnerOnLocalChain() public {
        vm.chainId(LOCAL_CHAIN);
        address owner = makeAddr("owner");

        usdtDeploy.exposedRequireContractOwnerOnGuardedChain(owner, false);
        wcoenDeploy.exposedRequireContractOwnerOnGuardedChain(owner, false);
    }

    function test_Guards_AllowEOAOwnerOnGuardedChainWithExplicitOverride() public {
        vm.chainId(SOURCE_CHAIN);
        address owner = makeAddr("owner");

        usdtDeploy.exposedRequireContractOwnerOnGuardedChain(owner, true);
        wcoenDeploy.exposedRequireContractOwnerOnGuardedChain(owner, true);
    }

    // === Mock USDT guard ===
    // The mock stands in for a canonical USDT the external chain lacks, so it may only land on the chain declared as
    // the external end of the route — never on whatever chain the RPC happens to point at.

    function test_MockUSDTDeploymentGuard_AllowsDeclaredExternalChain() public {
        vm.chainId(SOURCE_CHAIN);

        usdtDeploy.exposedRequireMockUSDTDeploymentAllowed();
    }

    function test_MockUSDTDeploymentGuard_RevertsOnUndeclaredChain() public {
        vm.chainId(UNDECLARED_CHAIN);

        vm.expectRevert(abi.encodeWithSelector(USDTDeploy.MockUSDTDeploymentNotAllowed.selector, UNDECLARED_CHAIN));
        usdtDeploy.exposedRequireMockUSDTDeploymentAllowed();
    }

    function test_MockUSDTDeploymentGuard_RevertsOnMainnet() public {
        vm.chainId(1);

        vm.expectRevert(abi.encodeWithSelector(USDTDeploy.MockUSDTDeploymentNotAllowed.selector, uint256(1)));
        usdtDeploy.exposedRequireMockUSDTDeploymentAllowed();
    }
}
