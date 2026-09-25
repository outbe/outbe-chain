// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IntexMetadata} from "@contracts/shared/libs/IntexMetadata.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {CreateSeriesLib} from "./helpers/CreateSeriesLib.sol";
import {MetadataTestLib} from "./helpers/MetadataTestLib.sol";
import {DateKey} from "@contracts/shared/libs/DateKey.sol";
import {MockVwapSource} from "@test-mocks/MockVwapSource.sol";
import {IVwapSource} from "@contracts/shared/interfaces/IVwapSource.sol";
import {IIntexFactory} from "@precompiles/IIntexFactory.sol";

/// @notice Per-token on-chain metadata: JSON document, attributes, and embedded SVG.
contract IntexNFT1155MetadataTest is Test {
    using MetadataTestLib for bytes;

    uint32 internal constant SERIES_ID_DAY = 20260622;
    bytes14 internal constant SERIES_ID = "20260622-USD-U";
    uint32 internal constant CAP = 10_000;
    uint32 internal constant CALL_PERIOD = 14 days;

    // One 840-unit per COEN on the 1e6 wire scale, with the protocol's 1.08x floor and 2.28x call.
    uint64 internal constant ENTRY_PRICE = 1e6;
    uint64 internal constant FLOOR_PRICE = (ENTRY_PRICE * 108) / 100;
    uint64 internal constant CALL_PRICE = (ENTRY_PRICE * 228) / 100;
    uint128 internal constant PROMIS_LOAD = 100_000e6;

    address internal admin = makeAddr("admin");
    address internal bridger = makeAddr("bridger");
    address internal user = makeAddr("user");
    address internal user2 = makeAddr("user2");

    IntexNFT1155 internal token;
    uint256 internal iTok;
    uint256 internal sTok;

    function setUp() public {
        token = DeployProxy.intexNFT1155(admin, bridger);
        bytes32 settlementRole = token.SETTLEMENT_ROLE();
        vm.prank(admin);
        token.grantRole(settlementRole, bridger);
        IIntexNFT1155.CreateSeriesParams memory params = CreateSeriesLib.params(SERIES_ID_DAY, CAP, CALL_PERIOD);
        params.entryPriceMinor = ENTRY_PRICE;
        params.floorPriceMinor = FLOOR_PRICE;
        params.callPriceMinor = CALL_PRICE;
        params.promisLoadMinor = PROMIS_LOAD;
        vm.prank(bridger);
        token.createSeries(params);
        vm.prank(bridger);
        token.issue(user, 10, SERIES_ID);
        (iTok, sTok) = token.tokenIds(SERIES_ID);
    }

    function _json(uint256 tokenId) internal view returns (bytes memory) {
        return MetadataTestLib.decodeJsonDataUri(token.uri(tokenId));
    }

    function _assertContains(bytes memory json, string memory needle) internal pure {
        assertTrue(json.contains(bytes(needle)), needle);
    }

    /// @dev Rows are laid out by a cursor, so the y is what a reordering breaks.
    function _assertRowAt(bytes memory svg, string memory label, uint256 y) internal view {
        assertTrue(
            svg.contains(
                bytes(
                    string.concat(
                        "<text x=\"60\" y=\"",
                        vm.toString(y),
                        "\" font-family=\"sans-serif\" font-size=\"20\" fill=\"#999\">",
                        label,
                        "</text>"
                    )
                )
            ),
            label
        );
    }

    function test_nameAndSymbol_NameTheCollection() public view {
        assertEq(token.name(), "Intex");
        assertEq(token.symbol(), "INTEX");
    }

    function test_uri_IssuedToken_RendersIdentity() public view {
        bytes memory json = _json(iTok);
        _assertContains(json, string.concat("\"name\":\"Intex ", string(abi.encodePacked(SERIES_ID)), "\","));
        _assertContains(
            json,
            "\"description\":\"Unsettled units of Intex series 20260622-USD-U from the Worldwide Day 20260622 auction."
        );
        _assertContains(json, "{\"trait_type\":\"Token Status\",\"value\":\"Issued\"}");
        _assertContains(json, "{\"trait_type\":\"Series State\",\"value\":\"Issued\"}");
        _assertContains(json, "{\"trait_type\":\"Worldwide Day\",\"value\":20260622,\"display_type\":\"number\"}");
        _assertContains(json, "{\"trait_type\":\"Issuance Currency\",\"value\":840,\"display_type\":\"number\"}");
        _assertContains(json, "{\"trait_type\":\"Reference Currency\",\"value\":840,\"display_type\":\"number\"}");
        // Six fraction digits with trailing zeros trimmed, decoded from the 1e6 wire scale.
        _assertContains(json, "{\"trait_type\":\"Entry Price\",\"value\":1,\"display_type\":\"number\"}");
        _assertContains(json, "{\"trait_type\":\"Floor Price\",\"value\":1.08,\"display_type\":\"number\"}");
        _assertContains(json, "{\"trait_type\":\"Call Price\",\"value\":2.28,\"display_type\":\"number\"}");
        _assertContains(json, "{\"trait_type\":\"Promis Load\",\"value\":100000,\"display_type\":\"number\"}");
        assertFalse(json.contains("\"Called At\""), "no call rows before markCalled");
        assertFalse(json.contains("\"Call Deadline\""), "no call rows before markCalled");
    }

    function test_uri_ExcludesRetiredTraits() public view {
        bytes memory json = _json(iTok);
        assertFalse(json.contains("Total Supply"), "supply is per-chain local, not rendered");
        assertFalse(json.contains("Issued Intex Count"), "cap is a series parameter, not a trait");
        assertFalse(json.contains("Issued At"), "worldwide day carries the date semantics");
        assertFalse(json.contains("Series ID"), "composite id lives in the name");
        assertFalse(json.contains("Cost Amount"), "cost is derived at settlement, not published");
    }

    /// @dev Prices the series' first full day only, so a read from any other day finds nothing.
    function _pointAtSource(uint256 price) internal returns (MockVwapSource source) {
        source = new MockVwapSource();
        source.set(DateKey.firstFullDay(token.readData(SERIES_ID).issuedAt), price);
        vm.prank(admin);
        token.setVwapSource(address(source));
    }

    function test_uri_Qualified_OnceADayClosesAboveTheFloor() public {
        _pointAtSource(FLOOR_PRICE + 1);
        bytes memory json = _json(iTok);
        _assertContains(json, "{\"trait_type\":\"Series State\",\"value\":\"Qualified\"}");
        bytes memory svg = json.decodeSvg();
        assertTrue(svg.contains("QUALIFIED"), "badge text");
        assertTrue(svg.contains("#16a34a"), "badge color");
        assertFalse(svg.contains("Floor Price"), "floor price shows only while issued");
        _assertRowAt(svg, "Call Price", 355);
    }

    function test_uri_Issued_WhileNoDayClosedAboveTheFloor() public {
        _pointAtSource(FLOOR_PRICE);
        _assertContains(_json(iTok), "{\"trait_type\":\"Series State\",\"value\":\"Issued\"}");
    }

    function test_uri_Issued_WhenTheSourceFailsOrIsUnset() public {
        string memory issued = "{\"trait_type\":\"Series State\",\"value\":\"Issued\"}";
        MockVwapSource source = _pointAtSource(FLOOR_PRICE + 1);
        source.setReverts(true);
        _assertContains(_json(iTok), issued);

        vm.prank(admin);
        token.setVwapSource(makeAddr("no code"));
        _assertContains(_json(iTok), issued);

        vm.prank(admin);
        token.setVwapSource(address(0));
        _assertContains(_json(iTok), issued);
    }

    function test_uri_Called_WhateverTheSourceSays() public {
        _pointAtSource(FLOOR_PRICE + 1);
        vm.prank(bridger);
        token.markCalled(SERIES_ID, uint32(block.timestamp));
        _assertContains(_json(iTok), "{\"trait_type\":\"Series State\",\"value\":\"Called\"}");
    }

    function test_isQualified_ReadsTheSameSourceTheCardDoes() public {
        assertFalse(token.isQualified(SERIES_ID), "no source, no qualification");
        MockVwapSource source = _pointAtSource(FLOOR_PRICE + 1);
        assertTrue(token.isQualified(SERIES_ID));

        source.set(DateKey.firstFullDay(token.readData(SERIES_ID).issuedAt), FLOOR_PRICE);
        assertFalse(token.isQualified(SERIES_ID), "a day at the floor does not qualify");

        source.setReverts(true);
        assertFalse(token.isQualified(SERIES_ID), "a failing source answers no rather than reverting");
    }

    function test_TheIntexFactoryAnswersAsAVwapSource() public pure {
        assertEq(IIntexFactory.maxUtcDayVwapSince.selector, IVwapSource.maxUtcDayVwapSince.selector);
    }

    function test_setVwapSource_OnlyAdmin() public {
        address source = makeAddr("source");
        vm.expectRevert();
        vm.prank(user);
        token.setVwapSource(source);

        vm.expectEmit(address(token));
        emit IIntexNFT1155.VwapSourceSet(source);
        vm.prank(admin);
        token.setVwapSource(source);
        assertEq(token.vwapSource(), source);
    }

    function test_uri_Called_AddsCallTimestamps() public {
        vm.prank(bridger);
        token.markCalled(SERIES_ID, uint32(block.timestamp));
        uint256 calledAt = block.timestamp;

        bytes memory json = _json(iTok);
        _assertContains(json, "{\"trait_type\":\"Series State\",\"value\":\"Called\"}");
        _assertContains(
            json,
            string.concat(
                "{\"trait_type\":\"Called At\",\"value\":", vm.toString(calledAt), ",\"display_type\":\"date\"}"
            )
        );
        _assertContains(
            json,
            string.concat(
                "{\"trait_type\":\"Call Deadline\",\"value\":",
                vm.toString(calledAt + CALL_PERIOD),
                ",\"display_type\":\"date\"}"
            )
        );
        bytes memory svg = json.decodeSvg();
        assertTrue(svg.contains("CALLED"), "badge text");
        assertTrue(svg.contains("#f97316"), "badge color");
        // calledAt == 1, deadline == 1 + 14 days == 1970-01-15 00:00:01 UTC.
        assertFalse(svg.contains("Floor Price"), "floor price shows only while issued");
        _assertRowAt(svg, "Call Price", 355);
        _assertRowAt(svg, "Call Deadline", 400);
        assertTrue(svg.contains("15.01.1970 00:00 UTC"), "deadline date formatting");
    }

    function test_uri_Expired_DerivedFromClock() public {
        vm.prank(bridger);
        token.markCalled(SERIES_ID, uint32(block.timestamp));
        uint256 deadline = block.timestamp + CALL_PERIOD;

        vm.warp(deadline);
        _assertContains(_json(iTok), "{\"trait_type\":\"Series State\",\"value\":\"Called\"}");

        vm.warp(deadline + 1);
        bytes memory json = _json(iTok);
        _assertContains(json, "{\"trait_type\":\"Series State\",\"value\":\"Expired\"}");
        _assertContains(json, "\"Call Deadline\"");
        bytes memory svg = json.decodeSvg();
        assertTrue(svg.contains("EXPIRED"), "badge text");
        assertTrue(svg.contains("#6b7280"), "badge color");
        assertFalse(svg.contains("Floor Price"), "floor price shows only while issued");
        _assertRowAt(svg, "Call Deadline", 400);
    }

    function test_uri_SettledToken_SuffixAndNoLifecycle() public {
        vm.prank(bridger);
        token.settleIntex(SERIES_ID, user, user2, 3);

        bytes memory json = _json(sTok);
        _assertContains(json, string.concat("\"name\":\"Intex ", string(abi.encodePacked(SERIES_ID)), " - Settled\","));
        _assertContains(json, "{\"trait_type\":\"Token Status\",\"value\":\"Settled\"}");
        _assertContains(json, "{\"trait_type\":\"Worldwide Day\",\"value\":20260622,\"display_type\":\"number\"}");
        _assertContains(json, "{\"trait_type\":\"Entry Price\",\"value\":1,\"display_type\":\"number\"}");
        _assertContains(json, "\"description\":\"Settled units of Intex series 20260622-USD-U.");
        assertFalse(json.contains("Series State"), "lifecycle is final for the Settled class");
        assertFalse(json.contains("\"Called At\""), "no call rows on Settled");
        bytes memory svg = json.decodeSvg();
        assertTrue(svg.contains("SETTLED"), "badge text");
        assertTrue(svg.contains("#a855f7"), "badge color");
        assertFalse(svg.contains("Floor Price"), "floor price shows only while issued");
    }

    function test_uri_SettledToken_RendersBeforeAnySettle() public view {
        bytes memory json = _json(sTok);
        _assertContains(json, string.concat("\"name\":\"Intex ", string(abi.encodePacked(SERIES_ID)), " - Settled\","));
        _assertContains(json, "{\"trait_type\":\"Token Status\",\"value\":\"Settled\"}");
    }

    function test_uri_UnknownToken_FallsBackToCollection() public view {
        assertEq(token.uri(0xdead), token.contractURI());
    }

    function test_tokenURI_SettledClassWithoutRecord_FallsBackToCollection() public view {
        IIntexNFT1155.SeriesData memory missing;
        assertEq(IntexMetadata.tokenURI(missing, true), token.contractURI());
    }

    /// @dev A currency the oracle has no letters for keeps its digits in the id.
    function test_tokenURI_RendersTheNumericFallbackId() public view {
        IIntexNFT1155.SeriesData memory data;
        data.worldwideDay = SERIES_ID_DAY;
        data.seriesId = "20260622-949-U";
        data.issuanceCurrency = 949;
        data.referenceCurrency = 840;
        data.issuedAt = 1;
        bytes memory json = MetadataTestLib.decodeJsonDataUri(IntexMetadata.tokenURI(data, false));
        _assertContains(json, "\"name\":\"Intex 20260622-949-U\",");
    }

    function test_contractURI_CollectionDocument() public view {
        bytes memory json = MetadataTestLib.decodeJsonDataUri(token.contractURI());
        assertEq(
            string(json),
            string.concat("{\"name\":\"Intex\",\"description\":\"", IntexMetadata.COLLECTION_DESCRIPTION, "\"}")
        );
    }

    function test_svg_FormatsHumanValues() public view {
        bytes memory svg = _json(iTok).decodeSvg();
        assertTrue(svg.contains(">INTEX</text>"), "header");
        assertTrue(svg.contains(bytes(abi.encodePacked(SERIES_ID))), "composite id");
        assertTrue(svg.contains(">1</text>"), "entry price");
        assertTrue(svg.contains(">2.28</text>"), "call price");
        assertTrue(svg.contains(">100,000</text>"), "promis load as whole units with separators");
        assertTrue(svg.contains(">1.08</text>"), "floor price while issued");
        _assertRowAt(svg, "Promis Load", 265);
        _assertRowAt(svg, "Entry Price", 310);
        _assertRowAt(svg, "Floor Price", 355);
        _assertRowAt(svg, "Call Price", 400);
        assertFalse(svg.contains("Call Deadline"), "no deadline row before call");
    }

    function test_tokenURI_TrimsFractionAndKeepsWholeAmounts() public view {
        IIntexNFT1155.SeriesData memory data;
        data.worldwideDay = SERIES_ID_DAY;
        data.issuedAt = 1;
        data.entryPriceMinor = 12e6; // whole units render without a decimal point
        data.floorPriceMinor = 1; // smallest representable six-decimal value
        data.callPriceMinor = 1_234; // 0.001234 on the six-decimal wire
        bytes memory json = MetadataTestLib.decodeJsonDataUri(IntexMetadata.tokenURI(data, false));
        _assertContains(json, "{\"trait_type\":\"Entry Price\",\"value\":12,\"display_type\":\"number\"}");
        _assertContains(json, "{\"trait_type\":\"Floor Price\",\"value\":0.000001,\"display_type\":\"number\"}");
        _assertContains(json, "{\"trait_type\":\"Call Price\",\"value\":0.001234,\"display_type\":\"number\"}");

        data.entryPriceMinor = 0;
        json = MetadataTestLib.decodeJsonDataUri(IntexMetadata.tokenURI(data, false));
        _assertContains(json, "{\"trait_type\":\"Entry Price\",\"value\":0,\"display_type\":\"number\"}");
    }
}
