// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {Create3Factory} from "@contracts/factory/Create3Factory.sol";

/// @title PredictAddresses
/// @author Outbe
/// @notice Print the CREATE3 addresses a given deployer and salt version produce, without
///         deploying anything. CREATE3 keys an address on (factory, deployer, salt) alone, so
///         these hold before any contract exists.
/// @dev Env: DEPLOYER_PRIVATE_KEY, optional SALT_VERSION (blank uses the production default).
///      Run against a chain: the factory answers the prediction, and is deployed first if absent.
contract PredictAddresses is BaseScript {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);

        vm.startBroadcast(pk);
        Create3Factory factory = ensureCreate3Factory();
        vm.stopBroadcast();

        console.log("salt version:  ", saltVersion());
        console.log("deployer:      ", deployer);
        console.log("Create3Factory:", address(factory));
        console.log("OriginRouter:  ", predictProxy(factory, deployer, "OriginRouter"));
        console.log("IntexNFT1155:  ", predictProxy(factory, deployer, "IntexNFT1155"));
        console.log("TargetRouter:  ", predictProxy(factory, deployer, "TargetRouter"));
    }
}
