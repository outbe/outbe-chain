// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {UpgradeProposal} from "@shared/UpgradeProposal.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {Create3Deploy} from "../../../deploy/Create3Deploy.sol";
import {PrepareUpgrade} from "../../../deploy/PrepareUpgrade.s.sol";

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
    uint32 internal constant OUTBE = 54_322_345;
    address internal constant HUB = address(0xB0B);
    address internal constant GOVERNANCE = address(0x60F);

    PrepareUpgradeHarness internal script;
    Create3Factory internal factory;
    address internal deployer;
    IntexNFT1155 internal nft;
    IntexNFT1155Bridge internal nftBridge;
    TargetRouter internal router;

    function setUp() public {
        script = new PrepareUpgradeHarness();
        factory = new Create3Factory();
        deployer = vm.addr(PK);
        vm.setEnv("CREATE3_FACTORY_ADDRESS", vm.toString(address(factory)));
        vm.setEnv("CONTRACT_SALT", SALT);

        // A remote target stack: no OriginRouter, EscrowAdapter or IntexAuction here.
        vm.startPrank(deployer);
        nft = IntexNFT1155(
            _deploy("IntexNFT1155", address(new IntexNFT1155()), abi.encodeCall(IntexNFT1155.initialize, (deployer)))
        );
        nftBridge = IntexNFT1155Bridge(
            payable(_deploy(
                    "IntexNFT1155Bridge",
                    address(new IntexNFT1155Bridge(address(nft), HUB)),
                    abi.encodeCall(IntexNFT1155Bridge.initialize, (deployer))
                ))
        );
        router = TargetRouter(
            payable(_deploy(
                    "TargetRouter",
                    address(new TargetRouter(HUB, OUTBE)),
                    abi.encodeCall(TargetRouter.initialize, (deployer))
                ))
        );
        vm.stopPrank();
    }

    function test_DeployerOwned_UpgradesWithTheKey() public {
        address oldImpl = _implementation(address(router));

        script.prepareChain(PK);

        assertTrue(_implementation(address(router)) != oldImpl, "router upgraded");
        assertEq(address(TargetRouter(payable(_implementation(address(router)))).BRIDGE()), HUB, "hub kept");
        assertEq(TargetRouter(payable(_implementation(address(router)))).OUTBE_CHAIN_ID(), OUTBE, "outbe kept");
        assertEq(bytes(script.proposal()).length, 0, "no payload");
    }

    function test_GovernanceOwned_PrintsOnePayloadWithTheLiveImmutables() public {
        _handOver();
        script.setSourceCommit("abc123");
        address oldImpl = _implementation(address(router));

        script.prepareChain(PK);

        assertEq(_implementation(address(router)), oldImpl, "nothing upgraded");
        string memory payload = script.proposal();
        assertEq(vm.parseJsonString(payload, ".operation"), "upgradeContracts");
        assertTrue(vm.keyExistsJson(payload, ".upgrades[2]"), "three upgrades");
        assertFalse(vm.keyExistsJson(payload, ".upgrades[3]"), "only the deployed proxies");
        assertEq(vm.parseJsonAddress(payload, ".upgrades[0].proxy"), address(nft));
        assertEq(vm.parseJsonAddress(payload, ".upgrades[1].proxy"), address(nftBridge));
        assertEq(vm.parseJsonAddress(payload, ".upgrades[2].proxy"), address(router));
        assertEq(vm.parseJsonUint(payload, ".upgrades[2].chainId"), block.chainid);
        assertEq(vm.parseJsonBytes(payload, ".upgrades[2].callData"), "");
        address[2] memory impls = [
            vm.parseJsonAddress(payload, ".upgrades[1].implementation"),
            vm.parseJsonAddress(payload, ".upgrades[2].implementation")
        ];
        IntexNFT1155Bridge bridgeImpl = IntexNFT1155Bridge(payable(impls[0]));
        assertEq(address(bridgeImpl.token()), address(nft), "bridged token kept");
        assertEq(address(bridgeImpl.BRIDGE()), HUB, "bridge hub kept");
        TargetRouter routerImpl = TargetRouter(payable(impls[1]));
        assertEq(address(routerImpl.BRIDGE()), HUB, "router hub kept");
        assertEq(routerImpl.OUTBE_CHAIN_ID(), OUTBE, "outbe kept");
    }

    function test_RevertWhen_PayloadHasNoSourceCommit() public {
        _handOver();
        vm.expectRevert(UpgradeProposal.MissingSourceCommit.selector);
        script.prepareChain(PK);
    }

    function test_RevertWhen_OwnershipIsMixed() public {
        vm.startPrank(deployer);
        router.grantRole(bytes32(0), GOVERNANCE);
        router.renounceRole(bytes32(0), deployer);
        vm.stopPrank();

        vm.expectRevert(abi.encodeWithSelector(UpgradeProposal.MixedOwnership.selector, address(router)));
        script.prepareChain(PK);
    }

    function _deploy(string memory prefix, address impl, bytes memory initData) internal returns (address) {
        return Create3Deploy.deployProxy(factory, deployer, prefix, SALT, impl, initData);
    }

    function _handOver() internal {
        address[3] memory proxies = [address(nft), address(nftBridge), address(router)];
        vm.startPrank(deployer);
        for (uint256 i = 0; i < proxies.length; i++) {
            IntexNFT1155(proxies[i]).grantRole(bytes32(0), GOVERNANCE);
            IntexNFT1155(proxies[i]).renounceRole(bytes32(0), deployer);
        }
        vm.stopPrank();
    }

    function _implementation(address proxy) internal view returns (address) {
        return address(uint160(uint256(vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT))));
    }
}
