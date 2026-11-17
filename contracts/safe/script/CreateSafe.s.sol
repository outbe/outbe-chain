// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {ISafeProxyFactory, SafeScript} from "./SafeScript.sol";

/// @dev Creates a Safe wallet from the deployed Safe infrastructure. Any funded account can run it.
contract CreateSafe is Script, SafeScript {
    function run() external {
        uint256 callerKey = vm.envUint("DEPLOYER_PK");
        address singleton = vm.envAddress("SAFE_SINGLETON");
        ISafeProxyFactory proxyFactory = ISafeProxyFactory(vm.envAddress("SAFE_PROXY_FACTORY"));
        address fallbackHandler = vm.envAddress("SAFE_FALLBACK_HANDLER");
        address[] memory owners = vm.envAddress("SAFE_OWNERS", ",");
        uint256 threshold = vm.envUint("SAFE_THRESHOLD");
        uint256 nonce = vm.envOr("SAFE_NONCE", uint256(0));

        require(block.chainid == vm.envUint("OUTBE_CHAIN_ID"), "Unexpected chain id");
        require(singleton.code.length != 0, "Safe singleton is missing");
        require(address(proxyFactory).code.length != 0, "Safe proxy factory is missing");
        require(fallbackHandler.code.length != 0, "Safe fallback handler is missing");
        _validateConfig(owners, threshold);

        bytes memory initializer = _initializer(owners, threshold, fallbackHandler);
        address safe = _safeAddress(proxyFactory, singleton, initializer, nonce);

        if (safe.code.length == 0) {
            vm.startBroadcast(callerKey);
            safe = address(proxyFactory.createChainSpecificProxyWithNonceL2(singleton, initializer, nonce));
            vm.stopBroadcast();
        }

        _validateDeployment(safe, singleton, owners, threshold);
        console2.log("SAFE_ADDRESS:", safe);
    }
}
