// SPDX-License-Identifier: MIT
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {CreateSeriesLib} from "./helpers/CreateSeriesLib.sol";

/// @notice On-chain token and collection metadata (no image).
contract IntexNFT1155MetadataTest is Test {
    IntexNFT1155 private token;
    address private admin = address(this);
    uint32 private constant SERIES_ID = 20260401;
    uint256 private constant TOKEN_ID = uint256(SERIES_ID);
    string private constant DESC = "Intex collection.";

    function setUp() public {
        token = DeployProxy.intexNFT1155(admin, admin);
        token.createSeries(CreateSeriesLib.params(SERIES_ID, 10_000, 0));
        token.setCollectionMetadata(DESC);
    }

    function test_uri_returnsOnChainJson() public view {
        string memory expected = string.concat('data:application/json,{"name":"Intex","description":"', DESC, '"}');
        assertEq(token.uri(TOKEN_ID), expected);
    }

    function test_contractURI_returnsCollectionJson() public view {
        string memory expected =
            string.concat('data:application/json,{"name":"Intex","description":"', DESC, '"}');
        assertEq(token.contractURI(), expected);
    }
}
