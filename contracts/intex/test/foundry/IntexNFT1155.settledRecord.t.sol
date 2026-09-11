// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {CreateSeriesLib} from "./helpers/CreateSeriesLib.sol";

/// @dev The Settled class carries no identity record of its own: its id is the series id with the
///      Settled tag set, so identity resolves to the Issued entry and only the settled supply is
///      stored. These pin that nothing is written under the Settled id, that the class is still
///      recognised without a record, and that a settled position's card does not move with the
///      series' lifecycle. Slots are read raw via `vm.load` - there is no public reader.
contract IntexNFT1155SettledRecordTest is Test {
    uint32 internal constant SERIES_ID_DAY = 20260622;
    bytes14 internal constant SERIES_ID = "20260622-USD-U";
    uint32 internal constant CAP = 10_000;
    uint32 internal constant CALL_PERIOD = 14 days;

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.IntexNFT1155")) - 1)) & ~bytes32(uint256(0xff))
    uint256 internal constant _NFT_STORAGE_SLOT = 0xe941cbaf65abb9f7003c3006add9c5d12ba7e339abdf88d4afd5defeb8932900;
    // `seriesData` mapping is member 1 of the ERC-7201 struct; SeriesData spans 4 slots.
    uint256 internal constant _SERIES_DATA_OFFSET = 1;
    uint256 internal constant _SERIES_DATA_SLOTS = 4;
    // `status` sits at bit 96 of slot 3 (after issuedAt, calledAt, totalSupply).
    uint256 internal constant _STATUS_BIT = 96;

    address internal admin = makeAddr("admin");
    address internal bridger = makeAddr("bridger");
    address internal holder = makeAddr("holder");

    IntexNFT1155 internal nft;
    uint256 internal iTok;
    uint256 internal sTok;

    function setUp() public {
        nft = DeployProxy.intexNFT1155(admin, bridger);
        vm.prank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_DAY, CAP, CALL_PERIOD));
        (iTok, sTok) = nft.tokenIds(SERIES_ID);
    }

    function _recordSlot(uint256 tokenId, uint256 i) internal view returns (bytes32) {
        uint256 base = uint256(keccak256(abi.encode(tokenId, _NFT_STORAGE_SLOT + _SERIES_DATA_OFFSET)));
        return vm.load(address(nft), bytes32(base + i));
    }

    function test_CreateSeries_WritesNoRecordForTheSettledId() public view {
        for (uint256 i = 0; i < _SERIES_DATA_SLOTS; i++) {
            assertEq(_recordSlot(sTok, i), bytes32(0), "the Settled id must own no identity slot");
            assertTrue(_recordSlot(iTok, i) != bytes32(0), "the Issued record carries the identity");
        }

        // The class is read off the id, so it holds even though no record was written for it.
        assertEq(uint8(nft.statusOf(sTok)), uint8(IIntexNFT1155.IntexStatus.Settled));
        // And metadata still resolves: both ids render the same series.
        assertTrue(bytes(nft.uri(sTok)).length > 0, "settled metadata resolves to the series identity");
    }

    function test_SettledCard_DoesNotMoveWithTheSeriesLifecycle() public {
        string memory before = nft.uri(sTok);

        vm.prank(bridger);
        nft.markQualified(SERIES_ID);
        vm.prank(bridger);
        nft.markCalled(SERIES_ID, uint32(block.timestamp));

        assertEq(nft.uri(sTok), before, "a closed position is not moved by later transitions");
        assertEq(uint8(nft.readData(SERIES_ID).state), uint8(IIntexNFT1155.IntexState.Called));
    }

    function test_SettledRecord_BridgeGuardsUnchanged() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeOnSettledForbidden.selector, sTok));
        nft.crosschainMint(holder, sTok, 1);

        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeOnSettledForbidden.selector, sTok));
        nft.crosschainBurn(holder, holder, sTok, 1);
    }
}
