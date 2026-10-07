// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {CreateSeriesLib} from "./helpers/CreateSeriesLib.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";

/// @dev Randomized mints and burns into a fixed series. totalSupply moves both ways, and the parity
///      invariant must always hold.
contract NFT1155SupplyHandler is Test {
    IntexNFT1155 internal intex;
    bytes14 internal seriesId;
    uint256 internal tokenId;
    address[] internal bidders;

    constructor(IntexNFT1155 _intex, bytes14 _seriesId, address[] memory _bidders) {
        intex = _intex;
        seriesId = _seriesId;
        tokenId = _intex.issuedTokenId(_seriesId);
        bidders = _bidders;
    }

    function mint(uint256 bidderSeed, uint256 qtySeed) external {
        address to = bidders[bound(bidderSeed, 0, bidders.length - 1)];
        uint256 qty = bound(qtySeed, 1, 1_000);
        try intex.issueIntex(to, qty, seriesId) {} catch {}
    }

    function burn(uint256 bidderSeed, uint256 qtySeed) external {
        address from = bidders[bound(bidderSeed, 0, bidders.length - 1)];
        uint256 bal = intex.balanceOf(from, tokenId);
        if (bal == 0) return;
        uint256 qty = bound(qtySeed, 1, bal);
        try intex.crosschainBurn(from, from, tokenId, qty) {} catch {}
    }
}

contract IntexNFT1155SupplyInvariantTest is StdInvariant, Test {
    IntexNFT1155 internal intex;
    NFT1155SupplyHandler internal handler;
    address internal admin = address(this);
    address[] internal bidders;

    uint32 internal constant SERIES_ID_DAY = 20250101;
    bytes14 internal constant SERIES_ID = "20250101-USD-U";
    uint32 internal constant ISSUED_UNITS = 10_000;

    function setUp() public {
        intex = DeployProxy.intexNFT1155(admin, admin);
        intex.createSeries(CreateSeriesLib.params(SERIES_ID_DAY, ISSUED_UNITS, 0));

        bidders.push(address(0xB1));
        bidders.push(address(0xB2));
        bidders.push(address(0xB3));

        handler = new NFT1155SupplyHandler(intex, SERIES_ID, bidders);
        // Handler drives mint/crosschainBurn directly; both are RELAYER_ROLE-gated.
        intex.grantRole(intex.RELAYER_ROLE(), address(handler));

        bytes4[] memory selectors = new bytes4[](2);
        selectors[0] = NFT1155SupplyHandler.mint.selector;
        selectors[1] = NFT1155SupplyHandler.burn.selector;
        targetSelector(FuzzSelector({addr: address(handler), selectors: selectors}));
        targetContract(address(handler));
    }

    /// @dev `issuedUnits` never moves, and parity (sum balanceOf == totalSupply) holds.
    function invariant_issuedUnitsAndParity() public view {
        uint256 iTok = intex.issuedTokenId(SERIES_ID);
        IIntexNFT1155.SeriesData memory d = intex.readData(SERIES_ID);
        assertEq(d.issuedUnits, ISSUED_UNITS, "issuedUnits is immutable");
        uint256 sum;
        for (uint256 i = 0; i < bidders.length; i++) {
            sum += intex.balanceOf(bidders[i], iTok);
        }
        assertEq(sum, d.totalSupply, "sum(balanceOf) != totalSupply");
    }
}
