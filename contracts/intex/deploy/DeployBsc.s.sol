// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {Create3Factory} from "@contracts/factory/Create3Factory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {EscrowAdapter} from "@contracts/bnb/EscrowAdapter.sol";
import {IntexAuction} from "@contracts/bnb/IntexAuction.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";

/// @title DeployBsc
/// @author Outbe
/// @notice Deploy the BNB-side intex contracts as UUPS proxies through the CREATE3 factory.
/// @dev Env: DEPLOYER_PRIVATE_KEY, LZ_ENDPOINT, OUTBE_EID (the remote endpoint id for the BNB-side
///      LayerZero contracts). The deployer is the admin (DEFAULT_ADMIN_ROLE) and the owner / LZ
///      delegate. Wiring (peers, escrow/compact/vault, roles) is a separate step.
contract DeployBsc is BaseScript {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        // The deployer is admin and owner/LZ delegate.
        address admin = deployer;
        address delegate = deployer;
        address lzEndpoint = vm.envAddress("LZ_ENDPOINT");
        uint32 outbeEid = uint32(vm.envUint("OUTBE_EID"));

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
        address onft = deployProxy(
            factory,
            deployer,
            "ONFT1155Adapter",
            address(new ONFT1155Adapter(nft, lzEndpoint)),
            abi.encodeCall(ONFT1155Adapter.initialize, (delegate))
        );
        address onftBatch = deployProxy(
            factory,
            deployer,
            "ONFT1155AdapterBatch",
            address(new ONFT1155AdapterBatch(nft, lzEndpoint)),
            abi.encodeCall(ONFT1155AdapterBatch.initialize, (delegate))
        );
        address messenger = deployProxy(
            factory,
            deployer,
            "TargetMessenger",
            address(new TargetMessenger(lzEndpoint, outbeEid)),
            abi.encodeCall(TargetMessenger.initialize, (delegate))
        );

        vm.stopBroadcast();

        console.log("Create3Factory:", address(factory));
        console.log("IntexNFT1155:", nft);
        console.log("EscrowAdapter:", escrow);
        console.log("IntexAuction:", auction);
        console.log("ONFT1155Adapter:", onft);
        console.log("ONFT1155AdapterBatch:", onftBatch);
        console.log("TargetMessenger:", messenger);
    }
}
