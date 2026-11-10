// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {Create3Proxy} from "@shared/Create3Proxy.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {IntentCreate3} from "../script/IntentCreate3.sol";
import {PrepareUpgrade} from "../script/PrepareUpgrade.s.sol";
import {Auction} from "../src/Auction.sol";
import {SolverEscrow} from "../src/SolverEscrow.sol";
import {Router} from "../src/router/Router.sol";
import {MockERC7786Bridge} from "./mocks/MockERC7786Bridge.sol";
import {MockTheCompact} from "./mocks/MockTheCompact.sol";

/// @dev The test sets the source commit itself, so parallel tests share no env value that differs.
contract PrepareUpgradeHarness is PrepareUpgrade {
    string internal commit;

    function setSourceCommit(string memory commit_) external {
        commit = commit_;
    }

    function _sourceCommit() internal view override returns (string memory) {
        return commit;
    }
}

contract PrepareUpgradeTest is Test {
    string internal constant SALT = "v-test";
    uint256 internal constant PK = 0xA11CE;
    bytes12 internal constant LOCK_TAG = bytes12(uint96(7));
    address internal constant GOVERNANCE = address(0x60F);

    PrepareUpgradeHarness internal script;
    address internal factory;
    address internal deployer;
    address internal compact;
    address internal bridge;
    Auction internal auction;
    SolverEscrow internal escrow;
    Router internal router;

    function setUp() public {
        script = new PrepareUpgradeHarness();
        factory = address(new Create3Factory());
        deployer = vm.addr(PK);
        compact = address(new MockTheCompact());
        bridge = address(new MockERC7786Bridge(block.chainid));
        vm.setEnv("CREATE3_FACTORY_ADDRESS", vm.toString(factory));
        vm.setEnv("CONTRACT_SALT", SALT);

        // No allocators here: the script skips a proxy that is not deployed.
        vm.startPrank(deployer);
        auction = Auction(
            _deploy(IntentCreate3.AUCTION, address(new Auction()), abi.encodeCall(Auction.initialize, (deployer)))
        );
        escrow = SolverEscrow(
            _deploy(
                IntentCreate3.SOLVER_ESCROW,
                address(new SolverEscrow(compact, LOCK_TAG)),
                abi.encodeCall(SolverEscrow.initialize, (deployer, 1000))
            )
        );
        router = Router(
            _deploy(
                IntentCreate3.ROUTER,
                address(new Router(bridge, compact, LOCK_TAG, address(escrow), address(auction))),
                abi.encodeCall(Router.initialize, (deployer))
            )
        );
        vm.stopPrank();
    }

    function test_DeployerOwned_UpgradesWithTheKey() public {
        address oldImpl = _implementation(address(router));

        script.prepareChain(PK);

        address newImpl = _implementation(address(router));
        assertTrue(newImpl != oldImpl, "router upgraded");
        _assertRouterImmutables(newImpl);
        assertEq(bytes(script.proposal()).length, 0, "no payload");
    }

    function test_GovernanceOwned_PrintsOnePayloadWithTheLiveImmutables() public {
        _handOver();
        script.setSourceCommit("abc123");

        script.prepareChain(PK);

        string memory payload = script.proposal();
        assertTrue(vm.keyExistsJson(payload, ".upgrades[2]"), "three upgrades");
        assertFalse(vm.keyExistsJson(payload, ".upgrades[3]"), "only the deployed proxies");
        assertEq(vm.parseJsonAddress(payload, ".upgrades[0].proxy"), address(auction));
        assertEq(vm.parseJsonAddress(payload, ".upgrades[1].proxy"), address(escrow));
        assertEq(vm.parseJsonAddress(payload, ".upgrades[2].proxy"), address(router));
        SolverEscrow escrowImpl = SolverEscrow(vm.parseJsonAddress(payload, ".upgrades[1].implementation"));
        assertEq(address(escrowImpl.COMPACT()), compact, "escrow compact kept");
        assertEq(escrowImpl.LOCK_TAG(), LOCK_TAG, "escrow lock tag kept");
        _assertRouterImmutables(vm.parseJsonAddress(payload, ".upgrades[2].implementation"));
    }

    function test_RevertWhen_PayloadHasNoSourceCommit() public {
        _handOver();
        vm.expectRevert(UpgradeProposal.MissingSourceCommit.selector);
        script.prepareChain(PK);
    }

    function test_RevertWhen_OwnershipIsMixed() public {
        vm.prank(deployer);
        router.transferOwnership(GOVERNANCE);

        vm.expectRevert(abi.encodeWithSelector(UpgradeProposal.MixedOwnership.selector, address(router)));
        script.prepareChain(PK);
    }

    function _deploy(string memory label, address impl, bytes memory initData) internal returns (address) {
        return Create3Proxy.deploy(Create3Factory(factory), IntentCreate3.salt(label, SALT), impl, initData);
    }

    function _handOver() internal {
        vm.startPrank(deployer);
        auction.transferOwnership(GOVERNANCE);
        escrow.transferOwnership(GOVERNANCE);
        router.transferOwnership(GOVERNANCE);
        vm.stopPrank();
    }

    function _assertRouterImmutables(address impl) internal view {
        Router implementation = Router(impl);
        assertEq(address(implementation.bridge()), bridge, "hub kept");
        assertEq(address(implementation.COMPACT()), compact, "compact kept");
        assertEq(implementation.LOCK_TAG(), LOCK_TAG, "lock tag kept");
        assertEq(address(implementation.SOLVER_ESCROW()), address(escrow), "escrow kept");
        assertEq(address(implementation.AUCTION()), address(auction), "auction kept");
    }

    function _implementation(address proxy) internal view returns (address) {
        return address(uint160(uint256(vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT))));
    }
}
