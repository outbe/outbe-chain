// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {Create3Proxy} from "@shared/Create3Proxy.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {ERC7786TokenBridge} from "../../src/ERC7786TokenBridge.sol";
import {USDT} from "../../src/canonical/USDT.sol";
import {PrepareUpgrade} from "../../script/PrepareUpgrade.s.sol";

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
    address internal constant HUB = address(0xB0B);
    address internal constant GOVERNANCE = address(0x60F);

    PrepareUpgradeHarness internal script;
    Create3Factory internal factory;
    address internal deployer;
    address internal usdt;
    ERC7786TokenBridge internal bridge;

    function setUp() public {
        script = new PrepareUpgradeHarness();
        factory = new Create3Factory();
        deployer = vm.addr(PK);
        usdt = address(new USDT());
        vm.setEnv("CREATE3_FACTORY_ADDRESS", vm.toString(address(factory)));
        vm.setEnv("CONTRACT_SALT", SALT);

        // Only the USDT route has a bridge here.
        address impl = address(new ERC7786TokenBridge(usdt, HUB, ERC7786TokenBridge.TokenBridgeMode.LockUnlock));
        vm.prank(deployer);
        bridge = ERC7786TokenBridge(
            Create3Proxy.deploy(
                factory,
                keccak256(abi.encodePacked("USDTBridge", SALT)),
                impl,
                abi.encodeCall(ERC7786TokenBridge.initialize, (deployer, new ERC7786TokenBridge.Peer[](0)))
            )
        );
    }

    function test_DeployerOwned_UpgradesWithTheKey() public {
        address oldImpl = _implementation(address(bridge));

        script.prepareChain(PK);

        address newImpl = _implementation(address(bridge));
        assertTrue(newImpl != oldImpl, "bridge upgraded");
        _assertImmutables(newImpl);
        assertEq(bytes(script.proposal()).length, 0, "no payload");
    }

    function test_GovernanceOwned_PrintsOnePayloadWithTheLiveImmutables() public {
        vm.prank(deployer);
        bridge.transferOwnership(GOVERNANCE);
        script.setSourceCommit("abc123");

        script.prepareChain(PK);

        string memory payload = script.proposal();
        assertFalse(vm.keyExistsJson(payload, ".upgrades[1]"), "only the deployed bridge");
        assertEq(vm.parseJsonAddress(payload, ".upgrades[0].proxy"), address(bridge));
        _assertImmutables(vm.parseJsonAddress(payload, ".upgrades[0].implementation"));
    }

    function test_RevertWhen_PayloadHasNoSourceCommit() public {
        vm.prank(deployer);
        bridge.transferOwnership(GOVERNANCE);

        vm.expectRevert(UpgradeProposal.MissingSourceCommit.selector);
        script.prepareChain(PK);
    }

    function _assertImmutables(address impl) internal view {
        ERC7786TokenBridge implementation = ERC7786TokenBridge(impl);
        assertEq(address(implementation.token()), usdt, "token kept");
        assertEq(address(implementation.bridge()), HUB, "hub kept");
        assertEq(uint8(implementation.mode()), uint8(ERC7786TokenBridge.TokenBridgeMode.LockUnlock), "mode kept");
        assertEq(implementation.tokenDecimals(), 6, "decimals kept");
    }

    function _implementation(address proxy) internal view returns (address) {
        return address(uint160(uint256(vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT))));
    }
}
