// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Whitelist} from "../src/Whitelist.sol";

/// @dev Deploys a {Whitelist} registry. Consumers are pointed at it afterwards through their own
///      `setWhitelist`, so the address is configuration rather than something baked into bytecode -
///      no deterministic address needed, and the registry can be replaced later.
///
/// Required env vars:
///   DEPLOYER_PK       - deployer private key
/// Optional:
///   WHITELIST_OWNER   - registry admin, the only address allowed to add or remove entries
///                       (defaults to the deployer)
///   WHITELIST_INITIAL - csv of addresses to seed the registry with
contract DeployWhitelist is Script {
    function run() public virtual {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        address owner = vm.envOr("WHITELIST_OWNER", vm.addr(deployerPrivateKey));
        address[] memory initial = vm.envOr("WHITELIST_INITIAL", ",", new address[](0));

        vm.startBroadcast(deployerPrivateKey);
        address whitelist = deployWhitelist(owner, initial);
        vm.stopBroadcast();

        console2.log("  owner:", owner);
        console2.log("  seeded entries:", initial.length);
        console2.log("WHITELIST_ADDRESS=", whitelist);
    }

    function deployWhitelist(address owner, address[] memory initial) public returns (address) {
        return address(new Whitelist(owner, initial));
    }
}
