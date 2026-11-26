// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {console} from "forge-std/console.sol";
import {IntexCreate3} from "./IntexCreate3.sol";
import {IntexScript} from "./IntexScript.sol";

/// @dev Prints the CREATE3 address of every Intex proxy for the current salt. The node pins two of
///      them as constants, so they have to be known before the deploy rather than read after it.
///
/// Env: `DEPLOYER_PK` (the deployer namespaces the salt), `CREATE3_FACTORY_ADDRESS`, `CONTRACT_SALT`.
contract PredictAddresses is IntexScript {
    function run() external view {
        address deployer = vm.addr(vm.envUint("DEPLOYER_PK"));
        IntexCreate3.Stack memory s = stack(deployer);

        console.log("salt version:", saltVersion());
        console.log("deployer:    ", deployer);
        console.log("factory:     ", create3Factory());
        console.log(IntexCreate3.INTEX_NFT1155, s.intexNFT1155);
        console.log(IntexCreate3.INTEX_NFT1155_ROUTER, s.intexNFT1155Router);
        console.log(IntexCreate3.ESCROW_ADAPTER, s.escrowAdapter);
        console.log(IntexCreate3.INTEX_AUCTION, s.intexAuction);
        console.log(IntexCreate3.INTEX_AUCTION_TARGET_ROUTER, s.targetRouter);
    }
}
