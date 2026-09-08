// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;
import {BaseScript} from "./BaseScript.s.sol";
import {IVaultRouter} from "@precompiles/IVaultRouter.sol";

/// @notice Run as VaultRouter's administrator after deploying the account stack.
contract ConfigureBundleCustody is BaseScript {
    function run() public {
        IVaultRouter router = IVaultRouter(vm.envOr("VAULT_ROUTER_ADDRESS", address(0x1017)));
        address custody = vm.envAddress("BUNDLE_MODULE_PLUGIN_ADDRESS");
        address current = router.bundleCustody();
        if (current == custody) return;
        require(current == address(0), "router custody already bound");
        vm.startBroadcast();
        router.setBundleCustody(custody);
        vm.stopBroadcast();
    }
}
