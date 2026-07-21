// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {BnbVaultProvider} from "src/BnbVaultProvider.sol";

/// @notice Deploys the fixed BNB WCOEN vault adapter.
/// @dev Required env: PRIVATE_KEY, DEPLOYER_ADDRESS, BRIDGE_ADDRESS, BSC_CHAIN_ID,
///      OUTBE_CHAIN_ID, BSC_WCOEN_TOKEN, BSC_WCOEN_BRIDGE, BNB_WCOEN_VAULT.
///      Optional env: OUTBE_PROVIDER (default 0x1017), PROVIDER_OWNER (default DEPLOYER_ADDRESS).
contract DeployBnbVaultProvider is Script {
    address internal constant OUTBE_VAULT_PROVIDER_PRECOMPILE = 0x0000000000000000000000000000000000001017;

    function run() external returns (BnbVaultProvider provider) {
        uint256 privateKey = vm.parseUint(vm.envString("PRIVATE_KEY"));
        address deployer = vm.addr(privateKey);
        address configuredDeployer = vm.envAddress("DEPLOYER_ADDRESS");
        uint256 bscChainId = vm.envUint("BSC_CHAIN_ID");
        uint256 outbeChainId = vm.envUint("OUTBE_CHAIN_ID");
        require(outbeChainId <= type(uint32).max, "OUTBE_CHAIN_ID exceeds uint32");

        address asset = vm.envAddress("BSC_WCOEN_TOKEN");
        address vault = vm.envAddress("BNB_WCOEN_VAULT");
        address tokenBridge = vm.envAddress("BSC_WCOEN_BRIDGE");
        address messageBridge = vm.envAddress("BRIDGE_ADDRESS");
        address outbeProvider = vm.envOr("OUTBE_PROVIDER", OUTBE_VAULT_PROVIDER_PRECOMPILE);
        address owner = vm.envOr("PROVIDER_OWNER", configuredDeployer);

        require(block.chainid == bscChainId, "wrong destination chain");
        require(deployer == configuredDeployer, "PRIVATE_KEY != DEPLOYER_ADDRESS");

        vm.startBroadcast(privateKey);
        provider =
            new BnbVaultProvider(asset, vault, tokenBridge, messageBridge, uint32(outbeChainId), outbeProvider, owner);
        vm.stopBroadcast();

        console2.log("BnbVaultProvider:", address(provider));
        console2.log("WCOEN:", asset);
        console2.log("1:1 vault:", vault);
        console2.log("WCOEN token bridge:", tokenBridge);
        console2.log("ERC-7786 message bridge:", messageBridge);
        console2.log("trusted Outbe chain:", outbeChainId);
        console2.log("trusted Outbe provider:", outbeProvider);
        console2.log("owner:", owner);
    }
}
