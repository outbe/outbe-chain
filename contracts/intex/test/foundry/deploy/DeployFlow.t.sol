// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {ERC1967Utils} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Utils.sol";
import {Create3Factory} from "@contracts/deploy/Create3Factory.sol";
import {Create3Deploy} from "../../../deploy/Create3Deploy.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuction} from "@contracts/bnb/IntexAuction.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";

/// @dev Verifies the CREATE3 proxy deployment path used by the deploy scripts: deterministic
///      addresses, correct implementation pointer, initialization, idempotency, and that the proxy
///      address is independent of the implementation init code.
contract DeployFlowTest is TestHelperOz5 {
    string internal constant VERSION = "v1.0.0";
    uint32 internal constant A_EID = 1;
    uint32 internal constant B_EID = 2;

    address internal admin = makeAddr("admin");
    address internal bridger = makeAddr("bridger");

    Create3Factory internal factory;

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);
        factory = new Create3Factory();
    }

    function _implSlot(address proxy) internal view returns (address) {
        return address(uint160(uint256(vm.load(proxy, ERC1967Utils.IMPLEMENTATION_SLOT))));
    }

    function test_DeployNonLzProxy_Deterministic() public {
        address predicted = Create3Deploy.predictProxy(factory, address(this), "IntexNFT1155", VERSION);
        address impl = address(new IntexNFT1155());
        address proxy = Create3Deploy.deployProxy(
            factory,
            address(this),
            "IntexNFT1155",
            VERSION,
            impl,
            abi.encodeCall(IntexNFT1155.initialize, (admin, bridger))
        );

        assertEq(proxy, predicted, "predict != deploy");
        assertEq(_implSlot(proxy), impl, "impl pointer wrong");
        assertTrue(IntexNFT1155(proxy).hasRole(IntexNFT1155(proxy).DEFAULT_ADMIN_ROLE(), admin), "not initialized");
    }

    function test_DeployLzProxy_Deterministic() public {
        address predicted = Create3Deploy.predictProxy(factory, address(this), "OriginMessenger", VERSION);
        address impl = address(new OriginMessenger(address(endpoints[A_EID]), B_EID));
        address proxy = Create3Deploy.deployProxy(
            factory,
            address(this),
            "OriginMessenger",
            VERSION,
            impl,
            abi.encodeCall(OriginMessenger.initialize, (admin))
        );

        assertEq(proxy, predicted, "predict != deploy");
        assertEq(_implSlot(proxy), impl, "impl pointer wrong");
        assertEq(OriginMessenger(payable(proxy)).owner(), admin, "owner not set");
        assertEq(OriginMessenger(payable(proxy)).BNB_EID(), B_EID, "immutable lost");
    }

    function test_Idempotent_SkipsRedeploy() public {
        address impl = address(new IntexNFT1155());
        bytes memory initData = abi.encodeCall(IntexNFT1155.initialize, (admin, bridger));
        address first = Create3Deploy.deployProxy(factory, address(this), "IntexNFT1155", VERSION, impl, initData);
        // Second call must not revert and must return the same proxy.
        address second = Create3Deploy.deployProxy(factory, address(this), "IntexNFT1155", VERSION, impl, initData);
        assertEq(first, second, "redeploy not idempotent");
    }

    function test_ProxyAddressIndependentOfImpl() public {
        address predicted = Create3Deploy.predictProxy(factory, address(this), "AddrTest", VERSION);

        uint256 snap = vm.snapshotState();
        address a =
            Create3Deploy.deployProxy(factory, address(this), "AddrTest", VERSION, address(new IntexNFT1155()), "");
        vm.revertToState(snap);
        // Different implementation bytecode, same salt + deployer -> same proxy address.
        address b =
            Create3Deploy.deployProxy(factory, address(this), "AddrTest", VERSION, address(new IntexAuction()), "");

        assertEq(a, predicted, "a != predicted");
        assertEq(b, predicted, "b != predicted");
    }
}
