// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {CreateSeriesLib} from "./helpers/CreateSeriesLib.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {Test} from "forge-std/Test.sol";

/// @title - issued supply, burnIntex, and the paginated owners getter.
/// @notice Every test here exercises a behavior introduced by the lifecycle/DoS hardening pass.
contract IntexNFT1155SupplyTest is Test {
    IntexNFT1155 nft;

    address admin = address(0xA11CE);
    address bridger = address(0xB81DE);
    address burner = address(0xB0B);
    address ownerA = address(0xA);
    address ownerB = address(0xB);

    uint32 constant SERIES_ID_DAY = 20260401;
    bytes14 constant SERIES_ID = "20260401-USD-U";
    uint256 constant TOKEN_ID = uint256(keccak256(abi.encodePacked(SERIES_ID)));

    uint32 constant CALL_PERIOD = uint32(1 days);

    function setUp() public {
        nft = DeployProxy.intexNFT1155(admin, bridger);
        vm.startPrank(admin);
        nft.grantRole(nft.BURNER_ROLE(), burner);
        vm.stopPrank();
    }

    function _createSeries(uint32 issuedUnits) internal {
        vm.prank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_DAY, issuedUnits, CALL_PERIOD));
    }

    // --- createSeries / issueIntex ---

    function test_CreateSeries_ZeroIssuedCount_Reverts() public {
        vm.prank(bridger);
        vm.expectRevert(IIntexNFT1155.ZeroIssuedUnits.selector);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_DAY, 0, CALL_PERIOD));
    }

    function test_CreateSeries_RejectsAnIssuedAtItCannotHonour() public {
        vm.warp(1_700_000_000);
        IIntexNFT1155.CreateSeriesParams memory params = CreateSeriesLib.params(SERIES_ID_DAY, 100, CALL_PERIOD);

        params.issuedAt = 0;
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.InvalidIssuedAt.selector, uint32(0)));
        nft.createSeries(params);

        params.issuedAt = uint32(block.timestamp) + 1;
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.InvalidIssuedAt.selector, params.issuedAt));
        nft.createSeries(params);
    }

    function test_CreateSeries_KeepsTheOriginsIssuedAt() public {
        vm.warp(1_700_000_000);
        IIntexNFT1155.CreateSeriesParams memory params = CreateSeriesLib.params(SERIES_ID_DAY, 100, CALL_PERIOD);
        params.issuedAt = uint32(block.timestamp) - 3600;

        vm.prank(bridger);
        nft.createSeries(params);

        assertEq(nft.readData(SERIES_ID).issuedAt, params.issuedAt);
    }

    function test_Issue_BeyondIssuedUnits_Succeeds() public {
        uint32 issuedUnits = 100;
        _createSeries(issuedUnits);

        vm.startPrank(bridger);
        nft.issueIntex(ownerA, issuedUnits + 1, SERIES_ID);
        nft.issueIntex(ownerB, 60, SERIES_ID);
        vm.stopPrank();

        assertEq(nft.totalSupply(TOKEN_ID), issuedUnits + 61);
        assertEq(nft.balanceOf(ownerA, TOKEN_ID), issuedUnits + 1);
        assertEq(nft.balanceOf(ownerB, TOKEN_ID), 60);
        assertEq(nft.readData(SERIES_ID).issuedUnits, issuedUnits, "issuedUnits stays the original issuance");
    }

    function test_Issue_PastUint32Supply_Reverts() public {
        _createSeries(100);

        vm.startPrank(bridger);
        nft.crosschainMint(ownerA, TOKEN_ID, type(uint32).max);
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.SupplyCapExceeded.selector,
                SERIES_ID,
                uint256(type(uint32).max) + 1,
                uint256(type(uint32).max)
            )
        );
        nft.issueIntex(ownerB, 1, SERIES_ID);
        vm.stopPrank();

        assertEq(nft.totalSupply(TOKEN_ID), type(uint32).max);
    }

    // --- burnIntex: the precompiles own the series state, so the burn checks none ---

    function _issue(uint256 units, bool called) internal {
        _createSeries(10);
        vm.startPrank(bridger);
        nft.issueIntex(ownerA, units, SERIES_ID);
        if (called) nft.markCalled(SERIES_ID, uint32(block.timestamp));
        vm.stopPrank();
    }

    function test_BurnIntex_BurnsTheOwnersUnitsInAnyState() public {
        _issue(6, true);
        vm.prank(burner);
        nft.burnIntex(ownerA, SERIES_ID, 4);
        assertEq(nft.balanceOf(ownerA, TOKEN_ID), 2);
        assertEq(nft.totalSupply(TOKEN_ID), 2);
    }

    function test_BurnIntex_ZeroUnits_Reverts() public {
        _issue(6, false);
        vm.prank(burner);
        vm.expectRevert(IIntexNFT1155.ZeroUnits.selector);
        nft.burnIntex(ownerA, SERIES_ID, 0);
    }

    function test_BurnIntex_UnknownSeries_Reverts() public {
        vm.prank(burner);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID));
        nft.burnIntex(ownerA, SERIES_ID, 1);
    }

    function test_BurnIntex_WithoutTheRole_Reverts() public {
        _issue(6, false);
        bytes32 role = nft.BURNER_ROLE();
        vm.prank(ownerA);
        vm.expectRevert(abi.encodeWithSignature("AccessControlUnauthorizedAccount(address,bytes32)", ownerA, role));
        nft.burnIntex(ownerA, SERIES_ID, 1);
    }

    // --- Live supply ---

    function test_CrosschainMint_BeyondIssuedUnits_Succeeds() public {
        uint32 issuedUnits = 10;
        _createSeries(issuedUnits);

        vm.startPrank(bridger);
        nft.issueIntex(ownerA, issuedUnits, SERIES_ID);
        nft.crosschainMint(ownerB, TOKEN_ID, 5);
        vm.stopPrank();

        assertEq(nft.totalSupply(TOKEN_ID), issuedUnits + 5);
        assertEq(nft.balanceOf(ownerB, TOKEN_ID), 5);
        assertEq(nft.readData(SERIES_ID).issuedUnits, issuedUnits, "issuedUnits stays the original issuance");
    }

    function test_CrosschainMint_PastUint32Supply_Reverts() public {
        _createSeries(10);

        vm.startPrank(bridger);
        nft.issueIntex(ownerA, 1, SERIES_ID);
        nft.crosschainMint(ownerB, TOKEN_ID, type(uint32).max - 1);
        assertEq(nft.totalSupply(TOKEN_ID), type(uint32).max, "fills the uint32 range exactly");

        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.SupplyCapExceeded.selector,
                SERIES_ID,
                uint256(type(uint32).max) + 1,
                uint256(type(uint32).max)
            )
        );
        nft.crosschainMint(ownerB, TOKEN_ID, 1);
        vm.stopPrank();
    }

    function test_CrosschainMint_AfterCrosschainBurn_RestoresSupply() public {
        uint32 issuedUnits = 10;
        _createSeries(issuedUnits);

        vm.startPrank(bridger);
        nft.issueIntex(ownerA, issuedUnits, SERIES_ID);
        nft.crosschainBurn(ownerA, ownerA, TOKEN_ID, 4);
        nft.crosschainMint(ownerB, TOKEN_ID, 4);
        vm.stopPrank();

        assertEq(nft.totalSupply(TOKEN_ID), issuedUnits);
        assertEq(nft.balanceOf(ownerA, TOKEN_ID), 6);
        assertEq(nft.balanceOf(ownerB, TOKEN_ID), 4);
        assertEq(nft.readData(SERIES_ID).totalSupply, issuedUnits, "totalSupply restored after the return hop");
    }

    function test_TotalSupply_TracksLiveIssuedBalance() public {
        _createSeries(10);
        assertEq(nft.readData(SERIES_ID).totalSupply, 0);

        vm.startPrank(bridger);
        nft.issueIntex(ownerA, 3, SERIES_ID);
        assertEq(nft.readData(SERIES_ID).totalSupply, 3);

        nft.issueIntex(ownerB, 4, SERIES_ID);
        assertEq(nft.readData(SERIES_ID).totalSupply, 7);
        vm.stopPrank();

        vm.prank(burner);
        nft.burnIntex(ownerA, SERIES_ID, 2);
        assertEq(nft.totalSupply(TOKEN_ID), 5, "burnIntex burns Issued (totalSupply 7 - 2)");
        assertEq(nft.readData(SERIES_ID).totalSupply, 5, "SeriesData mirror tracks live Issued supply");
    }
}
