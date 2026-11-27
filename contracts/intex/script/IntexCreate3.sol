// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Create3Factory} from "@shared/Create3Factory.sol";

/// @dev CREATE3 labels of the Intex contracts. Each proxy address depends only on (factory,
///      deployer, label, salt), so every chain gets the same address for each contract, and each
///      contract knows the others before any of them is deployed.
library IntexCreate3 {
    string internal constant INTEX_NFT1155 = "IntexNFT1155";
    string internal constant INTEX_NFT1155_ROUTER = "IntexNFT1155Router";
    string internal constant ESCROW_ADAPTER = "EscrowAdapter";
    string internal constant INTEX_AUCTION = "IntexAuction";
    string internal constant INTEX_AUCTION_TARGET_ROUTER = "IntexAuctionTargetRouter";

    /// @dev The five proxies of one deployer and salt.
    struct Stack {
        address intexNFT1155;
        address intexNFT1155Router;
        address escrowAdapter;
        address intexAuction;
        address intexAuctionTargetRouter;
    }

    /// @dev The factory namespaces this with the caller, so the salt omits the deployer.
    function salt(string memory label, string memory version) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("outbe-intex:", label, ":", version));
    }

    function predict(address factory, address deployer, string memory label, string memory version)
        internal
        view
        returns (address)
    {
        return Create3Factory(factory).predict(deployer, salt(label, version));
    }

    function stack(address factory, address deployer, string memory version) internal view returns (Stack memory) {
        return Stack({
            intexNFT1155: predict(factory, deployer, INTEX_NFT1155, version),
            intexNFT1155Router: predict(factory, deployer, INTEX_NFT1155_ROUTER, version),
            escrowAdapter: predict(factory, deployer, ESCROW_ADAPTER, version),
            intexAuction: predict(factory, deployer, INTEX_AUCTION, version),
            intexAuctionTargetRouter: predict(factory, deployer, INTEX_AUCTION_TARGET_ROUTER, version)
        });
    }

    function proxies(Stack memory s) internal pure returns (address[5] memory) {
        return [s.intexNFT1155, s.intexNFT1155Router, s.escrowAdapter, s.intexAuction, s.intexAuctionTargetRouter];
    }
}
