// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";

/// @dev Randomized mints into a fixed series; the cap and parity invariants must always hold.
contract NFT1155CapHandler is Test {
    IntexNFT1155 internal intex;
    uint32 internal seriesId;
    address[] internal bidders;

    constructor(IntexNFT1155 _intex, uint32 _seriesId, address[] memory _bidders) {
        intex = _intex;
        seriesId = _seriesId;
        bidders = _bidders;
    }

    function mint(uint256 bidderSeed, uint256 qtySeed) external {
        address to = bidders[bound(bidderSeed, 0, bidders.length - 1)];
        uint256 qty = bound(qtySeed, 1, 1_000);
        try intex.mint(to, qty, seriesId) {} catch {}
    }
}

contract IntexNFT1155CapInvariantTest is StdInvariant, Test {
    IntexNFT1155 internal intex;
    NFT1155CapHandler internal handler;
    address internal admin = address(this);
    address[] internal bidders;

    uint32 internal constant SERIES_ID = 20250101;
    uint32 internal constant CAP = 10_000;

    function setUp() public {
        intex = new IntexNFT1155(admin, admin);
        intex.createSeries(SERIES_ID, CAP, 0);

        bidders.push(address(0xB1));
        bidders.push(address(0xB2));
        bidders.push(address(0xB3));

        handler = new NFT1155CapHandler(intex, SERIES_ID, bidders);

        bytes4[] memory selectors = new bytes4[](1);
        selectors[0] = NFT1155CapHandler.mint.selector;
        targetSelector(FuzzSelector({addr: address(handler), selectors: selectors}));
        targetContract(address(handler));
    }

    function invariant_supplyCapAndParity() public view {
        uint256 iTok = intex.issuedTokenId(SERIES_ID);
        IIntexNFT1155.SeriesData memory d = intex.readData(SERIES_ID);
        assertLe(d.totalSupply, CAP, "totalSupply exceeds cap");
        assertLe(d.mintedCount, CAP, "mintedCount exceeds cap");
        uint256 sum;
        for (uint256 i = 0; i < bidders.length; i++) {
            sum += intex.balanceOf(bidders[i], iTok);
        }
        assertEq(sum, d.totalSupply, "sum(balanceOf) != totalSupply");
    }
}
