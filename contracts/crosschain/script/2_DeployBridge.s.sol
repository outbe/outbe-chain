// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {console2} from "forge-std/console2.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {ERC7786Bridge} from "src/ERC7786Bridge.sol";
import {DeployBase} from "./DeployBase.sol";

/// @dev Deploys the {ERC7786Bridge} facade via CREATE3, with one adapter as the active gateway.
///      The script derives the adapter address deterministically (CREATE3), so the bridge does not
///      depend on the adapter script's output. The deployer owns the bridge until 4_HandOver gives
///      it to governance.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `CREATE3_FACTORY_ADDRESS`.
/// Optional: `ACTIVE_GATEWAY` ("lz" | "hyperlane", default "lz").
contract DeployBridge is DeployBase {
    function run() public virtual {
        string memory salt = vm.envString("CONTRACT_SALT");
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        address deployer = _deployer();
        string memory active = vm.envOr("ACTIVE_GATEWAY", string(""));
        if (bytes(active).length == 0) active = "lz";

        address gateway = adapterAddress(factory, salt, deployer, active);

        vm.startBroadcast(_pk());
        address bridge = deployBridge(factory, salt, deployer, gateway);
        vm.stopBroadcast();

        console2.log("active gateway:", gateway);
        console2.log("ERC7786Bridge:", bridge);
    }

    function deployBridge(address factory, string memory salt, address owner, address gateway)
        public
        returns (address)
    {
        bytes32 saltHash = keccak256(abi.encodePacked("ERC7786Bridge", salt));
        bytes memory code = abi.encodePacked(type(ERC7786Bridge).creationCode, abi.encode(owner, gateway));
        return Create3Factory(factory).deploy(saltHash, code);
    }

    /// @dev Deterministic CREATE3 address of the adapter selected by `active` ("hyperlane" or "lz").
    function adapterAddress(address factory, string memory salt, address deployer, string memory active)
        public
        view
        returns (address)
    {
        bytes32 gateway = keccak256(bytes(active));
        require(
            gateway == keccak256("lz") || gateway == keccak256("hyperlane"), "ACTIVE_GATEWAY must be lz or hyperlane"
        );
        string memory label = gateway == keccak256("hyperlane") ? "HyperlaneGatewayAdapter" : "LayerZeroGatewayAdapter";
        return Create3Factory(factory).predict(deployer, keccak256(abi.encodePacked(label, salt)));
    }
}
