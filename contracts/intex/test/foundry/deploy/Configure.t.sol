// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test, Vm} from "forge-std/Test.sol";
import {IAccessControl} from "@openzeppelin/contracts/access/IAccessControl.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {BaseScript} from "../../../deploy/BaseScript.s.sol";
import {ConfigureOrigin} from "../../../deploy/ConfigureOrigin.s.sol";
import {ConfigureTarget} from "../../../deploy/ConfigureTarget.s.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockTheCompact} from "../../mocks/MockTheCompact.sol";
import {MockWCOEN} from "../../mocks/MockWCOEN.sol";

abstract contract ConfigureTest is Test {
    address internal constant GOVERNANCE = address(0x60F);
    address internal constant BRIDGE = address(0xB41D6E);

    function _assertNoWrites(Vm.AccountAccess[] memory accesses) internal pure {
        for (uint256 i = 0; i < accesses.length; i++) {
            for (uint256 j = 0; j < accesses[i].storageAccesses.length; j++) {
                assertFalse(accesses[i].storageAccesses[j].isWrite, "a re-run writes nothing");
            }
        }
    }

    function _handOver(address proxy, address deployer) internal {
        vm.startPrank(deployer);
        IAccessControl(proxy).grantRole(bytes32(0), GOVERNANCE);
        IAccessControl(proxy).renounceRole(bytes32(0), deployer);
        vm.stopPrank();
    }
}

contract ConfigureOriginTest is ConfigureTest {
    ConfigureOrigin internal script;
    address internal deployer;
    ConfigureOrigin.Origin internal o;

    function setUp() public {
        script = new ConfigureOrigin();
        deployer = address(script);
        o = ConfigureOrigin.Origin({
            nft: address(DeployProxy.intexNFT1155(deployer, makeAddr("bridger"))),
            intexFactory: makeAddr("intexFactory"),
            gemFactory: makeAddr("gemFactory")
        });
    }

    function test_Configure_GrantsTheBurnerRoleToBothFactories() public {
        script.configure(deployer, o);

        IntexNFT1155 nft = IntexNFT1155(o.nft);
        assertTrue(nft.hasRole(nft.BURNER_ROLE(), o.intexFactory));
        assertTrue(nft.hasRole(nft.BURNER_ROLE(), o.gemFactory));
    }

    function test_Configure_GrantsNoRouterOrBridgeRoleOnOutbe() public {
        script.configure(deployer, o);

        IntexNFT1155 nft = IntexNFT1155(o.nft);
        address[2] memory callers = [o.intexFactory, o.gemFactory];
        for (uint256 i = 0; i < callers.length; i++) {
            assertFalse(nft.hasRole(nft.ROUTER_ROLE(), callers[i]));
            assertFalse(nft.hasRole(nft.BRIDGE_ROLE(), callers[i]));
        }
    }

    function test_Configure_SecondRunWritesNothing() public {
        script.configure(deployer, o);

        vm.startStateDiffRecording();
        script.configure(deployer, o);
        _assertNoWrites(vm.stopAndReturnStateDiff());
    }

    function test_Configure_GrantsAChangedFactory() public {
        script.configure(deployer, o);
        o.intexFactory = makeAddr("newIntexFactory");

        script.configure(deployer, o);

        IntexNFT1155 nft = IntexNFT1155(o.nft);
        assertTrue(nft.hasRole(nft.BURNER_ROLE(), o.intexFactory));
    }

    function test_Configure_HandedOverAndWired_IsANoop() public {
        script.configure(deployer, o);
        _handOver(o.nft, deployer);

        script.configure(deployer, o);
    }

    function test_RevertWhen_HandedOverAndARoleIsMissing() public {
        script.configure(deployer, o);
        _handOver(o.nft, deployer);
        o.gemFactory = makeAddr("newGemFactory");

        vm.expectRevert(abi.encodeWithSelector(BaseScript.HandedOverToGovernance.selector, o.nft));
        script.configure(deployer, o);
    }

    function test_RevertWhen_TheNftIsNotDeployed() public {
        o.nft = makeAddr("nothing");

        vm.expectRevert(abi.encodeWithSelector(BaseScript.NotDeployed.selector, o.nft));
        script.configure(deployer, o);
    }
}

contract ConfigureTargetTest is ConfigureTest {
    ConfigureTarget internal script;
    address internal deployer;
    ConfigureTarget.Target internal t;

    function setUp() public {
        script = new ConfigureTarget();
        deployer = address(script);
        address bridger = makeAddr("bridger");
        address nft = address(DeployProxy.intexNFT1155(deployer, bridger));
        t = ConfigureTarget.Target({
            nft: nft,
            escrow: address(DeployProxy.escrowAdapter(deployer, bridger)),
            auction: address(DeployProxy.intexAuction(deployer, bridger)),
            nftRouter: address(DeployProxy.intexNFT1155Router(nft, BRIDGE, deployer)),
            router: address(DeployProxy.targetRouter(BRIDGE, deployer, 1)),
            compact: address(new MockTheCompact()),
            wcoen: address(new MockWCOEN())
        });
    }

    function test_Configure_WiresTheStackAndGrantsTheRelayerRoles() public {
        script.configure(deployer, t);

        assertEq(address(IntexAuction(t.auction).escrow()), t.escrow);
        EscrowAdapter escrow = EscrowAdapter(payable(t.escrow));
        assertEq(escrow.intexAuction(), t.auction);
        assertEq(address(escrow.compact()), t.compact);
        assertEq(address(escrow.intexAuctionAsset()), t.wcoen);
        IntexAuctionTargetRouter router = IntexAuctionTargetRouter(payable(t.router));
        assertEq(address(router.intexAuction()), t.auction);
        assertEq(address(router.intexContract()), t.nft);
        assertEq(address(router.escrow()), t.escrow);
        assertTrue(IntexAuction(t.auction).hasRole(IntexAuction(t.auction).RELAYER_ROLE(), t.router));
        assertTrue(escrow.hasRole(escrow.RELAYER_ROLE(), t.router));
        IntexNFT1155 nft = IntexNFT1155(t.nft);
        assertTrue(nft.hasRole(nft.ROUTER_ROLE(), t.router));
        assertTrue(nft.hasRole(nft.BRIDGE_ROLE(), t.nftRouter));
    }

    function test_Configure_GivesEachTokenHolderOnlyItsRole() public {
        script.configure(deployer, t);

        IntexNFT1155 nft = IntexNFT1155(t.nft);
        assertFalse(nft.hasRole(nft.BRIDGE_ROLE(), t.router));
        assertFalse(nft.hasRole(nft.ROUTER_ROLE(), t.nftRouter));
    }

    function test_Configure_SecondRunWritesNothing() public {
        script.configure(deployer, t);

        vm.startStateDiffRecording();
        script.configure(deployer, t);
        _assertNoWrites(vm.stopAndReturnStateDiff());
    }

    function test_RevertWhen_HandedOverAndUnwired() public {
        _handOver(t.auction, deployer);

        vm.expectRevert(abi.encodeWithSelector(BaseScript.HandedOverToGovernance.selector, t.auction));
        script.configure(deployer, t);
    }

    function test_RevertWhen_TheNftBridgeIsNotDeployed() public {
        t.nftRouter = makeAddr("nothing");

        vm.expectRevert(abi.encodeWithSelector(BaseScript.NotDeployed.selector, t.nftRouter));
        script.configure(deployer, t);
    }
}
