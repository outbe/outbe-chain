// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {Create3Proxy} from "@shared/Create3Proxy.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {ERC7786Bridge} from "src/ERC7786Bridge.sol";
import {HyperlaneGatewayAdapter} from "src/adapters/HyperlaneGatewayAdapter.sol";
import {PrepareUpgrade} from "../script/PrepareUpgrade.s.sol";
import {MockHyperlaneMailbox} from "./mocks/MockHyperlaneMailbox.sol";

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
    string internal constant SALT = "v-prepare-upgrade";
    uint256 internal constant PK = 0xA11CE;
    address internal constant GOVERNANCE = address(0x60F);

    PrepareUpgradeHarness internal script;
    Create3Factory internal factory;
    address internal deployer;
    address internal mailbox;
    ERC7786Bridge internal hub;
    HyperlaneGatewayAdapter internal adapter;

    function setUp() public {
        script = new PrepareUpgradeHarness();
        factory = new Create3Factory();
        deployer = vm.addr(PK);
        mailbox = address(new MockHyperlaneMailbox(1));
        vm.setEnv("CREATE3_FACTORY_ADDRESS", vm.toString(address(factory)));
        vm.setEnv("CONTRACT_SALT", SALT);

        // A Hyperlane-only stack: no LayerZero or loopback adapter here.
        vm.startPrank(deployer);
        adapter = HyperlaneGatewayAdapter(
            _deploy(
                "HyperlaneGatewayAdapter",
                address(new HyperlaneGatewayAdapter(mailbox)),
                abi.encodeCall(HyperlaneGatewayAdapter.initialize, (deployer))
            )
        );
        hub = ERC7786Bridge(
            _deploy(
                "ERC7786Bridge",
                address(new ERC7786Bridge()),
                abi.encodeCall(ERC7786Bridge.initialize, (deployer, address(adapter)))
            )
        );
        vm.stopPrank();
    }

    function test_DeployerOwned_UpgradesWithTheKey() public {
        address oldImpl = _implementation(address(adapter));

        script.prepareChain(PK);

        address newImpl = _implementation(address(adapter));
        assertTrue(newImpl != oldImpl, "adapter upgraded");
        assertEq(address(HyperlaneGatewayAdapter(newImpl).MAILBOX()), mailbox, "mailbox kept");
        assertEq(adapter.owner(), deployer, "owner kept");
        assertEq(bytes(script.proposal()).length, 0, "no payload");
    }

    function test_GovernanceOwned_PrintsOnePayloadWithTheLiveImmutables() public {
        _handOver();
        script.setSourceCommit("abc123");

        script.prepareChain(PK);

        string memory payload = script.proposal();
        assertEq(vm.parseJsonString(payload, ".operation"), "upgradeContracts");
        assertTrue(vm.keyExistsJson(payload, ".upgrades[1]"), "two upgrades");
        assertFalse(vm.keyExistsJson(payload, ".upgrades[2]"), "only the deployed proxies");
        assertEq(vm.parseJsonAddress(payload, ".upgrades[0].proxy"), address(hub));
        assertEq(vm.parseJsonAddress(payload, ".upgrades[1].proxy"), address(adapter));
        assertEq(vm.parseJsonUint(payload, ".upgrades[1].chainId"), block.chainid);
        address impl = vm.parseJsonAddress(payload, ".upgrades[1].implementation");
        assertEq(address(HyperlaneGatewayAdapter(impl).MAILBOX()), mailbox, "mailbox kept");
    }

    function test_ChainListedTwice_IsPreparedOnce() public {
        _handOver();
        script.setSourceCommit("abc123");

        script.prepareChain(PK);
        script.prepareChain(PK);

        assertFalse(vm.keyExistsJson(script.proposal(), ".upgrades[2]"), "no duplicate upgrades");
    }

    function test_RevertWhen_PayloadHasNoSourceCommit() public {
        _handOver();
        vm.expectRevert(UpgradeProposal.MissingSourceCommit.selector);
        script.prepareChain(PK);
    }

    function test_RevertWhen_OwnershipIsMixed() public {
        vm.prank(deployer);
        adapter.transferOwnership(GOVERNANCE);

        vm.expectRevert(abi.encodeWithSelector(UpgradeProposal.MixedOwnership.selector, address(adapter)));
        script.prepareChain(PK);
    }

    function _deploy(string memory label, address impl, bytes memory initData) internal returns (address) {
        return Create3Proxy.deploy(factory, keccak256(abi.encodePacked(label, SALT)), impl, initData);
    }

    function _handOver() internal {
        vm.startPrank(deployer);
        Ownable(address(hub)).transferOwnership(GOVERNANCE);
        Ownable(address(adapter)).transferOwnership(GOVERNANCE);
        vm.stopPrank();
    }

    function _implementation(address proxy) internal view returns (address) {
        return address(uint160(uint256(vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT))));
    }
}
