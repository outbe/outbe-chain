// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Script} from "forge-std/Script.sol";
import {console2} from "forge-std/console2.sol";
import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

import {Auction} from "../src/Auction.sol";

/// @dev Deployment script for standalone Auction contract, a UUPS proxy.
///
/// Required env vars:
///   DEPLOYER_PK      - deployer private key
///   AUCTION_OWNER    - auction contract owner (admin)
contract DeployAuction is Script {
    function run() public virtual {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        address auctionOwner = vm.envAddress("AUCTION_OWNER");

        vm.startBroadcast(deployerPrivateKey);
        address auction = deployAuction(auctionOwner);
        vm.stopBroadcast();

        console2.log("Auction deployed at:", auction);
    }

    function deployAuction(address auctionOwner) public returns (address) {
        Auction auction = Auction(
            address(new ERC1967Proxy(address(new Auction()), abi.encodeCall(Auction.initialize, (auctionOwner))))
        );
        console2.log("  Auction owner:", auctionOwner);
        return address(auction);
    }
}
