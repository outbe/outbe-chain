// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Test} from "forge-std/Test.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {ERC7786Bridge} from "src/ERC7786Bridge.sol";
import {HyperlaneGatewayAdapter} from "src/adapters/HyperlaneGatewayAdapter.sol";
import {LayerZeroGatewayAdapter} from "src/adapters/LayerZeroGatewayAdapter.sol";
import {DeployAll} from "script/DeployAll.s.sol";
import {HandOver} from "script/4_HandOver.s.sol";
import {DeployBase, Hub} from "script/DeployBase.sol";
import {EndpointV2Mock} from "../mocks/MockLayerZeroEndpoint.sol";
import {MockHyperlaneMailbox} from "../mocks/MockHyperlaneMailbox.sol";

contract HandOverHarness is DeployAll {
    function exposedDeployAll() external returns (Hub memory) {
        return _deployAll();
    }

    function exposedHandOverDeployed() external {
        _handOverDeployed();
    }

    function exposedDeployedHub() external view returns (Hub memory) {
        return _deployedHub();
    }

    function exposedGovernanceOwner() external returns (address) {
        return _governanceOwner();
    }

    /// @dev The harness is the deployer. Under a real run `vm.startBroadcast` puts the deployer key
    ///      behind the calls. There is no broadcast in tests.
    function _deployer() internal view override returns (address) {
        return address(this);
    }

    /// @dev There is no Outbe node in tests.
    function _controllerAccount(uint32) internal pure override returns (address) {
        return address(uint160(0x1CA));
    }
}

/// @dev Every test here relies on the constant env values that `setUp` writes. `vm.setEnv` writes
///      the process environment, and forge runs tests concurrently. Thus a test that sets its own
///      env values would race its siblings. The tests put each computed contract (factory,
///      endpoint, mailbox) at a fixed address with `vm.etch` instead.
contract HandOverTest is Test {
    uint256 internal constant OUTBE_CHAIN = 54_322_345;
    uint256 internal constant EXTERNAL_CHAIN = 11_155_111; // Sepolia
    uint256 internal constant REMOTE_CHAIN = 97;
    uint32 internal constant REMOTE_EID = 40_102;
    string internal constant SALT = "TEST_V1";

    address internal constant FACTORY = address(uint160(0xC3F00));
    address internal constant LZ_ENDPOINT = address(uint160(0x1E0));
    address internal constant MAILBOX = address(uint160(0x4A11));
    address internal constant CONTROLLER = 0x000000000000000000000000000000000000EE14;
    address internal constant CONTROLLER_ICA = address(uint160(0x1CA));

    HandOverHarness internal deploy;
    address internal deployer;

    function setUp() public {
        vm.setEnv("DEPLOYER_PK", "0xA11CE");
        vm.setEnv("CONTRACT_SALT", SALT);
        vm.setEnv("CREATE3_FACTORY_ADDRESS", "0x00000000000000000000000000000000000C3F00");
        vm.setEnv("LZ_ENDPOINT", "0x00000000000000000000000000000000000001E0");
        vm.setEnv("HYPERLANE_MAILBOX", "0x0000000000000000000000000000000000004A11");
        vm.setEnv("WIRE_LOOPBACK", "true");
        vm.setEnv("ACTIVE_GATEWAY", "lz");
        vm.setEnv("OUTBE_CHAIN_ID", "54322345");
        vm.setEnv("REMOTE_CHAIN_IDS", "97");
        vm.setEnv("REMOTE_EIDS", "40102");

        vm.etch(FACTORY, address(new Create3Factory()).code);
        vm.etch(LZ_ENDPOINT, address(new EndpointV2Mock(30_000)).code);
        vm.etch(MAILBOX, address(new MockHyperlaneMailbox(uint32(OUTBE_CHAIN))).code);

        deploy = new HandOverHarness();
        deployer = address(deploy);
    }

    // === DeployAll ===

    function test_DeployAll_OnOutbe_ControllerOwnsEverything() public {
        vm.chainId(OUTBE_CHAIN);

        Hub memory hub = deploy.exposedDeployAll();

        _assertGovernanceOwns(hub, CONTROLLER);
    }

    function test_DeployAll_OffOutbe_ControllerAccountOwnsEverything() public {
        vm.chainId(EXTERNAL_CHAIN);

        Hub memory hub = deploy.exposedDeployAll();

        _assertGovernanceOwns(hub, CONTROLLER_ICA);
    }

    /// @dev The deployer still wires the hub before the hand-over, and the hand-over does not move
    ///      the CREATE3 addresses.
    function test_DeployAll_WiresBeforeHandOver() public {
        vm.chainId(OUTBE_CHAIN);

        Hub memory hub = deploy.exposedDeployAll();

        assertEq(ERC7786Bridge(hub.bridge).getGateway(), hub.lz, "active gateway");
        assertEq(ERC7786Bridge(hub.bridge).getGateway(OUTBE_CHAIN), hub.lb, "loopback gateway");
        assertEq(LayerZeroGatewayAdapter(hub.lz).peers(REMOTE_EID), bytes32(uint256(uint160(hub.lz))), "lz peer");
        assertEq(
            HyperlaneGatewayAdapter(hub.hl).routers(uint32(REMOTE_CHAIN)),
            bytes32(uint256(uint160(hub.hl))),
            "hyperlane router"
        );

        Hub memory found = deploy.exposedDeployedHub();
        assertEq(found.bridge, hub.bridge, "bridge address");
        assertEq(found.lz, hub.lz, "lz address");
        assertEq(found.hl, hub.hl, "hyperlane address");
        assertEq(found.lb, hub.lb, "loopback address");
        assertEq(hub.bridge, _predict("ERC7786Bridge"), "bridge salt or label changed");
        assertEq(hub.lz, _predict("LayerZeroGatewayAdapter"), "lz salt or label changed");
        assertEq(hub.hl, _predict("HyperlaneGatewayAdapter"), "hyperlane salt or label changed");
        assertEq(hub.lb, _predict("LoopbackGatewayAdapter"), "loopback salt or label changed");
    }

    // === HandOver alone ===

    function test_HandOver_OnOutbe_AfterSteps1To3() public {
        vm.chainId(OUTBE_CHAIN);
        Hub memory hub = _deployAndWire();
        _assertDeployerOwns(hub);

        deploy.exposedHandOverDeployed();

        _assertGovernanceOwns(hub, CONTROLLER);
    }

    function test_HandOver_OffOutbe_AfterSteps1To3() public {
        vm.chainId(EXTERNAL_CHAIN);
        Hub memory hub = _deployAndWire();

        deploy.exposedHandOverDeployed();

        _assertGovernanceOwns(hub, CONTROLLER_ICA);
    }

    /// @dev A run that stopped after some transfers can run again. It skips what governance owns.
    function test_HandOver_CompletesPartialRun() public {
        vm.chainId(OUTBE_CHAIN);
        Hub memory hub = _deployAndWire();
        vm.prank(deployer);
        Ownable(hub.bridge).transferOwnership(CONTROLLER);

        deploy.exposedHandOverDeployed();

        _assertGovernanceOwns(hub, CONTROLLER);
    }

    /// @dev Governance owns the LayerZero adapter, but the deployer is still its delegate. The
    ///      script cannot fix that, so the read-back must fail loudly.
    function test_HandOver_RevertsWhenLzOwnerMovedWithoutDelegate() public {
        vm.chainId(OUTBE_CHAIN);
        Hub memory hub = _deployAndWire();
        vm.prank(deployer);
        Ownable(hub.lz).transferOwnership(CONTROLLER);

        vm.expectRevert(abi.encodeWithSelector(HandOver.DelegateNotGovernance.selector, hub.lz, deployer, CONTROLLER));
        deploy.exposedHandOverDeployed();
    }

    function test_HandOver_RevertsWhenAdapterIsNotDeployed() public {
        vm.chainId(OUTBE_CHAIN);
        address lz = _predict("LayerZeroGatewayAdapter");

        vm.expectRevert(abi.encodeWithSelector(HandOver.MissingCode.selector, "LayerZeroGatewayAdapter", lz));
        deploy.exposedHandOverDeployed();
    }

    // === Read-back ===

    function test_Check_RevertsWhenBridgeOwnerIsDeployer() public {
        _checkRevertsWhenOwnerIsDeployer("ERC7786Bridge", 0);
    }

    function test_Check_RevertsWhenLzOwnerIsDeployer() public {
        _checkRevertsWhenOwnerIsDeployer("LayerZeroGatewayAdapter", 1);
    }

    function test_Check_RevertsWhenHyperlaneOwnerIsDeployer() public {
        _checkRevertsWhenOwnerIsDeployer("HyperlaneGatewayAdapter", 2);
    }

    function test_Check_RevertsWhenLoopbackOwnerIsDeployer() public {
        _checkRevertsWhenOwnerIsDeployer("LoopbackGatewayAdapter", 3);
    }

    function test_Check_RevertsWhenLzDelegateIsDeployer() public {
        vm.chainId(OUTBE_CHAIN);
        Hub memory hub = deploy.exposedDeployAll();
        vm.prank(CONTROLLER);
        LayerZeroGatewayAdapter(hub.lz).setDelegate(deployer);

        vm.expectRevert(abi.encodeWithSelector(HandOver.DelegateNotGovernance.selector, hub.lz, deployer, CONTROLLER));
        deploy.checkHandOver(hub, CONTROLLER);
    }

    function test_GovernanceOwner_RevertsOnChainIdAboveDomainRange() public {
        uint256 chainId = uint256(type(uint32).max) + 1;
        vm.chainId(chainId);

        vm.expectRevert(abi.encodeWithSelector(DeployBase.DomainTooLarge.selector, chainId));
        deploy.exposedGovernanceOwner();
    }

    // === Helpers ===

    /// @dev Steps 1-3 of DeployAll without the hand-over. The harness is the deployer and the owner.
    function _deployAndWire() internal returns (Hub memory hub) {
        hub.lz = deploy.deployLayerZeroAdapter(FACTORY, SALT, deployer, LZ_ENDPOINT);
        hub.hl = deploy.deployHyperlaneAdapter(FACTORY, SALT, deployer, MAILBOX);
        hub.lb = deploy.deployLoopbackAdapter(FACTORY, SALT, deployer, deployer);
        hub.bridge = deploy.deployBridge(FACTORY, SALT, deployer, hub.lz);
        deploy.configureBridge(hub.bridge, hub.lz, hub.hl);
        deploy.configureLoopback(hub.bridge, hub.lb);
    }

    /// @dev Gives one contract back to the deployer after a full hand-over. `index` selects the
    ///      contract in `Hub` field order.
    function _checkRevertsWhenOwnerIsDeployer(string memory name, uint256 index) internal {
        vm.chainId(OUTBE_CHAIN);
        Hub memory hub = deploy.exposedDeployAll();
        address target = [hub.bridge, hub.lz, hub.hl, hub.lb][index];
        vm.prank(CONTROLLER);
        Ownable(target).transferOwnership(deployer);

        vm.expectRevert(
            abi.encodeWithSelector(HandOver.OwnerNotGovernance.selector, name, target, deployer, CONTROLLER)
        );
        deploy.checkHandOver(hub, CONTROLLER);
    }

    function _assertGovernanceOwns(Hub memory hub, address governance) internal view {
        assertEq(Ownable(hub.bridge).owner(), governance, "bridge owner");
        assertEq(Ownable(hub.lz).owner(), governance, "lz owner");
        assertEq(Ownable(hub.hl).owner(), governance, "hyperlane owner");
        assertEq(Ownable(hub.lb).owner(), governance, "loopback owner");
        assertEq(EndpointV2Mock(LZ_ENDPOINT).delegates(hub.lz), governance, "lz delegate");
        deploy.checkHandOver(hub, governance);
    }

    function _assertDeployerOwns(Hub memory hub) internal view {
        assertEq(Ownable(hub.bridge).owner(), deployer, "bridge owner");
        assertEq(Ownable(hub.lz).owner(), deployer, "lz owner");
        assertEq(Ownable(hub.hl).owner(), deployer, "hyperlane owner");
        assertEq(Ownable(hub.lb).owner(), deployer, "loopback owner");
        assertEq(EndpointV2Mock(LZ_ENDPOINT).delegates(hub.lz), deployer, "lz delegate");
    }

    function _predict(string memory label) internal view returns (address) {
        return Create3Factory(FACTORY).predict(deployer, keccak256(abi.encodePacked(label, SALT)));
    }
}
