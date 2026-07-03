// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {Create3Factory} from "@contracts/factory/Create3Factory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155Bridge} from "@contracts/shared/IntexNFT1155Bridge.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";

/// @title DeployBsc
/// @author Outbe
/// @notice Deploy the BNB-side intex contracts as UUPS proxies through the CREATE3 factory.
/// @dev Env: DEPLOYER_PRIVATE_KEY, BRIDGE_ADDRESS (the ERC-7786 bridge all clients speak to), OUTBE_CHAIN_ID
///      (Outbe's EVM chainId). The deployer is the admin (DEFAULT_ADMIN_ROLE) and delegate. Registers the
///      Outbe-side peers on each client; app wiring (escrow/compact/vault, roles) is a separate step.
contract DeployBsc is BaseScript {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        // The deployer is admin and delegate.
        address admin = deployer;
        address delegate = deployer;
        address bridge = vm.envAddress("BRIDGE_ADDRESS");
        uint32 outbeChainId = uint32(vm.envUint("OUTBE_CHAIN_ID"));

        vm.startBroadcast(pk);

        Create3Factory factory = ensureCreate3Factory();

        address nft = deployProxy(
            factory,
            deployer,
            "IntexNFT1155",
            address(new IntexNFT1155()),
            abi.encodeCall(IntexNFT1155.initialize, (admin))
        );
        address escrow = deployProxy(
            factory,
            deployer,
            "EscrowAdapter",
            address(new EscrowAdapter()),
            abi.encodeCall(EscrowAdapter.initialize, (admin))
        );
        address auction = deployProxy(
            factory,
            deployer,
            "IntexAuction",
            address(new IntexAuction()),
            abi.encodeCall(IntexAuction.initialize, (admin))
        );
        address nftBridge = deployProxy(
            factory,
            deployer,
            "IntexNFT1155Bridge",
            address(new IntexNFT1155Bridge(nft, bridge)),
            abi.encodeCall(IntexNFT1155Bridge.initialize, (delegate))
        );
        address messenger = deployProxy(
            factory,
            deployer,
            "TargetRouter",
            address(new TargetRouter(bridge, outbeChainId)),
            abi.encodeCall(TargetRouter.initialize, (delegate))
        );

        // Register the Outbe-side peers. Proxy addresses are CREATE3-deterministic across chains, so the
        // Outbe clients are predictable from the same (factory, deployer, salt) before that chain is deployed.
        TargetRouter(payable(messenger))
            .setRemoteMessenger(
                outbeChainId,
                InteroperableAddress.formatEvmV1(outbeChainId, predictProxy(factory, deployer, "OriginRouter"))
            );
        IntexNFT1155Bridge(payable(nftBridge))
            .setRemoteMessenger(
                outbeChainId,
                InteroperableAddress.formatEvmV1(outbeChainId, predictProxy(factory, deployer, "IntexNFT1155Bridge"))
            );

        vm.stopBroadcast();

        console.log("Create3Factory:", address(factory));
        console.log("IntexNFT1155:", nft);
        console.log("EscrowAdapter:", escrow);
        console.log("IntexAuction:", auction);
        console.log("IntexNFT1155Bridge:", nftBridge);
        console.log("TargetRouter:", messenger);
    }
}
