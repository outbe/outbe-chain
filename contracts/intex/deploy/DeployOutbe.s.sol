// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {Create3Factory} from "@contracts/factory/Create3Factory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";

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
        address onft = deployProxy(
            factory,
            deployer,
            "ONFT1155Adapter",
            address(new ONFT1155Adapter(nft, bridge)),
            abi.encodeCall(ONFT1155Adapter.initialize, (delegate))
        );
        address onftBatch = deployProxy(
            factory,
            deployer,
            "ONFT1155AdapterBatch",
            address(new ONFT1155AdapterBatch(nft, bridge)),
            abi.encodeCall(ONFT1155AdapterBatch.initialize, (delegate))
        );
        address messenger = deployProxy(
            factory,
            deployer,
            "OriginMessenger",
            address(new OriginMessenger(bridge, bnbChainId)),
            abi.encodeCall(OriginMessenger.initialize, (delegate))
        );

        // Register the BNB-side peers. Proxy addresses are CREATE3-deterministic across chains, so the
        // BNB clients are predictable from the same (factory, deployer, salt) before that chain is deployed.
        OriginMessenger(payable(messenger))
            .setRemoteMessenger(
                bnbChainId,
                InteroperableAddress.formatEvmV1(bnbChainId, predictProxy(factory, deployer, "TargetMessenger"))
            );
        ONFT1155Adapter(payable(onft))
            .setRemoteMessenger(
                bnbChainId,
                InteroperableAddress.formatEvmV1(bnbChainId, predictProxy(factory, deployer, "ONFT1155Adapter"))
            );
        ONFT1155AdapterBatch(payable(onftBatch))
            .setRemoteMessenger(
                bnbChainId,
                InteroperableAddress.formatEvmV1(bnbChainId, predictProxy(factory, deployer, "ONFT1155AdapterBatch"))
            );

        vm.stopBroadcast();

        console.log("Create3Factory:", address(factory));
        console.log("IntexNFT1155:", nft);
        console.log("ONFT1155Adapter:", onft);
        console.log("ONFT1155AdapterBatch:", onftBatch);
        console.log("OriginMessenger:", messenger);
    }
}
