// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {CreateSeriesLib} from "./helpers/CreateSeriesLib.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IERC1155Receiver} from "@openzeppelin/contracts/token/ERC1155/IERC1155Receiver.sol";
import {Test} from "forge-std/Test.sol";

/// @dev ERC1155 receiver that, during the `onERC1155Received` callback, snapshots
///      `totalSupply(tokenId)` against `balanceOf(self, tokenId)`. After the mint
///      returns, the test asserts the two were equal mid-callback — which holds iff
///      the contract writes `totalSupply` before `_mint` (the read-only-reentrancy guarantee).
contract MidCallbackSnapshotReceiver is IERC1155Receiver {
    IntexNFT1155 public immutable nft;
    uint256 public observedTotalSupply;
    uint256 public observedBalance;
    bool public observed;

    constructor(IntexNFT1155 nft_) {
        nft = nft_;
    }

    function onERC1155Received(address, address, uint256 id, uint256, bytes calldata) external returns (bytes4) {
        observedTotalSupply = nft.totalSupply(id);
        observedBalance = nft.balanceOf(address(this), id);
        observed = true;
        return IERC1155Receiver.onERC1155Received.selector;
    }

    function onERC1155BatchReceived(address, address, uint256[] calldata ids, uint256[] calldata, bytes calldata)
        external
        returns (bytes4)
    {
        // Snapshot the last id in the batch — the mid-callback inconsistency exists
        // after the full _mint loop, before the post-loop totalSupply write.
        uint256 last = ids[ids.length - 1];
        observedTotalSupply = nft.totalSupply(last);
        observedBalance = nft.balanceOf(address(this), last);
        observed = true;
        return IERC1155Receiver.onERC1155BatchReceived.selector;
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == type(IERC1155Receiver).interfaceId;
    }
}

contract IntexNFT1155Test is Test {
    IntexNFT1155 nft;
    address admin = address(1);
    address bridger = address(4);
    address user = address(5);
    address user2 = address(6);

    uint32 constant SERIES_ID_1 = 20250101;
    uint32 constant SERIES_ID_2 = 20250102;
    uint32 constant SERIES_ID_3 = 20250103;
    uint256 constant TOKEN_ID_1 = uint256(SERIES_ID_1);
    uint256 constant TOKEN_ID_2 = uint256(SERIES_ID_2);
    uint256 constant TOKEN_ID_3 = uint256(SERIES_ID_3);

    /// @dev Sized well above every per-mint quantity in this suite so existing tests
    ///      exercise lifecycle and bridge behavior independently of the supply cap.
    ///      Dedicated cap coverage lives in `IntexNFT1155.supply.t.sol`.
    uint32 constant ISSUED_INTEX_COUNT = 10_000;

    function setUp() public {
        nft = DeployProxy.intexNFT1155(admin, bridger);
        // Most existing bridge tests exercise both Qualified (user-driven) and Called (system)
        // bridge paths; granting SYSTEM_RELAYER_ROLE to `bridger` keeps role-orthogonal tests
        // focused on state and balance semantics. Role-specific gating is covered separately.
        vm.startPrank(admin);
        nft.grantRole(nft.SYSTEM_RELAYER_ROLE(), bridger);
        vm.stopPrank();
    }

    /// @dev Create a series with the standard parameters and a given call period.
    function _createSeries(uint32 seriesId, uint32 callPeriod) internal {
        vm.prank(bridger);
        nft.createSeries(CreateSeriesLib.params(seriesId, ISSUED_INTEX_COUNT, callPeriod));
    }

    function test_InitialState() public view {
        assertTrue(nft.hasRole(nft.DEFAULT_ADMIN_ROLE(), admin));
        assertTrue(nft.hasRole(nft.RELAYER_ROLE(), bridger));
    }

    function test_CreateSeries() public {
        uint32 callPeriod = uint32(30 days);
        vm.prank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1, ISSUED_INTEX_COUNT, callPeriod));

        IIntexNFT1155.SeriesData memory data = nft.readData(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Issued));
        assertEq(uint8(data.status), uint8(IIntexNFT1155.IntexStatus.Issued));
        assertEq(data.issuedAt, block.timestamp);
        assertEq(data.calledAt, 0);
        assertEq(data.totalSupply, 0);
        assertEq(data.issuedIntexCount, ISSUED_INTEX_COUNT);
        // callPeriod is stored verbatim; defaulting/bounding is the caller's (intexfactory) responsibility.
        assertEq(data.callTrigger.intexCallPeriod, callPeriod);
    }

    function test_OnlyBridgeCanCreateSeries() public {
        vm.prank(user);
        vm.expectRevert();
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1, ISSUED_INTEX_COUNT, 0));
    }

    function test_CreateSeriesDuplicate() public {
        _createSeries(SERIES_ID_1, 0);

        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.TokenAlreadyExists.selector, TOKEN_ID_1));
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1, ISSUED_INTEX_COUNT, 0));
    }

    function test_CreateSeries_RecordsWorldwideDay() public {
        _createSeries(SERIES_ID_1, 0);
        _createSeries(SERIES_ID_2, 0);

        assertEq(nft.worldwideDayOf(SERIES_ID_1), SERIES_ID_1);
        uint32[] memory ids = nft.seriesIdsByWorldwideDay(SERIES_ID_1);
        assertEq(ids.length, 1);
        assertEq(ids[0], SERIES_ID_1);
        assertEq(nft.seriesIdsByWorldwideDay(SERIES_ID_2)[0], SERIES_ID_2);
    }

    /// @dev The day is stored verbatim, not inferred from `seriesId`: prove it with distinct values so a future
    ///      composite seriesId (many series per day) records the real day. Fails if provenance reads `params.seriesId`.
    function test_CreateSeries_StoresRealDay_DistinctFromSeriesId() public {
        uint32 seriesId = 7;
        uint32 worldwideDay = 20260101;
        IIntexNFT1155.CreateSeriesParams memory p = CreateSeriesLib.params(worldwideDay, ISSUED_INTEX_COUNT, 0);
        p.seriesId = seriesId; // break the identity so seriesId != worldwideDay
        vm.prank(bridger);
        nft.createSeries(p);

        assertEq(nft.worldwideDayOf(seriesId), worldwideDay, "day stored verbatim");
        uint32[] memory ids = nft.seriesIdsByWorldwideDay(worldwideDay);
        assertEq(ids.length, 1);
        assertEq(ids[0], seriesId, "day indexes the series id");
        assertEq(nft.seriesIdsByWorldwideDay(seriesId).length, 0, "the series id is not itself a day key");
    }

    function test_Mint() public {
        uint256 quantity = 10;

        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, quantity, SERIES_ID_1);

        assertEq(nft.balanceOf(user, TOKEN_ID_1), quantity);
        assertEq(nft.readData(SERIES_ID_1).totalSupply, quantity);
    }

    function test_OnlyBridgeCanMint() public {
        _createSeries(SERIES_ID_1, 0);

        vm.prank(user);
        vm.expectRevert();
        nft.mint(user, 10, SERIES_ID_1);
    }

    function test_MintToZeroAddress() public {
        _createSeries(SERIES_ID_1, 0);

        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.ZeroAddress.selector, "to", address(0)));
        nft.mint(address(0), 10, SERIES_ID_1);
    }

    function test_MintNonexistentSeries() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.mint(user, 10, SERIES_ID_1);
    }

    function test_MintQuantityTooLarge() public {
        _createSeries(SERIES_ID_1, 0);

        uint256 tooLarge = uint256(type(uint16).max) + 1;
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.QuantityTooLarge.selector, tooLarge));
        nft.mint(user, tooLarge, SERIES_ID_1);
    }

    function test_AuctionWonCount_SingleMint() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        // Auction won count should be recorded.
        assertEq(nft.getAuctionWonCount(SERIES_ID_1, user), 10);
        // Non-minted address should return 0.
        assertEq(nft.getAuctionWonCount(SERIES_ID_1, user2), 0);
    }

    function test_AuctionWonCount_UnchangedAfterTransfer() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        // Transfer some tokens to user2.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 3, "");

        // Auction won count should remain unchanged for user.
        assertEq(nft.getAuctionWonCount(SERIES_ID_1, user), 10);
        // user2 received via transfer, not mint - should be 0.
        assertEq(nft.getAuctionWonCount(SERIES_ID_1, user2), 0);

        // Current balances are different from initial.
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 7);
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 3);
    }

    function test_MarkCalled() public {
        uint32 customCallPeriod = uint32(14 days);
        uint32 calledAt = uint32(block.timestamp);

        _createSeries(SERIES_ID_1, customCallPeriod);
        vm.prank(bridger);
        nft.markCalled(SERIES_ID_1);

        IIntexNFT1155.SeriesData memory data = nft.readData(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Called));
        assertEq(data.calledAt, calledAt);
        assertEq(data.callTrigger.intexCallPeriod, customCallPeriod);
        assertEq(data.calledAt + data.callTrigger.intexCallPeriod, calledAt + customCallPeriod);
    }

    function test_OnlyBridgeCanMarkCalled() public {
        _createSeries(SERIES_ID_1, 0);

        vm.prank(user);
        vm.expectRevert();
        nft.markCalled(SERIES_ID_1);
    }

    function test_MarkCalledNonexistentToken() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.markCalled(SERIES_ID_1);
    }

    function test_MarkCalledInvalidState() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.markCalled(SERIES_ID_1);
        // Re-calling on an already Called series surfaces the canonical "Qualified expected" hint.
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.InvalidState.selector,
                uint8(IIntexNFT1155.IntexState.Qualified),
                uint8(IIntexNFT1155.IntexState.Called)
            )
        );
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();
    }

    function test_MarkQualifiedTransitions() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.markQualified(SERIES_ID_1);

        IIntexNFT1155.SeriesData memory data = nft.readData(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Qualified));
        assertEq(data.calledAt, 0);
    }

    function test_MarkCalledFromQualified() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.markQualified(SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();

        IIntexNFT1155.SeriesData memory data = nft.readData(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Called));
        assertEq(data.calledAt, uint32(block.timestamp));
    }

    function test_MarkQualifiedRevertsFromCalled() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.markCalled(SERIES_ID_1);
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.InvalidState.selector,
                uint8(IIntexNFT1155.IntexState.Issued),
                uint8(IIntexNFT1155.IntexState.Called)
            )
        );
        nft.markQualified(SERIES_ID_1);
        vm.stopPrank();
    }

    function test_CrosschainBurnNonexistentToken() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.crosschainBurn(user, TOKEN_ID_1, 5);
    }

    function test_CrosschainBurnAndMint_AllowedInIssuedState() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        // Voluntary bridging is open while the series is tradable (Issued): burn out...
        nft.crosschainBurn(user, TOKEN_ID_1, 4);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 6);

        // ...and mint in (the destination side of the same hop).
        nft.crosschainMint(user2, TOKEN_ID_1, 4);
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 4);
        vm.stopPrank();
    }

    function test_CrosschainBurn_AllowedInQualifiedAndCalled_ForSystemRelayer() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        nft.markQualified(SERIES_ID_1);
        nft.crosschainBurn(user, TOKEN_ID_1, 3);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 7);

        nft.markCalled(SERIES_ID_1);
        // bridger holds SYSTEM_RELAYER_ROLE in setUp, so Called-state crosschainBurn is permitted.
        nft.crosschainBurn(user, TOKEN_ID_1, 2);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 5);
        vm.stopPrank();
    }

    function test_CrosschainBurn_RevertsInCalled_ForPlainRelayer() public {
        address plainRelayer = address(0x9999);
        vm.startPrank(admin);
        nft.grantRole(nft.RELAYER_ROLE(), plainRelayer);
        vm.stopPrank();

        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();

        vm.prank(plainRelayer);
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.BridgeStateForbidden.selector, TOKEN_ID_1, uint8(IIntexNFT1155.IntexState.Called)
            )
        );
        nft.crosschainBurn(user, TOKEN_ID_1, 1);
    }

    function test_ReadData() public {
        vm.prank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1, ISSUED_INTEX_COUNT, 0));

        IIntexNFT1155.SeriesData memory data = nft.readData(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Issued));
    }

    function test_ReadDataNonexistentToken() public {
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.readData(SERIES_ID_1);
    }

    function test_SetCollectionMetadata() public {
        string memory description = "Intex financial instrument NFT";

        vm.prank(admin);
        vm.expectEmit();
        emit IIntexNFT1155.CollectionMetadataUpdated(description);
        nft.setCollectionMetadata(description);

        assertEq(nft.collectionDescription(), description);
    }

    function test_OnlyAdminCanSetCollectionMetadata() public {
        string memory description = "Test description";

        vm.prank(user);
        vm.expectRevert();
        nft.setCollectionMetadata(description);
    }

    function test_TransferRestrictions() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        // Token should be transferable in Issued state.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 5, "");
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 5);
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 5);

        // Transfer back to user for next test.
        vm.prank(user2);
        nft.safeTransferFrom(user2, user, TOKEN_ID_1, 5, "");

        // Still transferable in Qualified state.
        vm.prank(bridger);
        nft.markQualified(SERIES_ID_1);
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 2, "");
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 2);

        // Mark as called.
        vm.prank(bridger);
        nft.markCalled(SERIES_ID_1);

        // Called freezes holder-to-holder transfers: the settlement obligation
        // stays with the holder and cannot be passed on.
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.TransferOnCalledForbidden.selector, TOKEN_ID_1));
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 3, "");
    }

    function test_TransferRestrictionsIssued() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        // Token should be transferable in Issued state.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 5, "");
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 5);
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 5);
    }

    function test_Events() public {
        uint256 quantity = 10;
        uint32 customCallPeriod = uint32(14 days);
        uint32 callDeadlineAt = uint32(block.timestamp + 14 days);

        vm.startPrank(bridger);
        vm.expectEmit();
        emit IIntexNFT1155.MetadataUpdate(TOKEN_ID_1);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1, ISSUED_INTEX_COUNT, customCallPeriod));

        vm.expectEmit(true, true, true, true);
        emit IIntexNFT1155.IntexIssued(bridger, TOKEN_ID_1, user, quantity);
        nft.mint(user, quantity, SERIES_ID_1);

        vm.expectEmit(true, true, false, true);
        emit IIntexNFT1155.IntexStatusUpdated(
            bridger,
            TOKEN_ID_1,
            IIntexNFT1155.IntexState.Issued,
            IIntexNFT1155.IntexState.Called,
            uint32(block.timestamp),
            callDeadlineAt
        );
        nft.markCalled(SERIES_ID_1);

        vm.expectEmit();
        emit IIntexNFT1155.MetadataUpdate(TOKEN_ID_1);
        nft.crosschainBurn(user, TOKEN_ID_1, 5);
        vm.stopPrank();
    }

    function test_TokenIds_PairAndStatus() public {
        _createSeries(SERIES_ID_1, 0);

        (uint256 issued, uint256 settled) = nft.tokenIds(SERIES_ID_1);
        assertEq(issued, uint256(SERIES_ID_1));
        assertEq(issued, nft.issuedTokenId(SERIES_ID_1));
        assertEq(settled, nft.settledTokenId(SERIES_ID_1));
        assertTrue(issued != settled, "issued and settled ids differ");

        assertEq(uint8(nft.statusOf(issued)), uint8(IIntexNFT1155.IntexStatus.Issued));
        assertEq(uint8(nft.statusOf(settled)), uint8(IIntexNFT1155.IntexStatus.Settled));
    }

    function test_BatchTransferRestrictions() public {
        _createSeries(SERIES_ID_1, 0);
        _createSeries(SERIES_ID_2, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user, 10, SERIES_ID_2);

        // Mark one as called.
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();

        uint256[] memory ids = new uint256[](2);
        ids[0] = TOKEN_ID_1;
        ids[1] = TOKEN_ID_2;

        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 5;
        amounts[1] = 5;

        // A batch containing a Called series id reverts atomically.
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.TransferOnCalledForbidden.selector, TOKEN_ID_1));
        nft.safeBatchTransferFrom(user, user2, ids, amounts, "");

        // The non-Called series still transfers on its own.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_2, 5, "");
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 0);
        assertEq(nft.balanceOf(user2, TOKEN_ID_2), 5);
    }

    // --- Tests for CrosschainBurn/CrosschainMint ---
    function test_CrosschainBurn() public {
        uint256 quantity = 10;
        uint256 burnAmount = 5;

        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, quantity, SERIES_ID_1);
        nft.markQualified(SERIES_ID_1);
        nft.crosschainBurn(user, TOKEN_ID_1, burnAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), quantity - burnAmount);
    }

    function test_CrosschainBurnInCalledState() public {
        uint256 quantity = 10;
        uint256 burnAmount = 5;

        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, quantity, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        nft.crosschainBurn(user, TOKEN_ID_1, burnAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), quantity - burnAmount);
    }

    function test_OnlyBridgeCanCrosschainBurn() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        vm.prank(user);
        vm.expectRevert();
        nft.crosschainBurn(user, TOKEN_ID_1, 5);
    }

    function test_CrosschainMint() public {
        uint256 mintAmount = 10;

        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.markQualified(SERIES_ID_1);
        nft.crosschainMint(user, TOKEN_ID_1, mintAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), mintAmount);
    }

    function test_CrosschainMintInCalledState() public {
        uint256 mintAmount = 10;

        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.markCalled(SERIES_ID_1);
        nft.crosschainMint(user, TOKEN_ID_1, mintAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), mintAmount);
    }

    function test_CrosschainBurn_RevertsAfterDeadline() public {
        uint32 callPeriod = uint32(14 days);
        _createSeries(SERIES_ID_1, callPeriod);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        uint32 calledAt = uint32(block.timestamp);
        nft.markCalled(SERIES_ID_1);
        uint32 deadline = calledAt + callPeriod;

        // One second past the settlement deadline: even the system relayer is frozen out.
        vm.warp(uint256(deadline) + 1);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeAfterDeadline.selector, TOKEN_ID_1, deadline));
        nft.crosschainBurn(user, TOKEN_ID_1, 5);
        vm.stopPrank();
    }

    function test_CrosschainMint_RevertsAfterDeadline() public {
        uint32 callPeriod = uint32(14 days);
        _createSeries(SERIES_ID_1, callPeriod);
        vm.startPrank(bridger);
        uint32 calledAt = uint32(block.timestamp);
        nft.markCalled(SERIES_ID_1);
        uint32 deadline = calledAt + callPeriod;

        // Mirror of crosschainBurn: crosschainMint cannot re-inflate supply after the window closes.
        vm.warp(uint256(deadline) + 1);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeAfterDeadline.selector, TOKEN_ID_1, deadline));
        nft.crosschainMint(user, TOKEN_ID_1, 10);
        vm.stopPrank();
    }

    function test_CrosschainBurn_AllowedAtDeadlineBoundary() public {
        uint32 callPeriod = uint32(14 days);
        _createSeries(SERIES_ID_1, callPeriod);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        uint32 calledAt = uint32(block.timestamp);
        nft.markCalled(SERIES_ID_1);
        uint32 deadline = calledAt + callPeriod;

        // Exactly at the deadline is still inside the window (the gate is strict `>`).
        vm.warp(deadline);
        nft.crosschainBurn(user, TOKEN_ID_1, 5);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), 5);
    }

    function test_OnlyBridgeCanCrosschainMint() public {
        _createSeries(SERIES_ID_1, 0);

        vm.prank(user);
        vm.expectRevert();
        nft.crosschainMint(user, TOKEN_ID_1, 10);
    }

    function test_CrosschainMintRevertsNonexistentSeries() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.crosschainMint(user, TOKEN_ID_1, 10);
    }

    // --- Tests for two-token model (Issued + Settled) ---

    /// @dev Helper: grant SETTLEMENT_ROLE on the deployed Intex contract to `account`.
    function _grantSettlementRole(address account) internal {
        bytes32 role = nft.SETTLEMENT_ROLE();
        vm.prank(admin);
        nft.grantRole(role, account);
    }

    /// @dev Helper: grant PROMIS_ROLE on the deployed Intex contract to `account`.
    function _grantPromisRole(address account) internal {
        bytes32 role = nft.PROMIS_ROLE();
        vm.prank(admin);
        nft.grantRole(role, account);
    }

    /// @dev Helper: grant GEM_ROLE on the deployed Intex contract to `account`.
    function _grantGemRole(address account) internal {
        bytes32 role = nft.GEM_ROLE();
        vm.prank(admin);
        nft.grantRole(role, account);
    }

    function test_Settle_BurnsIssued_MintsSettled() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();

        // Grant SETTLEMENT_ROLE to this test for direct settle invocation.
        _grantSettlementRole(address(this));
        nft.settle(SERIES_ID_1, user, user, 4);

        (uint256 issued, uint256 settled) = nft.tokenIds(SERIES_ID_1);
        assertEq(nft.balanceOf(user, issued), 6, "Issued drained by settled amount");
        assertEq(nft.balanceOf(user, settled), 4, "Settled minted to same holder");
        assertEq(nft.totalSupply(issued), 6);
        assertEq(nft.totalSupply(settled), 4);

        IIntexNFT1155.HolderBalances memory bals = nft.holderBalances(SERIES_ID_1, user);
        assertEq(bals.issued, 6);
        assertEq(bals.settled, 4);
    }

    function test_HolderBalances_AboveUint16NoTruncation() public {
        // Drive a single holder above type(uint16).max via two sub-cap mints (each <= 65_535).
        uint32 bigCap = 100_000;
        vm.startPrank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1, bigCap, uint32(21 days)));
        nft.mint(user, 40_000, SERIES_ID_1);
        nft.mint(user, 40_000, SERIES_ID_1);
        vm.stopPrank();

        // 80_000 would wrap to 14_464 under the old uint16 field; the widened field must not truncate.
        IIntexNFT1155.HolderBalances memory bals = nft.holderBalances(SERIES_ID_1, user);
        assertEq(bals.issued, 80_000);
        assertEq(bals.settled, 0);
    }

    function test_Settle_RevertsInIssued() public {
        _createSeries(SERIES_ID_1, 0);
        _grantSettlementRole(address(this));

        vm.expectRevert(
            abi.encodeWithSelector(IIntexNFT1155.InvalidStateForSettle.selector, uint8(IIntexNFT1155.IntexState.Issued))
        );
        nft.settle(SERIES_ID_1, user, user, 1);
    }

    function test_Settle_OnlySettlementRole() public {
        _createSeries(SERIES_ID_1, 0);
        // Bridger has RELAYER_ROLE only — settle must reject.
        vm.expectRevert();
        vm.prank(bridger);
        nft.settle(SERIES_ID_1, user, user, 1);
    }

    function test_Settle_EmitsIntexSettled() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();
        _grantSettlementRole(address(this));

        vm.expectEmit(true, true, false, true);
        emit IIntexNFT1155.IntexSettled(SERIES_ID_1, user, 4);
        nft.settle(SERIES_ID_1, user, user, 4);
    }

    function test_Settled_IsSoulbound() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();
        _grantSettlementRole(address(this));
        nft.settle(SERIES_ID_1, user, user, 5);

        uint256 sTok = nft.settledTokenId(SERIES_ID_1);
        // Settled cannot be transferred to another holder.
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.SoulboundSettled.selector, sTok));
        nft.safeTransferFrom(user, user2, sTok, 1, "");
    }

    function test_BurnSettled_OnlyPromisRole() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();
        _grantSettlementRole(address(this));
        nft.settle(SERIES_ID_1, user, user, 5);

        // Without PROMIS_ROLE, burnSettled reverts.
        vm.expectRevert();
        nft.burnSettled(user, SERIES_ID_1, 1);

        _grantPromisRole(address(this));

        uint256 sTok = nft.settledTokenId(SERIES_ID_1);
        vm.expectEmit(true, true, false, true);
        emit IIntexNFT1155.IntexCompleted(SERIES_ID_1, user, 3);
        nft.burnSettled(user, SERIES_ID_1, 3);

        assertEq(nft.balanceOf(user, sTok), 2);
        assertEq(nft.totalSupply(sTok), 2);
    }

    function test_Settle_RevertsAfterDeadline() public {
        uint32 callPeriod = uint32(14 days);
        _createSeries(SERIES_ID_1, callPeriod);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        uint32 calledAt = uint32(block.timestamp);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();
        uint32 deadline = calledAt + callPeriod;

        _grantSettlementRole(address(this));
        // One second past the call window: no new Settled tokens may be minted.
        vm.warp(uint256(deadline) + 1);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.SettleAfterDeadline.selector, TOKEN_ID_1, deadline));
        nft.settle(SERIES_ID_1, user, user, 4);
    }

    function test_Settle_AllowedAtDeadlineBoundary() public {
        uint32 callPeriod = uint32(14 days);
        _createSeries(SERIES_ID_1, callPeriod);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        uint32 calledAt = uint32(block.timestamp);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();
        uint32 deadline = calledAt + callPeriod;

        _grantSettlementRole(address(this));
        // Exactly at the deadline is still inside the window (the gate is strict `>`).
        vm.warp(deadline);
        nft.settle(SERIES_ID_1, user, user, 4);

        (uint256 issued, uint256 settled) = nft.tokenIds(SERIES_ID_1);
        assertEq(nft.balanceOf(user, issued), 6);
        assertEq(nft.balanceOf(user, settled), 4);
    }

    function test_Settle_QualifiedNotDeadlineGated() public {
        uint32 callPeriod = uint32(14 days);
        _createSeries(SERIES_ID_1, callPeriod);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markQualified(SERIES_ID_1);
        vm.stopPrank();

        _grantSettlementRole(address(this));
        // Qualified series have no call deadline (`calledAt == 0`), so settle is time-independent.
        vm.warp(block.timestamp + 3650 days);
        nft.settle(SERIES_ID_1, user, user, 4);

        (, uint256 settled) = nft.tokenIds(SERIES_ID_1);
        assertEq(nft.balanceOf(user, settled), 4);
    }

    function test_BurnSettled_AllowedAfterDeadline() public {
        uint32 callPeriod = uint32(14 days);
        _createSeries(SERIES_ID_1, callPeriod);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();

        _grantSettlementRole(address(this));
        _grantPromisRole(address(this));
        // Redeem after the window: the exit stays open so a settled holder is never trapped.
        uint32 deadline = uint32(block.timestamp) + callPeriod;
        nft.settle(SERIES_ID_1, user, user, 5);

        vm.warp(uint256(deadline) + 1);
        nft.burnSettled(user, SERIES_ID_1, 5);

        assertEq(nft.balanceOf(user, nft.settledTokenId(SERIES_ID_1)), 0);
    }

    // --- Tests for parkForGems (Gem Factory parking) ---

    function test_ParkForGems_BurnsIssued() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        _grantGemRole(address(this));

        vm.expectEmit(true, true, false, true);
        emit IIntexNFT1155.IntexParked(SERIES_ID_1, user, 4);
        nft.parkForGems(user, SERIES_ID_1, 4);

        assertEq(nft.balanceOf(user, TOKEN_ID_1), 6);
        assertEq(nft.totalSupply(TOKEN_ID_1), 6);
    }

    function test_ParkForGems_AllowedInQualified() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markQualified(SERIES_ID_1);
        vm.stopPrank();
        _grantGemRole(address(this));

        nft.parkForGems(user, SERIES_ID_1, 10);

        assertEq(nft.balanceOf(user, TOKEN_ID_1), 0);
        assertEq(nft.totalSupply(TOKEN_ID_1), 0);
    }

    function test_ParkForGems_OnlyGemRole() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        vm.prank(bridger);
        vm.expectRevert();
        nft.parkForGems(user, SERIES_ID_1, 1);
    }

    function test_ParkForGems_RevertsWhenCalled() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();
        _grantGemRole(address(this));

        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.InvalidState.selector,
                uint8(IIntexNFT1155.IntexState.Qualified),
                uint8(IIntexNFT1155.IntexState.Called)
            )
        );
        nft.parkForGems(user, SERIES_ID_1, 1);
    }

    function test_ParkForGems_RevertsOnNonexistentSeries() public {
        _grantGemRole(address(this));
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.parkForGems(user, SERIES_ID_1, 1);
    }

    function test_ParkForGems_RevertsOnZeroAmount() public {
        _createSeries(SERIES_ID_1, 0);
        _grantGemRole(address(this));
        vm.expectRevert(IIntexNFT1155.ZeroAmount.selector);
        nft.parkForGems(user, SERIES_ID_1, 0);
    }

    function test_ParkForGems_RevertsOnZeroHolder() public {
        _createSeries(SERIES_ID_1, 0);
        _grantGemRole(address(this));
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.ZeroAddress.selector, "holder", address(0)));
        nft.parkForGems(address(0), SERIES_ID_1, 1);
    }

    function test_ParkForGems_RevertsAboveBalance() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 5, SERIES_ID_1);
        nft.mint(user2, 5, SERIES_ID_1);
        vm.stopPrank();
        _grantGemRole(address(this));

        // amount <= totalSupply but > holder balance
        vm.expectRevert();
        nft.parkForGems(user, SERIES_ID_1, 6);
    }

    function test_ParkForGems_DoesNotTouchSettled() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markQualified(SERIES_ID_1);
        vm.stopPrank();
        _grantSettlementRole(address(this));
        nft.settle(SERIES_ID_1, user, user, 4);
        _grantGemRole(address(this));

        nft.parkForGems(user, SERIES_ID_1, 6);

        (uint256 issued, uint256 settled) = nft.tokenIds(SERIES_ID_1);
        assertEq(nft.balanceOf(user, issued), 0);
        assertEq(nft.balanceOf(user, settled), 4, "Settled balance is out of parking's reach");
        assertEq(nft.totalSupply(settled), 4);
    }

    function test_ParkForGems_FreesCapRoom() public {
        uint32 cap = 10;
        vm.startPrank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1, cap, 0));
        nft.mint(user, 10, SERIES_ID_1);
        vm.stopPrank();
        _grantGemRole(address(this));

        nft.parkForGems(user, SERIES_ID_1, 4);

        // Deliberate: the cap is enforced against live totalSupply, so parking frees mint room.
        vm.prank(bridger);
        nft.mint(user2, 4, SERIES_ID_1);
        assertEq(nft.totalSupply(TOKEN_ID_1), 10);
    }

    function test_ExpireSeries_PreservesSettled() public {
        uint32 customCallPeriod = uint32(1 days);

        _createSeries(SERIES_ID_1, customCallPeriod);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();
        _grantSettlementRole(address(this));
        nft.settle(SERIES_ID_1, user, user, 4);

        vm.warp(block.timestamp + customCallPeriod + 1);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID_1, type(uint256).max);

        (uint256 issued, uint256 settled) = nft.tokenIds(SERIES_ID_1);
        assertEq(nft.balanceOf(user, issued), 0, "Issued swept on expiration");
        assertEq(nft.balanceOf(user, settled), 4, "Settled survives expiration");
        assertEq(nft.totalSupply(issued), 0);
        assertEq(nft.totalSupply(settled), 4);
    }

    function test_BridgeOnSettled_Forbidden() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();
        _grantSettlementRole(address(this));
        nft.settle(SERIES_ID_1, user, user, 5);

        uint256 sTok = nft.settledTokenId(SERIES_ID_1);
        // crosschainBurn is gated by RELAYER_ROLE; bridger has it. Even so, Settled ids are rejected.
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeOnSettledForbidden.selector, sTok));
        nft.crosschainBurn(user, sTok, 1);
    }

    // --- Tests for expireSeries ---
    function test_ExpireSeries() public {
        uint32 customCallPeriod = uint32(1 days);

        _createSeries(SERIES_ID_1, customCallPeriod);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user2, 5, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), 10);
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 5);

        vm.warp(block.timestamp + customCallPeriod + 1);

        vm.expectEmit(true, true, false, false);
        emit IIntexNFT1155.SeriesExpired(TOKEN_ID_1, bridger);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID_1, type(uint256).max);

        IIntexNFT1155.SeriesData memory data = nft.readData(SERIES_ID_1);
        // Expiration is event-only; the series stays in Called.
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Called));
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 0);
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 0);
        assertEq(nft.totalSupply(TOKEN_ID_1), 0);
    }

    function test_ExpireSeriesRevertsBeforeDeadline() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);

        vm.expectRevert();
        nft.expireSeries(SERIES_ID_1, type(uint256).max);
        vm.stopPrank();
    }

    function test_ExpireSeriesRevertsInIssuedState() public {
        _createSeries(SERIES_ID_1, 0);

        vm.prank(bridger);
        vm.expectRevert();
        nft.expireSeries(SERIES_ID_1, type(uint256).max);
    }

    function test_ExpireSeriesRevertsIfAlreadyExpired() public {
        uint32 customCallPeriod = uint32(1 days);

        _createSeries(SERIES_ID_1, customCallPeriod);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();

        vm.warp(block.timestamp + customCallPeriod + 1);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID_1, type(uint256).max);

        vm.prank(bridger);
        vm.expectRevert();
        nft.expireSeries(SERIES_ID_1, type(uint256).max);
    }

    function test_IssuedBalanceWipedAfterExpiration() public {
        uint32 customCallPeriod = uint32(1 days);

        _createSeries(SERIES_ID_1, customCallPeriod);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();

        vm.warp(block.timestamp + customCallPeriod + 1);
        vm.prank(bridger);
        nft.expireSeries(SERIES_ID_1, type(uint256).max);

        // expireSeries sweeps the Issued balance to zero; nothing left to transfer.
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 0);
        assertEq(nft.totalSupply(TOKEN_ID_1), 0);

        vm.prank(user);
        vm.expectRevert();
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 5, "");
    }

    // --- Tests for Enumerable Functions ---

    function test_GetAllSeriesAndTotalSeries() public {
        // Initially empty.
        assertEq(nft.totalSeries(), 0);
        uint256[] memory initialSeries = nft.getAllSeries();
        assertEq(initialSeries.length, 0);

        // Create first series.
        _createSeries(SERIES_ID_1, 0);
        assertEq(nft.totalSeries(), 1);

        // Create second series.
        _createSeries(SERIES_ID_2, 0);
        assertEq(nft.totalSeries(), 2);

        // Get all series.
        uint256[] memory allSeries = nft.getAllSeries();
        assertEq(allSeries.length, 2);
        assertEq(allSeries[0], TOKEN_ID_1);
        assertEq(allSeries[1], TOKEN_ID_2);
    }

    function test_GetOwnedSeriesAndOwnedSeriesCount() public {
        // Create two series.
        _createSeries(SERIES_ID_1, 0);
        _createSeries(SERIES_ID_2, 0);

        // Initially user has no tokens.
        assertEq(nft.ownedSeriesCount(user), 0);
        uint256[] memory initialOwned = nft.getOwnedSeries(user);
        assertEq(initialOwned.length, 0);

        // Mint first series to user.
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        assertEq(nft.ownedSeriesCount(user), 1);

        // Mint second series to user.
        vm.prank(bridger);
        nft.mint(user, 5, SERIES_ID_2);
        assertEq(nft.ownedSeriesCount(user), 2);

        // Get owned series.
        uint256[] memory ownedSeries = nft.getOwnedSeries(user);
        assertEq(ownedSeries.length, 2);
    }

    function test_TotalBalance() public {
        _createSeries(SERIES_ID_1, 0);
        _createSeries(SERIES_ID_2, 0);

        // Initially zero.
        assertEq(nft.totalBalance(user), 0);

        // Mint to user.
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        assertEq(nft.totalBalance(user), 10);

        vm.prank(bridger);
        nft.mint(user, 5, SERIES_ID_2);
        assertEq(nft.totalBalance(user), 15);

        // Additional mint to same series should add up.
        vm.prank(bridger);
        nft.mint(user, 3, SERIES_ID_1);
        assertEq(nft.totalBalance(user), 18);
    }

    function test_EnumerableUpdateOnFullTransfer() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        // User has 1 series, user2 has 0.
        assertEq(nft.ownedSeriesCount(user), 1);
        assertEq(nft.ownedSeriesCount(user2), 0);

        // Transfer all to user2.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 10, "");

        // User should have 0 series now, user2 should have 1.
        assertEq(nft.ownedSeriesCount(user), 0);
        assertEq(nft.ownedSeriesCount(user2), 1);
        assertEq(nft.totalBalance(user), 0);
        assertEq(nft.totalBalance(user2), 10);

        // Verify getOwnedSeries reflects the change.
        uint256[] memory userOwned = nft.getOwnedSeries(user);
        uint256[] memory user2Owned = nft.getOwnedSeries(user2);
        assertEq(userOwned.length, 0);
        assertEq(user2Owned.length, 1);
        assertEq(user2Owned[0], TOKEN_ID_1);
    }

    function test_EnumerablePartialTransfer() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        // Partial transfer.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 5, "");

        // Both users should still own the series.
        assertEq(nft.ownedSeriesCount(user), 1);
        assertEq(nft.ownedSeriesCount(user2), 1);
        assertEq(nft.totalBalance(user), 5);
        assertEq(nft.totalBalance(user2), 5);
    }

    function test_EnumerableBurnTracking() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markQualified(SERIES_ID_1);
        vm.stopPrank();

        assertEq(nft.ownedSeriesCount(user), 1);
        assertEq(nft.totalBalance(user), 10);

        // Partial burn - should still own the series.
        vm.prank(bridger);
        nft.crosschainBurn(user, TOKEN_ID_1, 5);
        assertEq(nft.ownedSeriesCount(user), 1);
        assertEq(nft.totalBalance(user), 5);

        // Full burn - should no longer own the series.
        vm.prank(bridger);
        nft.crosschainBurn(user, TOKEN_ID_1, 5);
        assertEq(nft.ownedSeriesCount(user), 0);
        assertEq(nft.totalBalance(user), 0);

        uint256[] memory ownedAfterBurn = nft.getOwnedSeries(user);
        assertEq(ownedAfterBurn.length, 0);
    }

    function test_GetOwnedSeriesWithBalances() public {
        _createSeries(SERIES_ID_1, 0);
        _createSeries(SERIES_ID_2, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user, 25, SERIES_ID_2);
        vm.stopPrank();

        (uint256[] memory ownedTokenIds, uint256[] memory balances) = nft.getOwnedSeriesWithBalances(user);

        assertEq(ownedTokenIds.length, 2);
        assertEq(balances.length, 2);

        // Check that TOKEN_ID_1 has balance 10 and TOKEN_ID_2 has balance 25.
        for (uint256 i = 0; i < ownedTokenIds.length; i++) {
            if (ownedTokenIds[i] == TOKEN_ID_1) {
                assertEq(balances[i], 10);
            } else if (ownedTokenIds[i] == TOKEN_ID_2) {
                assertEq(balances[i], 25);
            }
        }
    }

    function test_EnumerableMultiHolderMint() public {
        _createSeries(SERIES_ID_1, 0);

        vm.startPrank(bridger);
        nft.mint(user, 5, SERIES_ID_1);
        nft.mint(user2, 10, SERIES_ID_1);
        vm.stopPrank();

        assertEq(nft.ownedSeriesCount(user), 1);
        assertEq(nft.ownedSeriesCount(user2), 1);
        assertEq(nft.totalBalance(user), 5);
        assertEq(nft.totalBalance(user2), 10);
    }

    function test_EnumerableMultipleSeries() public {
        // Create 3 series.
        _createSeries(SERIES_ID_1, 0);
        _createSeries(SERIES_ID_2, 0);
        _createSeries(SERIES_ID_3, 0);

        // Mint all 3 to user.
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user, 20, SERIES_ID_2);
        nft.mint(user, 30, SERIES_ID_3);
        vm.stopPrank();

        assertEq(nft.ownedSeriesCount(user), 3);
        assertEq(nft.totalBalance(user), 60);

        // Transfer middle one completely.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_2, 20, "");

        assertEq(nft.ownedSeriesCount(user), 2);
        assertEq(nft.totalBalance(user), 40);
        assertEq(nft.ownedSeriesCount(user2), 1);
        assertEq(nft.totalBalance(user2), 20);

        // Verify correct series are owned.
        uint256[] memory userOwned = nft.getOwnedSeries(user);
        assertEq(userOwned.length, 2);

        bool hasToken1 = false;
        bool hasToken3 = false;
        for (uint256 i = 0; i < userOwned.length; i++) {
            if (userOwned[i] == TOKEN_ID_1) hasToken1 = true;
            if (userOwned[i] == TOKEN_ID_3) hasToken3 = true;
        }
        assertTrue(hasToken1);
        assertTrue(hasToken3);
    }

    function test_EnumerableCrosschainMintCrosschainBurn() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.markQualified(SERIES_ID_1);

        // Bridge crosschainMint (like receiving from another chain).
        vm.prank(bridger);
        nft.crosschainMint(user, TOKEN_ID_1, 15);
        assertEq(nft.ownedSeriesCount(user), 1);
        assertEq(nft.totalBalance(user), 15);

        // Bridge crosschainBurn partial.
        vm.prank(bridger);
        nft.crosschainBurn(user, TOKEN_ID_1, 5);
        assertEq(nft.ownedSeriesCount(user), 1);
        assertEq(nft.totalBalance(user), 10);

        // Bridge crosschainBurn full.
        vm.prank(bridger);
        nft.crosschainBurn(user, TOKEN_ID_1, 10);
        assertEq(nft.ownedSeriesCount(user), 0);
        assertEq(nft.totalBalance(user), 0);
    }

    function test_EnumerableNoDuplicates() public {
        _createSeries(SERIES_ID_1, 0);

        // Mint multiple times to same user.
        vm.startPrank(bridger);
        nft.mint(user, 5, SERIES_ID_1);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user, 15, SERIES_ID_1);
        vm.stopPrank();

        // Should still only have 1 series entry.
        assertEq(nft.ownedSeriesCount(user), 1);
        assertEq(nft.totalBalance(user), 30);

        uint256[] memory ownedSeries = nft.getOwnedSeries(user);
        assertEq(ownedSeries.length, 1);
        assertEq(ownedSeries[0], TOKEN_ID_1);
    }

    function test_BatchTransferWithDuplicateTokenIds() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        assertEq(nft.ownedSeriesCount(user), 1);
        assertEq(nft.ownedSeriesCount(user2), 0);

        // Batch transfer with same tokenId twice: [1, 1] with amounts [5, 5].
        uint256[] memory ids = new uint256[](2);
        ids[0] = TOKEN_ID_1;
        ids[1] = TOKEN_ID_1;

        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 5;
        amounts[1] = 5;

        vm.prank(user);
        nft.safeBatchTransferFrom(user, user2, ids, amounts, "");

        // user should have 0 balance and no owned series.
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 0);
        assertEq(nft.ownedSeriesCount(user), 0);
        assertEq(nft.totalBalance(user), 0);

        // user2 should have 10 balance and 1 owned series.
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 10);
        assertEq(nft.ownedSeriesCount(user2), 1);
        assertEq(nft.totalBalance(user2), 10);

        uint256[] memory user1Owned = nft.getOwnedSeries(user);
        uint256[] memory user2Owned = nft.getOwnedSeries(user2);
        assertEq(user1Owned.length, 0);
        assertEq(user2Owned.length, 1);
        assertEq(user2Owned[0], TOKEN_ID_1);
    }

    // ============================================================
    // Series Holder Tracking (tokenId → holders[])
    // ============================================================

    function test_SeriesHolders_EmptyInitially() public {
        _createSeries(SERIES_ID_1, 0);

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 0);
        address[] memory holders = nft.getSeriesHolders(TOKEN_ID_1);
        assertEq(holders.length, 0);
    }

    function test_SeriesHolders_AddedOnMint() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 1);
        address[] memory holders = nft.getSeriesHolders(TOKEN_ID_1);
        assertEq(holders[0], user);
    }

    function test_SeriesHolders_MultipleHolders() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user2, 5, SERIES_ID_1);
        vm.stopPrank();

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 2);

        (address[] memory holders, uint256[] memory balances) = nft.getSeriesHoldersWithBalances(TOKEN_ID_1);
        assertEq(holders.length, 2);
        assertEq(balances.length, 2);
        assertEq(holders[0], user);
        assertEq(holders[1], user2);
        assertEq(balances[0], 10);
        assertEq(balances[1], 5);
    }

    function test_PaginatedGetters_WindowClipAndTotal() public {
        _createSeries(SERIES_ID_1, 0);
        address h3 = address(7);
        address h4 = address(8);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user2, 20, SERIES_ID_1);
        nft.mint(h3, 30, SERIES_ID_1);
        nft.mint(h4, 40, SERIES_ID_1);
        vm.stopPrank();

        (address[] memory hp, uint256 total) = nft.getSeriesHoldersPaginated(TOKEN_ID_1, 1, 2);
        assertEq(total, 4);
        assertEq(hp.length, 2);
        assertEq(hp[0], user2);
        assertEq(hp[1], h3);

        (address[] memory hb, uint256[] memory bal, uint256 total2) =
            nft.getSeriesHoldersWithBalancesPaginated(TOKEN_ID_1, 1, 2);
        assertEq(total2, 4);
        assertEq(hb[0], user2);
        assertEq(bal[0], 20);
        assertEq(hb[1], h3);
        assertEq(bal[1], 30);

        (address[] memory tail,) = nft.getSeriesHoldersPaginated(TOKEN_ID_1, 3, 100);
        assertEq(tail.length, 1);
        assertEq(tail[0], h4);

        (address[] memory none, uint256 total3) = nft.getSeriesHoldersPaginated(TOKEN_ID_1, 10, 5);
        assertEq(none.length, 0);
        assertEq(total3, 4);

        (uint256[] memory ids, uint256[] memory obal, uint256 ototal) =
            nft.getOwnedSeriesWithBalancesPaginated(user, 0, 10);
        assertEq(ototal, 1);
        assertEq(ids.length, 1);
        assertEq(ids[0], TOKEN_ID_1);
        assertEq(obal[0], 10);
    }

    function test_PaginatedGetters_ZeroLimitAndExactBoundary() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user2, 20, SERIES_ID_1);
        vm.stopPrank();

        // limit == 0 -> empty window, real total.
        (address[] memory h0, uint256 t0) = nft.getSeriesHoldersPaginated(TOKEN_ID_1, 0, 0);
        assertEq(h0.length, 0);
        assertEq(t0, 2);

        // offset == total -> empty window, real total.
        (address[] memory hEnd, uint256 tEnd) = nft.getSeriesHoldersPaginated(TOKEN_ID_1, 2, 5);
        assertEq(hEnd.length, 0);
        assertEq(tEnd, 2);

        // WithBalances variant, limit == 0.
        (address[] memory hb, uint256[] memory bb, uint256 t2) =
            nft.getSeriesHoldersWithBalancesPaginated(TOKEN_ID_1, 0, 0);
        assertEq(hb.length, 0);
        assertEq(bb.length, 0);
        assertEq(t2, 2);
    }

    function test_SeriesHolders_NoDuplicateOnDoubleMint() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user, 5, SERIES_ID_1);
        vm.stopPrank();

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 1);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 15);
    }

    function test_SeriesHolders_RemovedOnFullTransfer() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 1);

        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 10, "");

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 1);
        address[] memory holders = nft.getSeriesHolders(TOKEN_ID_1);
        assertEq(holders[0], user2);
    }

    function test_SeriesHolders_KeptOnPartialTransfer() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.mint(user, 10, SERIES_ID_1);

        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 3, "");

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 2);

        (address[] memory holders, uint256[] memory balances) = nft.getSeriesHoldersWithBalances(TOKEN_ID_1);
        assertEq(holders.length, 2);
        assertEq(balances[0], 7);
        assertEq(balances[1], 3);
    }

    function test_SeriesHolders_RemovedOnBurn() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markQualified(SERIES_ID_1);
        vm.stopPrank();

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 1);

        vm.prank(bridger);
        nft.crosschainBurn(user, TOKEN_ID_1, 10);

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 0);
        address[] memory holders = nft.getSeriesHolders(TOKEN_ID_1);
        assertEq(holders.length, 0);
    }

    function test_SeriesHolders_TracksAllHolders() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user2, 5, SERIES_ID_1);
        vm.stopPrank();

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 2);

        (address[] memory holders, uint256[] memory balances) = nft.getSeriesHoldersWithBalances(TOKEN_ID_1);
        assertEq(holders[0], user);
        assertEq(holders[1], user2);
        assertEq(balances[0], 10);
        assertEq(balances[1], 5);
    }

    function test_SeriesHolders_IndependentPerSeries() public {
        _createSeries(SERIES_ID_1, 0);
        _createSeries(SERIES_ID_2, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user2, 5, SERIES_ID_2);
        vm.stopPrank();

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 1);
        assertEq(nft.seriesHolderCount(TOKEN_ID_2), 1);

        address[] memory holders1 = nft.getSeriesHolders(TOKEN_ID_1);
        address[] memory holders2 = nft.getSeriesHolders(TOKEN_ID_2);
        assertEq(holders1[0], user);
        assertEq(holders2[0], user2);
    }

    function test_SeriesHolders_CrosschainBurnCrosschainMint() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markQualified(SERIES_ID_1);
        vm.stopPrank();

        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 1);

        // crosschainBurn (burn via bridge) removes holder.
        vm.prank(bridger);
        nft.crosschainBurn(user, TOKEN_ID_1, 10);
        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 0);

        // crosschainMint (mint via bridge) adds holder.
        vm.prank(bridger);
        nft.crosschainMint(user2, TOKEN_ID_1, 10);
        assertEq(nft.seriesHolderCount(TOKEN_ID_1), 1);
        address[] memory holders = nft.getSeriesHolders(TOKEN_ID_1);
        assertEq(holders[0], user2);
    }

    // ============================================================
    // totalSupply mid-callback consistency (read-only-reentrancy)
    // ============================================================

    function test_Mint_TotalSupplyConsistentMidCallback() public {
        _createSeries(SERIES_ID_1, 0);
        MidCallbackSnapshotReceiver receiver = new MidCallbackSnapshotReceiver(nft);

        vm.prank(bridger);
        nft.mint(address(receiver), 7, SERIES_ID_1);

        assertTrue(receiver.observed(), "callback did not fire");
        assertEq(receiver.observedBalance(), 7, "balance updated mid-callback");
        assertEq(
            receiver.observedTotalSupply(), receiver.observedBalance(), "totalSupply must equal balance mid-callback"
        );
    }

    function test_CrosschainMint_TotalSupplyConsistentMidCallback() public {
        _createSeries(SERIES_ID_1, 0);
        vm.prank(bridger);
        nft.markQualified(SERIES_ID_1);

        MidCallbackSnapshotReceiver receiver = new MidCallbackSnapshotReceiver(nft);

        vm.prank(bridger);
        nft.crosschainMint(address(receiver), TOKEN_ID_1, 9);

        assertTrue(receiver.observed(), "callback did not fire");
        assertEq(receiver.observedBalance(), 9, "balance updated mid-callback");
        assertEq(
            receiver.observedTotalSupply(), receiver.observedBalance(), "totalSupply must equal balance mid-callback"
        );
    }

    function test_Settle_TotalSupplyConsistentMidCallback() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();
        _grantSettlementRole(address(this));

        MidCallbackSnapshotReceiver receiver = new MidCallbackSnapshotReceiver(nft);
        uint256 sTok = nft.settledTokenId(SERIES_ID_1);
        uint256 iTokSupplyBefore = nft.totalSupply(TOKEN_ID_1);

        nft.settle(SERIES_ID_1, user, address(receiver), 4);

        // Settled mint callback: settled totalSupply must already reflect the new mint.
        assertTrue(receiver.observed(), "callback did not fire");
        assertEq(receiver.observedBalance(), 4, "settled balance updated mid-callback");
        assertEq(receiver.observedTotalSupply(), 4, "settled totalSupply must equal balance mid-callback");

        // And the Issued burn must have happened before the Settled mint — so the
        // Issued totalSupply read inside the callback would also be consistent.
        assertEq(nft.totalSupply(TOKEN_ID_1), iTokSupplyBefore - 4, "issued totalSupply decreased before settled mint");
        assertEq(nft.totalSupply(sTok), 4);
    }

    function test_GetIssuedHoldersWithBalances() public {
        _createSeries(SERIES_ID_1, 0);
        vm.startPrank(bridger);
        nft.mint(user, 10, SERIES_ID_1);
        nft.mint(user2, 6, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1);
        vm.stopPrank();

        // Settle part of user's Issued into Settled; user appears in both classes.
        _grantSettlementRole(address(this));
        nft.settle(SERIES_ID_1, user, user, 4);

        (address[] memory holders, uint256[] memory issued, uint256[] memory settled, uint256 total) =
            nft.getIssuedHoldersWithBalances(SERIES_ID_1, 0, type(uint256).max);

        assertEq(total, 2);
        assertEq(holders.length, 2);
        for (uint256 i = 0; i < holders.length; i++) {
            if (holders[i] == user) {
                assertEq(issued[i], 6);
                assertEq(settled[i], 4);
            } else if (holders[i] == user2) {
                assertEq(issued[i], 6);
                assertEq(settled[i], 0);
            }
        }
    }
}
