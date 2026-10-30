// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {Create3Proxy} from "@shared/Create3Proxy.sol";
import {ERC7786Bridge} from "src/ERC7786Bridge.sol";

/// @dev Deploys the {ERC7786Bridge} facade as a UUPS proxy via CREATE3, with one adapter as the active gateway.
///      The script derives the adapter address deterministically (CREATE3), so the bridge does not
///      depend on the adapter script's output. The deployer owns the bridge until `4_HandOver` gives
///      it to governance.
///
/// Required env: `DEPLOYER_PK`, `CONTRACT_SALT`, `CREATE3_FACTORY_ADDRESS`.
/// Optional: `ACTIVE_GATEWAY` ("lz" | "hyperlane", default "lz").
contract DeployBridge is Script {
    function run() public virtual {
        uint256 deployerPk = vm.envUint("DEPLOYER_PK");
        string memory salt = vm.envString("CONTRACT_SALT");
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        string memory active = vm.envOr("ACTIVE_GATEWAY", string(""));
        if (bytes(active).length == 0) active = "lz";

        address deployer = vm.addr(deployerPk);
        address gateway = adapterAddress(factory, salt, deployer, active);

        vm.startBroadcast(deployerPk);
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
        address impl = address(new ERC7786Bridge());
        bytes memory init = abi.encodeCall(ERC7786Bridge.initialize, (owner, gateway));
        return Create3Proxy.deploy(Create3Factory(factory), saltHash, impl, init);
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
