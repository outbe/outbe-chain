// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {BaseScript} from "./BaseScript.s.sol";
import {Create3Factory} from "@contracts/factory/Create3Factory.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";

/// @title DeployOutbe
/// @author Outbe
/// @notice Deploy the Outbe-side intex contracts as UUPS proxies through the CREATE3 factory.
/// @dev Env: DEPLOYER_PRIVATE_KEY, LZ_ENDPOINT, BNB_EID (the remote endpoint id for the Outbe-side
///      LayerZero contracts). The deployer is the admin (DEFAULT_ADMIN_ROLE) and the owner / LZ
///      delegate. Wiring is separate.
contract DeployOutbe is BaseScript {
    function run() external {
        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        address deployer = vm.addr(pk);
        // The deployer is admin and owner/LZ delegate.
        address admin = deployer;
        address delegate = deployer;
        address lzEndpoint = vm.envAddress("LZ_ENDPOINT");
        uint32 bnbEid = uint32(vm.envUint("BNB_EID"));

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
            "OriginMessenger",
            address(new OriginMessenger(lzEndpoint, bnbEid)),
            abi.encodeCall(OriginMessenger.initialize, (delegate))
        );

        vm.stopBroadcast();

        console.log("Create3Factory:", address(factory));
        console.log("IntexNFT1155:", nft);
        console.log("ONFT1155Adapter:", onft);
        console.log("ONFT1155AdapterBatch:", onftBatch);
        console.log("OriginMessenger:", messenger);
    }
}
