// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {Create3Factory} from "@contracts/factory/Create3Factory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";

/// @title DeployOutbe
/// @author Outbe
/// @notice Deploy the Outbe-side intex contracts as UUPS proxies through the CREATE3 factory.
/// @dev Env: DEPLOYER_PRIVATE_KEY, BRIDGE_ADDRESS (the ERC-7786 bridge all clients speak to), BNB_CHAIN_ID
///      (BNB's EVM chainId). The deployer is the admin (DEFAULT_ADMIN_ROLE) and delegate. Registers the
///      BNB-side peers on each client; app wiring (roles) is a separate step.
contract DeployOutbe is BaseScript {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        // The deployer is admin and delegate.
        address admin = deployer;
        address delegate = deployer;
        address bridge = vm.envAddress("BRIDGE_ADDRESS");
        uint32 bnbChainId = uint32(vm.envUint("BNB_CHAIN_ID"));

        vm.startBroadcast(pk);

        Create3Factory factory = ensureCreate3Factory();

        address nft = deployProxy(
            factory,
            deployer,
            "IntexNFT1155",
            address(new IntexNFT1155()),
            abi.encodeCall(IntexNFT1155.initialize, (admin))
        );
        address nftBridge = deployProxy(
            factory,
            deployer,
            "IntexNFT1155Bridge",
            address(new IntexNFT1155Bridge(nft, bridge)),
            abi.encodeCall(IntexNFT1155Bridge.initialize, (delegate))
        );
        address router = deployProxy(
            factory,
            deployer,
            "OriginRouter",
            address(new OriginRouter(bridge, bnbChainId)),
            abi.encodeCall(OriginRouter.initialize, (delegate))
        );

        // Register the BNB-side peers. Proxy addresses are CREATE3-deterministic across chains, so the
        // BNB clients are predictable from the same (factory, deployer, salt) before that chain is deployed.
        OriginRouter(payable(router))
            .setRemoteMessenger(
                bnbChainId,
                InteroperableAddress.formatEvmV1(bnbChainId, predictProxy(factory, deployer, "TargetRouter"))
            );
        IntexNFT1155Bridge(payable(nftBridge))
            .setRemoteMessenger(
                bnbChainId,
                InteroperableAddress.formatEvmV1(bnbChainId, predictProxy(factory, deployer, "IntexNFT1155Bridge"))
            );

        vm.stopBroadcast();

        console.log("Create3Factory:", address(factory));
        console.log("IntexNFT1155:", nft);
        console.log("IntexNFT1155Bridge:", nftBridge);
        console.log("OriginRouter:", router);
    }
}
