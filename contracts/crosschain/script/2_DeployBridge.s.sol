// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {CreateX} from "./0_DeployCreateX.s.sol";
import {ERC7786Bridge} from "src/ERC7786Bridge.sol";

/// @dev Deploys the {ERC7786Bridge} facade via CREATE3, with one adapter as the active gateway. The adapter address is
///      derived deterministically (CREATE3), so the bridge does not depend on the adapter script's output.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `CREATEX_ADDRESS`, `BRIDGE_OWNER`.
/// Optional: `ACTIVE_GATEWAY` ("lz" | "hyperlane", default "lz").
contract DeployBridge is Script {
    function run() public virtual {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");
        address createX = vm.envAddress("CREATEX_ADDRESS");
        address owner = vm.envAddress("BRIDGE_OWNER");
        string memory active = vm.envOr("ACTIVE_GATEWAY", string("lz"));

        address gateway = adapterAddress(createX, salt, vm.addr(deployerPk), active);

        vm.startBroadcast(deployerPk);
        address bridge = deployBridge(createX, salt, vm.addr(deployerPk), owner, gateway);
        vm.stopBroadcast();

        console2.log("active gateway:", gateway);
        console2.log("ERC7786Bridge:", bridge);
    }

    function deployBridge(address createX, string memory salt, address deployer, address owner, address gateway)
        public
        returns (address)
    {
        bytes32 saltHash = keccak256(abi.encodePacked("ERC7786Bridge", salt, deployer));
        bytes memory code = abi.encodePacked(type(ERC7786Bridge).creationCode, abi.encode(owner, gateway));
        return CreateX(createX).deployCreate3(saltHash, code);
    }

    /// @dev Deterministic CREATE3 address of the adapter selected by `active` ("hyperlane" or "lz").
    function adapterAddress(address createX, string memory salt, address deployer, string memory active)
        public
        view
        returns (address)
    {
        string memory label = keccak256(bytes(active)) == keccak256("hyperlane")
            ? "HyperlaneGatewayAdapter"
            : "LayerZeroGatewayAdapter";
        return CreateX(createX).computeCreate3Address(keccak256(abi.encodePacked(label, salt, deployer)));
    }
}
