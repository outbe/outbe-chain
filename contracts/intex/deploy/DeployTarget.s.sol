// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {Create3Factory} from "@shared/Create3Factory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";

/// @title DeployTarget
/// @author Outbe
/// @notice Deploy the auction target stack on one chain: the NFT collection + bridge,
///         EscrowAdapter, IntexAuction and IntexAuctionTargetRouter. The deploy is uniform for every target,
///         including the origin chain as a loopback target (origin==target). There, the shared
///         NFT/bridge result from the idempotent CREATE3 deploy. The bridge meshes only with OTHER
///         targets, so it never self-peers.
/// @dev Env: DEPLOYER_PK, BRIDGE_ADDRESS, ORIGIN_CHAIN_ID (Outbe), TARGET_CHAIN_IDS
///      (comma-separated, for the NFT-bridge mesh), WCOEN_BRIDGE (the token bridge of the proceeds). The deployer is admin + delegate. The auction gets its collection and
///      the issue role here. ConfigureTarget wires the rest (escrow/compact, router and bridge roles).
///      Peers are CREATE3-deterministic across chains.
contract DeployTarget is BaseScript {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PK");
        address deployer = vm.addr(pk);
        address admin = deployer;
        address delegate = deployer;
        address bridge = vm.envAddress("BRIDGE_ADDRESS");
        uint32 originChainId = uint32(vm.envUint("ORIGIN_CHAIN_ID"));
        address wcoenBridge = vm.envAddress("WCOEN_BRIDGE");
        uint256[] memory targetChainIds = vm.envUint("TARGET_CHAIN_IDS", ",");
        uint32 local = uint32(block.chainid);

        Create3Factory factory = create3Factory();
        // HandOver.s.sol moves the IntexAuctionTargetRouter last, so a handed-over router means the whole stack is.
        if (handedOver(predictProxy(factory, deployer, "IntexAuctionTargetRouter"), deployer)) {
            console.log("Create3Factory:", address(factory));
            console.log("IntexNFT1155:", predictProxy(factory, deployer, "IntexNFT1155"));
            console.log("EscrowAdapter:", predictProxy(factory, deployer, "EscrowAdapter"));
            console.log("IntexAuction:", predictProxy(factory, deployer, "IntexAuction"));
            console.log("IntexNFT1155Router:", predictProxy(factory, deployer, "IntexNFT1155Router"));
            console.log("IntexAuctionTargetRouter:", predictProxy(factory, deployer, "IntexAuctionTargetRouter"));
            console.log("Handed over to governance - skipping.");
            return;
        }

        vm.startBroadcast(pk);

        // Shared NFT collection + bridge: on a remote target, these deploy here. On the origin
        // loopback target, they already exist on this chain, so idempotent deployProxy returns the
        // existing ones.
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
        address nftRouter = deployProxy(
            factory,
            deployer,
            "IntexNFT1155Router",
            address(new IntexNFT1155Router(nft, bridge)),
            abi.encodeCall(IntexNFT1155Router.initialize, (delegate))
        );
        address router = deployProxy(
            factory,
            deployer,
            "IntexAuctionTargetRouter",
            address(new IntexAuctionTargetRouter(bridge, originChainId, wcoenBridge)),
            abi.encodeCall(IntexAuctionTargetRouter.initialize, (delegate))
        );
        requireDeployerAdmin(nft, deployer);
        requireDeployerAdmin(escrow, deployer);
        requireDeployerAdmin(auction, deployer);
        requireDeployerAdmin(nftRouter, deployer);
        requireDeployerAdmin(router, deployer);

        // Winners issue their own units through the auction.
        if (address(IntexAuction(auction).intexContract()) != nft) IntexAuction(auction).setIntexContract(nft);
        bytes32 issueRole = IntexNFT1155(nft).AUCTION_ROLE();
        if (!IntexNFT1155(nft).hasRole(issueRole, auction)) IntexNFT1155(nft).grantRole(issueRole, auction);

        // Mesh the NFT bridge with every OTHER target's bridge (all at the same CREATE3 address).
        // Skipping `local` means the origin loopback target never self-peers the bridge.
        address bridgePeer = predictProxy(factory, deployer, "IntexNFT1155Router");
        for (uint256 i = 0; i < targetChainIds.length; i++) {
            uint32 cid = uint32(targetChainIds[i]);
            if (cid == local) continue;
            IntexNFT1155Router(payable(nftRouter))
                .setRemoteRouter(cid, InteroperableAddress.formatEvmV1(cid, bridgePeer));
        }

        // The escrow hands finalized proceeds to the router, which sends them to IntexFactory on Outbe.
        EscrowAdapter(payable(escrow)).setProceedsRecipient(router);

        vm.stopBroadcast();

        console.log("Create3Factory:", address(factory));
        console.log("IntexNFT1155:", nft);
        console.log("EscrowAdapter:", escrow);
        console.log("IntexAuction:", auction);
        console.log("IntexNFT1155Router:", nftRouter);
        console.log("IntexAuctionTargetRouter:", router);
    }
}
