// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {CROSSCHAIN_GOV} from "@precompiles/ICrosschainGov.sol";
import {DeployAll, Externals} from "../script/DeployAll.s.sol";
import {HandOver, IntentContracts} from "../script/5_HandOver.s.sol";
import {MockERC7786Bridge} from "./mocks/MockERC7786Bridge.sol";
import {MockTheCompact} from "./mocks/MockTheCompact.sol";

/// @dev A test has no Outbe node, so the harness supplies the Interchain Account of the precompile.
contract DeployAllHarness is DeployAll {
    address internal constant GOVERNANCE_ICA = address(uint160(0x1CA));

    function _governanceAccount(uint32) internal pure override returns (address) {
        return GOVERNANCE_ICA;
    }
}

/// @dev `setUp` writes one constant env value. Forge runs tests concurrently, so no test writes its own
///      env value.
contract HandOverTest is Test {
    uint256 internal constant OUTBE_CHAIN = 54_322_345;
    uint256 internal constant OTHER_CHAIN = 11_155_111;
    address internal constant GOVERNANCE_ICA = address(uint160(0x1CA));
    string internal constant SALT = "HANDOVER_TEST";

    DeployAllHarness internal script;
    Create3Factory internal factory;
    Externals internal externals;

    function setUp() public {
        vm.setEnv("OUTBE_CHAIN_ID", vm.toString(OUTBE_CHAIN));
        script = new DeployAllHarness();
        factory = new Create3Factory();
        externals = Externals({
            compact: address(new MockTheCompact()),
            bridge: address(new MockERC7786Bridge(OUTBE_CHAIN)),
            collateralBps: 1000
        });
    }

    /// @dev The script is the deployer: it owns every contract until the hand-over.
    function _deploy(address governance) internal returns (IntentContracts memory) {
        return script.deployAll(address(factory), SALT, address(script), externals, governance);
    }

    function _assertOwner(IntentContracts memory deployed, address expected) internal view {
        assertEq(Ownable(deployed.auction).owner(), expected, "Auction");
        assertEq(Ownable(deployed.escrow).owner(), expected, "SolverEscrow");
        assertEq(Ownable(deployed.solverAllocator).owner(), expected, "SolverAllocator");
        assertEq(Ownable(deployed.router).owner(), expected, "Router");
        assertEq(Ownable(deployed.allocator).owner(), expected, "RouterAllocator");
    }

    function test_DeployAll_OnOutbe_EndsWithCrosschainGovAsOwner() public {
        vm.chainId(OUTBE_CHAIN);
        address governance = script.governanceOwner();
        assertEq(governance, CROSSCHAIN_GOV, "governance on Outbe");

        _assertOwner(_deploy(governance), CROSSCHAIN_GOV);
    }

    function test_DeployAll_OnAnotherChain_EndsWithTheInterchainAccountAsOwner() public {
        vm.chainId(OTHER_CHAIN);
        address governance = script.governanceOwner();
        assertEq(governance, GOVERNANCE_ICA, "governance on another chain");

        _assertOwner(_deploy(governance), GOVERNANCE_ICA);
    }

    function test_DeployAll_KeepDeployerAsOwner_SkipsTheHandOver() public {
        vm.chainId(OUTBE_CHAIN);
        _assertOwner(_deploy(address(0)), address(script));
    }

    function test_HandOver_IsIdempotent() public {
        vm.chainId(OUTBE_CHAIN);
        IntentContracts memory deployed = _deploy(address(0));

        script.handOver(address(script), CROSSCHAIN_GOV, deployed);
        script.handOver(address(script), CROSSCHAIN_GOV, deployed);

        _assertOwner(deployed, CROSSCHAIN_GOV);
    }

    function test_HandOver_RevertsOnAnUnexpectedHolder() public {
        vm.chainId(OUTBE_CHAIN);
        IntentContracts memory deployed = _deploy(address(0));
        address stranger = makeAddr("stranger");
        vm.prank(address(script));
        Ownable(deployed.router).transferOwnership(stranger);

        vm.expectRevert(abi.encodeWithSelector(HandOver.UnexpectedOwner.selector, deployed.router, stranger));
        script.handOver(address(script), CROSSCHAIN_GOV, deployed);
    }
}
