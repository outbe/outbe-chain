// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";

import {Create3Factory} from "@shared/Create3Factory.sol";
import {Create3Proxy} from "@shared/Create3Proxy.sol";
import {SALT_VERSION} from "@shared/SaltVersion.sol";
import {Auction} from "../src/Auction.sol";
import {IntentCreate3} from "./IntentCreate3.sol";

/// @dev Deployment script for standalone Auction contract, a UUPS proxy at a CREATE3 address. A
///      proxy that already exists is kept.
///
/// Required env vars:
///   DEPLOYER_PK      - deployer private key
///   CREATE3_FACTORY_ADDRESS - deployed Create3Factory (from contracts/shared)
///   CONTRACT_SALT    - salt string for deterministic deployment (default SALT_VERSION)
///   AUCTION_OWNER    - auction contract owner (admin)
contract DeployAuction is Script {
    function run() public virtual {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        address factory = vm.envAddress("CREATE3_FACTORY_ADDRESS");
        string memory salt = vm.envOr("CONTRACT_SALT", SALT_VERSION);
        address auctionOwner = vm.envAddress("AUCTION_OWNER");

        vm.startBroadcast(deployerPrivateKey);
        address auction = deployAuction(factory, salt, vm.addr(deployerPrivateKey), auctionOwner);
        vm.stopBroadcast();

        console2.log("Auction deployed at:", auction);
    }

    /// @dev `deployer` sends the factory call.
    function deployAuction(address factory, string memory salt, address deployer, address auctionOwner)
        public
        returns (address auction)
    {
        auction = IntentCreate3.predict(factory, deployer, IntentCreate3.AUCTION, salt);
        if (auction.code.length == 0) {
            Create3Proxy.deploy(
                Create3Factory(factory),
                IntentCreate3.salt(IntentCreate3.AUCTION, salt),
                address(new Auction()),
                abi.encodeCall(Auction.initialize, (auctionOwner))
            );
        }
        console2.log("  Auction owner:", Auction(auction).owner());
    }
}
