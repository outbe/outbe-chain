// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {IntexCreate3} from "../../script/IntexCreate3.sol";
import {DeployAll} from "../../script/DeployAll.s.sol";
import {DeployIntexAuctionTargetRouter} from "../../script/5_DeployIntexAuctionTargetRouter.s.sol";
import {MockTheCompact} from "../mocks/MockTheCompact.sol";
import {MockWCOEN} from "../mocks/MockWCOEN.sol";
import {DeployAllHarness, PrepareUpgradeHarness} from "./ScriptHarness.sol";

contract PrepareUpgradeTest is Test {
    uint256 internal constant PK = 0xA11CE;
    string internal constant SALT = "v-prepare-upgrade";
    address internal constant GOVERNANCE = address(0x60F);

    DeployAllHarness internal deploy;
    PrepareUpgradeHarness internal script;
    DeployAll.Config internal config;
    IntexCreate3.Stack internal s;

    function setUp() public {
        Create3Factory factory = new Create3Factory();
        deploy = new DeployAllHarness();
        script = new PrepareUpgradeHarness();
        deploy.setEnv(address(factory), SALT, true, address(0x1CA));
        script.setEnv(address(factory), SALT, true, address(0x1CA));

        uint256[] memory remoteChainIds = new uint256[](1);
        remoteChainIds[0] = 97;
        config.outbe = true;
        config.compact = address(new MockTheCompact());
        config.wcoen = address(new MockWCOEN());
        config.remoteChainIds = remoteChainIds;
        config.transport = DeployIntexAuctionTargetRouter.Transport({
            bridge: address(0xB0B),
            outbeChainId: uint32(block.chainid),
            wcoenBridge: address(0xC0E),
            outbeIca: address(0x1CA)
        });
        deploy.deployWith(PK, config);
        s = deploy.stack(vm.addr(PK));
    }

    function test_DeployerOwned_UpgradesWithTheKeyAndKeepsEveryLink() public {
        address[5] memory proxies = IntexCreate3.proxies(s);
        address[5] memory oldImpls;
        for (uint256 i = 0; i < proxies.length; i++) {
            oldImpls[i] = _implementation(proxies[i]);
        }

        script.prepareChain(PK);

        for (uint256 i = 0; i < proxies.length; i++) {
            assertTrue(_implementation(proxies[i]) != oldImpls[i], "upgraded");
        }
        deploy.checkLinks(s, config);
        assertEq(bytes(script.proposal()).length, 0, "no payload");
    }

    function test_GovernanceOwned_PrintsOnePayloadWithTheLiveLinks() public {
        _handOver();
        script.setSourceCommit("abc123");
        address oldImpl = _implementation(s.intexAuctionTargetRouter);

        script.prepareChain(PK);

        assertEq(_implementation(s.intexAuctionTargetRouter), oldImpl, "nothing upgraded");
        string memory payload = script.proposal();
        assertEq(vm.parseJsonString(payload, ".operation"), "upgradeContracts");
        assertTrue(vm.keyExistsJson(payload, ".upgrades[4]"), "five upgrades");
        assertFalse(vm.keyExistsJson(payload, ".upgrades[5]"), "one per proxy");

        address[5] memory proxies = IntexCreate3.proxies(s);
        for (uint256 i = 0; i < proxies.length; i++) {
            string memory key = string.concat(".upgrades[", vm.toString(i), "]");
            assertEq(vm.parseJsonAddress(payload, string.concat(key, ".proxy")), proxies[i]);
            assertEq(vm.parseJsonBytes(payload, string.concat(key, ".callData")), "");
            // Point the proxy at the new implementation to read its links through the readers.
            vm.store(
                proxies[i],
                ERC1967Utils.IMPLEMENTATION_SLOT,
                bytes32(uint256(uint160(vm.parseJsonAddress(payload, string.concat(key, ".implementation")))))
            );
        }
        deploy.checkLinks(s, config);
    }

    function test_RevertWhen_PayloadHasNoSourceCommit() public {
        _handOver();
        vm.expectRevert(UpgradeProposal.MissingSourceCommit.selector);
        script.prepareChain(PK);
    }

    function test_RevertWhen_OwnershipIsMixed() public {
        vm.prank(vm.addr(PK));
        Ownable(s.intexAuctionTargetRouter).transferOwnership(GOVERNANCE);

        vm.expectRevert(abi.encodeWithSelector(UpgradeProposal.MixedOwnership.selector, s.intexAuctionTargetRouter));
        script.prepareChain(PK);
    }

    function _handOver() internal {
        address[5] memory proxies = IntexCreate3.proxies(s);
        for (uint256 i = 0; i < proxies.length; i++) {
            vm.prank(vm.addr(PK));
            Ownable(proxies[i]).transferOwnership(GOVERNANCE);
        }
    }

    function _implementation(address proxy) internal view returns (address) {
        return address(uint160(uint256(vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT))));
    }
}
