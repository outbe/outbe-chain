// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity 0.8.30;

import {BaseScript} from "./BaseScript.s.sol";
import {VaultProvider} from "../src/VaultProvider.sol";

contract UpgradeVaultProvider is BaseScript {
    function run() external returns (address newImplementation) {
        address providerProxy = vm.envAddress("VAULT_PROVIDER_ADDRESS");

        require(providerProxy != address(0), "VAULT_PROVIDER_REQUIRED");

        vm.startBroadcast(privateKey);
        newImplementation = address(new VaultProvider());
        VaultProvider(payable(providerProxy)).upgradeToAndCall(newImplementation, bytes(""));
        vm.stopBroadcast();

        printAndWrite(exportLine("VAULT_PROVIDER_ADDRESS", vm.toString(providerProxy)));
        printAndWrite(exportLine("VAULT_PROVIDER_IMPL_ADDRESS", vm.toString(newImplementation)));
    }
}
