// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {BnbVaultRouter} from "src/BnbVaultRouter.sol";

/// @notice Deploys the fixed BNB WCOEN vault adapter.
/// @dev Required env: PRIVATE_KEY, DEPLOYER_ADDRESS, BRIDGE_ADDRESS, BSC_CHAIN_ID,
///      OUTBE_CHAIN_ID, BSC_WCOEN_TOKEN, BSC_WCOEN_BRIDGE, BNB_WCOEN_VAULT.
///      Optional env: OUTBE_ROUTER (default 0x1017), ROUTER_OWNER (default DEPLOYER_ADDRESS).
contract DeployBnbVaultRouter is Script {
    address internal constant OUTBE_VAULT_ROUTER_PRECOMPILE = 0x0000000000000000000000000000000000001017;

    function run() external returns (BnbVaultRouter router) {
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
        address outbeRouter = vm.envOr("OUTBE_ROUTER", OUTBE_VAULT_ROUTER_PRECOMPILE);
        address owner = vm.envOr("ROUTER_OWNER", configuredDeployer);

        require(block.chainid == bscChainId, "wrong destination chain");
        require(deployer == configuredDeployer, "PRIVATE_KEY != DEPLOYER_ADDRESS");

        vm.startBroadcast(privateKey);
        router = new BnbVaultRouter(asset, vault, tokenBridge, messageBridge, uint32(outbeChainId), outbeRouter, owner);
        vm.stopBroadcast();

        console2.log("BnbVaultRouter:", address(router));
        console2.log("WCOEN:", asset);
        console2.log("1:1 vault:", vault);
        console2.log("WCOEN token bridge:", tokenBridge);
        console2.log("ERC-7786 message bridge:", messageBridge);
        console2.log("trusted Outbe chain:", outbeChainId);
        console2.log("trusted Outbe router:", outbeRouter);
        console2.log("owner:", owner);
    }
}
