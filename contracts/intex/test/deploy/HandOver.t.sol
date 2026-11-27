// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {CROSSCHAIN_BRIDGE} from "@precompiles/ICrosschainBridge.sol";
import {IntexCreate3} from "../../script/IntexCreate3.sol";
import {DeployAll} from "../../script/DeployAll.s.sol";
import {DeployIntexAuctionTargetRouter} from "../../script/5_DeployIntexAuctionTargetRouter.s.sol";
import {MockTheCompact} from "../mocks/MockTheCompact.sol";
import {MockWCOEN} from "../mocks/MockWCOEN.sol";
import {DeployAllHarness, HandOverHarness} from "./ScriptHarness.sol";

contract HandOverTest is Test {
    uint256 internal constant PK = 0xA11CE;
    string internal constant SALT = "v-hand-over";
    address internal constant GOVERNANCE_ICA = address(0x1CA);

    HandOverHarness internal script;
    address internal deployer;
    address[5] internal proxies;

    function setUp() public {
        Create3Factory factory = new Create3Factory();
        DeployAllHarness deploy = new DeployAllHarness();
        script = new HandOverHarness();
        deploy.setEnv(address(factory), SALT, false, GOVERNANCE_ICA);
        script.setEnv(address(factory), SALT, false, GOVERNANCE_ICA);
        deployer = vm.addr(PK);

        deploy.deployWith(
            PK,
            DeployAll.Config({
                outbe: false,
                compact: address(new MockTheCompact()),
                wcoen: address(new MockWCOEN()),
                remoteChainIds: new uint256[](0),
                transport: DeployIntexAuctionTargetRouter.Transport({
                    bridge: address(0xB0B), outbeChainId: 1, wcoenBridge: address(0xC0E), outbeIca: GOVERNANCE_ICA
                })
            })
        );
        proxies = IntexCreate3.proxies(script.stack(deployer));
    }

    function test_HandOver_GivesEveryContractToGovernance() public {
        script.handOverWith(PK);

        for (uint256 i = 0; i < proxies.length; i++) {
            assertEq(Ownable(proxies[i]).owner(), GOVERNANCE_ICA, "governance owns it");
        }
    }

    function test_HandOver_LeavesTheDeployerNoRight() public {
        script.handOverWith(PK);

        for (uint256 i = 0; i < proxies.length; i++) {
            vm.prank(deployer);
            vm.expectRevert(abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, deployer));
            Ownable(proxies[i]).transferOwnership(deployer);
        }
    }

    function test_HandOver_SecondRunSendsNothing() public {
        script.handOverWith(PK);
        vm.recordLogs();
        script.handOverWith(PK);
        assertEq(vm.getRecordedLogs().length, 0, "nothing changes");
    }

    function test_GovernanceOwner_IsCrosschainBridgeOnOutbeAndItsAccountElsewhere() public {
        assertEq(script.governanceOwner(), GOVERNANCE_ICA, "other chain");
        script.setEnv(address(0), SALT, true, GOVERNANCE_ICA);
        assertEq(script.governanceOwner(), CROSSCHAIN_BRIDGE, "Outbe");
    }
}
