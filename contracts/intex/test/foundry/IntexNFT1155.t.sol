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
///      returns, the test asserts that the two were equal mid-callback. This holds iff
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
        // Snapshot the last id in the batch. The mid-callback inconsistency exists
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

    uint32 constant SERIES_ID_1_DAY = 20250101;
    bytes14 constant SERIES_ID_1 = "20250101-USD-U";
    uint32 constant SERIES_ID_2_DAY = 20250102;
    bytes14 constant SERIES_ID_2 = "20250102-USD-U";
    uint32 constant SERIES_ID_3_DAY = 20250103;
    bytes14 constant SERIES_ID_3 = "20250103-USD-U";
    uint256 constant TOKEN_ID_1 = uint256(uint112(SERIES_ID_1));
    uint256 constant TOKEN_ID_2 = uint256(uint112(SERIES_ID_2));
    uint256 constant TOKEN_ID_3 = uint256(uint112(SERIES_ID_3));

    uint32 constant ISSUED_UNITS = 10_000;

    function setUp() public {
        nft = DeployProxy.intexNFT1155(admin, bridger);
    }

    /// @dev Create a series with the standard parameters and a given call period.
    function _createSeries(uint32 worldwideDay, uint32 callPeriod) internal {
        vm.prank(bridger);
        nft.createSeries(CreateSeriesLib.params(worldwideDay, ISSUED_UNITS, callPeriod));
    }

    function test_InitialState() public view {
        assertTrue(nft.hasRole(nft.DEFAULT_ADMIN_ROLE(), admin));
        assertTrue(nft.hasRole(nft.RELAYER_ROLE(), bridger));
    }

    function test_CreateSeries() public {
        uint32 callPeriod = uint32(30 days);
        vm.prank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1_DAY, ISSUED_UNITS, callPeriod));

        IIntexNFT1155.SeriesData memory data = nft.readData(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Issued));
        assertEq(data.issuedAt, block.timestamp);
        assertEq(data.calledAt, 0);
        assertEq(data.totalSupply, 0);
        assertEq(data.issuedUnits, ISSUED_UNITS);
        // The contract stores callPeriod verbatim. Defaulting/bounding is the caller's
        // (intexfactory) responsibility.
        assertEq(data.callTrigger.callNoticePeriod, callPeriod);
    }

    function test_OnlyBridgeCanCreateSeries() public {
        vm.prank(user);
        vm.expectRevert();
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1_DAY, ISSUED_UNITS, 0));
    }

    function test_CreateSeriesDuplicate() public {
        _createSeries(SERIES_ID_1_DAY, 0);

        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.TokenAlreadyExists.selector, TOKEN_ID_1));
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1_DAY, ISSUED_UNITS, 0));
    }

    function test_CreateSeries_RecordsWorldwideDay() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        _createSeries(SERIES_ID_2_DAY, 0);

        assertEq(nft.worldwideDayOf(SERIES_ID_1), SERIES_ID_1_DAY);
        bytes14[] memory ids = nft.seriesIdsByWorldwideDay(SERIES_ID_1_DAY);
        assertEq(ids.length, 1);
        assertEq(ids[0], SERIES_ID_1);
        assertEq(nft.seriesIdsByWorldwideDay(SERIES_ID_2_DAY)[0], SERIES_ID_2);
    }

    function test_CreateSeries_IndexesTheDayCurrencyPair() public {
        IIntexNFT1155.CreateSeriesParams memory lira = CreateSeriesLib.params(SERIES_ID_1_DAY, ISSUED_UNITS, 0);
        lira.seriesId = "20250505-TRY-U";
        lira.issuanceCurrency = 949;
        vm.startPrank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1_DAY, ISSUED_UNITS, 0));
        nft.createSeries(lira);
        vm.stopPrank();

        assertEq(nft.seriesIdOf(SERIES_ID_1_DAY, 840, 840), SERIES_ID_1);
        assertEq(nft.seriesIdOf(SERIES_ID_1_DAY, 949, 840), lira.seriesId);
        assertEq(nft.seriesIdOf(SERIES_ID_1_DAY, 840, 949), bytes14(0), "the pair is ordered");
        assertEq(nft.seriesIdOf(SERIES_ID_2_DAY, 840, 840), bytes14(0), "another day has no series");
    }

    /// @dev The contract stores the day verbatim and does not infer it from `seriesId`. The test
    ///      proves this with distinct values, so that a future composite seriesId (many series per
    ///      day) records the real day. Fails if provenance reads `params.seriesId`.
    function test_CreateSeries_StoresRealDay_DistinctFromSeriesId() public {
        bytes14 seriesId = "20250505-TRY-U";
        uint32 worldwideDay = 20260101;
        IIntexNFT1155.CreateSeriesParams memory p = CreateSeriesLib.params(worldwideDay, ISSUED_UNITS, 0);
        p.seriesId = seriesId; // the id's own day differs from the provenance day
        vm.prank(bridger);
        nft.createSeries(p);

        assertEq(nft.worldwideDayOf(seriesId), worldwideDay, "day stored verbatim");
        bytes14[] memory ids = nft.seriesIdsByWorldwideDay(worldwideDay);
        assertEq(ids.length, 1);
        assertEq(ids[0], seriesId, "day indexes the series id");
        assertEq(nft.seriesIdsByWorldwideDay(20250505).length, 0, "the id's own day is not a provenance key");
    }

    function test_Issue() public {
        uint256 quantity = 10;

        _createSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, quantity, SERIES_ID_1);

        assertEq(nft.balanceOf(user, TOKEN_ID_1), quantity);
        assertEq(nft.readData(SERIES_ID_1).totalSupply, quantity);
    }

    function test_OnlyBridgeCanIssue() public {
        _createSeries(SERIES_ID_1_DAY, 0);

        vm.prank(user);
        vm.expectRevert();
        nft.issueIntex(user, 10, SERIES_ID_1);
    }

    function test_IssueToZeroAddress() public {
        _createSeries(SERIES_ID_1_DAY, 0);

        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.ZeroAddress.selector, "to", address(0)));
        nft.issueIntex(address(0), 10, SERIES_ID_1);
    }

    function test_IssueNonexistentSeries() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.issueIntex(user, 10, SERIES_ID_1);
    }

    function test_IssueUnitsTooLarge() public {
        _createSeries(SERIES_ID_1_DAY, 0);

        uint256 tooLarge = uint256(type(uint16).max) + 1;
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.UnitsTooLarge.selector, tooLarge));
        nft.issueIntex(user, tooLarge, SERIES_ID_1);
    }

    function test_MarkCalled() public {
        uint32 customCallPeriod = uint32(14 days);
        uint32 calledAt = uint32(block.timestamp);

        _createSeries(SERIES_ID_1_DAY, customCallPeriod);
        vm.prank(bridger);
        nft.markCalled(SERIES_ID_1, uint32(block.timestamp));

        IIntexNFT1155.SeriesData memory data = nft.readData(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Called));
        assertEq(data.calledAt, calledAt);
        assertEq(data.callTrigger.callNoticePeriod, customCallPeriod);
        assertEq(data.calledAt + data.callTrigger.callNoticePeriod, calledAt + customCallPeriod);
    }

    function test_TimesPastUint32MaxKeepTheirValue() public {
        vm.warp(uint256(type(uint32).max) + 1 days);
        uint32 period = uint32(14 days);
        _createSeries(SERIES_ID_1_DAY, period);
        uint64 calledAt = uint64(block.timestamp);

        vm.expectEmit(true, false, false, true);
        emit IIntexNFT1155.IntexSeriesCalled(SERIES_ID_1, calledAt, calledAt + period);
        vm.prank(bridger);
        nft.markCalled(SERIES_ID_1, calledAt);

        IIntexNFT1155.SeriesData memory data = nft.readData(SERIES_ID_1);
        assertEq(data.issuedAt, calledAt);
        assertEq(data.calledAt, calledAt);
    }

    function test_OnlyBridgeCanMarkCalled() public {
        _createSeries(SERIES_ID_1_DAY, 0);

        vm.prank(user);
        vm.expectRevert();
        nft.markCalled(SERIES_ID_1, uint32(block.timestamp));
    }

    function test_MarkCalledNonexistentToken() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.markCalled(SERIES_ID_1, uint32(block.timestamp));
    }

    function test_MarkCalledInvalidState() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.markCalled(SERIES_ID_1, uint32(block.timestamp));
        // Re-calling on an already Called series names Issued as the only state it could be called from.
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.InvalidState.selector,
                uint8(IIntexNFT1155.IntexState.Issued),
                uint8(IIntexNFT1155.IntexState.Called)
            )
        );
        nft.markCalled(SERIES_ID_1, uint32(block.timestamp));
        vm.stopPrank();
    }

    function test_CrosschainBurnNonexistentToken() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.crosschainBurn(user, user, TOKEN_ID_1, 5);
    }

    function test_CrosschainBurnAndMint_AllowedInIssuedState() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        // Voluntary bridging is open while the series is tradable (Issued): burn out...
        nft.crosschainBurn(user, user, TOKEN_ID_1, 4);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 6);

        // ...and mint in (the destination side of the same hop).
        nft.crosschainMint(user2, TOKEN_ID_1, 4);
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 4);
        vm.stopPrank();
    }

    function test_CrosschainBurn_AllowedInIssuedAndCalled_ForSystemRelayer() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        nft.crosschainBurn(user, user, TOKEN_ID_1, 3);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 7);

        nft.markCalled(SERIES_ID_1, uint32(block.timestamp));
        nft.crosschainBurn(user, user, TOKEN_ID_1, 2);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 5);
        vm.stopPrank();
    }

    /// @dev A Called series still changes holder across the bridge until its cutoff.
    function test_CrosschainBurn_InCalled_CarriesToAnotherHolder() public {
        address plainRelayer = address(0x9999);
        vm.startPrank(admin);
        nft.grantRole(nft.RELAYER_ROLE(), plainRelayer);
        vm.stopPrank();

        _createSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1, uint32(block.timestamp));
        vm.stopPrank();

        vm.prank(plainRelayer);
        nft.crosschainBurn(user, user2, TOKEN_ID_1, 1);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 9, "burned on the source, bound for another holder");
    }

    function test_ReadData() public {
        vm.prank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1_DAY, ISSUED_UNITS, 0));

        IIntexNFT1155.SeriesData memory data = nft.readData(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Issued));
    }

    function test_ReadDataNonexistentToken() public {
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.readData(SERIES_ID_1);
    }

    function test_TransferRestrictions() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        // Token should be transferable in Issued state.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 5, "");
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 5);
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 5);

        // Transfer back to user for next test.
        vm.prank(user2);
        nft.safeTransferFrom(user2, user, TOKEN_ID_1, 5, "");

        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 2, "");
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 2);

        // Mark as called. With no notice period the deadline is the call itself.
        uint32 deadline = uint32(block.timestamp);
        vm.prank(bridger);
        nft.markCalled(SERIES_ID_1, deadline);

        // A Called series still transfers until its settlement deadline.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 3, "");
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 5);

        // Past the deadline the units stay put, transit margin or not.
        vm.warp(block.timestamp + 1);
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.TransferAfterDeadline.selector, TOKEN_ID_1, deadline));
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 1, "");
    }

    function test_TransferRestrictionsIssued() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        // Token should be transferable in Issued state.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 5, "");
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 5);
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 5);
    }

    function test_Events() public {
        uint256 quantity = 10;
        uint32 customCallPeriod = uint32(14 days);
        uint64 settlementDeadline = uint64(block.timestamp + 14 days);

        vm.startPrank(bridger);
        vm.expectEmit();
        emit IIntexNFT1155.MetadataUpdate(TOKEN_ID_1);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1_DAY, ISSUED_UNITS, customCallPeriod));

        vm.expectEmit(true, true, true, true);
        emit IIntexNFT1155.IntexIssued(bridger, TOKEN_ID_1, user, quantity);
        nft.issueIntex(user, quantity, SERIES_ID_1);

        vm.expectEmit(true, false, false, true);
        emit IIntexNFT1155.IntexSeriesCalled(SERIES_ID_1, uint64(block.timestamp), settlementDeadline);
        nft.markCalled(SERIES_ID_1, uint64(block.timestamp));
        vm.stopPrank();
    }

    function test_IssuedTokenId_IsTheSeriesId() public view {
        assertEq(nft.issuedTokenId(SERIES_ID_1), uint256(uint112(SERIES_ID_1)));
    }

    function test_BatchTransferRestrictions() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        _createSeries(SERIES_ID_2_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        nft.issueIntex(user, 10, SERIES_ID_2);

        // Mark one as called. With no notice period the deadline is the call itself.
        uint32 deadline = uint32(block.timestamp);
        nft.markCalled(SERIES_ID_1, deadline);
        vm.stopPrank();

        uint256[] memory ids = new uint256[](2);
        ids[0] = TOKEN_ID_1;
        ids[1] = TOKEN_ID_2;

        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 5;
        amounts[1] = 5;

        // A batch containing a series past its settlement deadline reverts atomically.
        vm.warp(block.timestamp + 1);
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.TransferAfterDeadline.selector, TOKEN_ID_1, deadline));
        nft.safeBatchTransferFrom(user, user2, ids, amounts, "");

        // The uncalled series still transfers on its own.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_2, 5, "");
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 0);
        assertEq(nft.balanceOf(user2, TOKEN_ID_2), 5);
    }

    // --- Tests for CrosschainBurn/CrosschainMint ---
    function test_CrosschainBurn() public {
        uint256 quantity = 10;
        uint256 burnAmount = 5;

        _createSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, quantity, SERIES_ID_1);
        nft.crosschainBurn(user, user, TOKEN_ID_1, burnAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), quantity - burnAmount);
    }

    function test_CrosschainBurnInCalledState() public {
        uint256 quantity = 10;
        uint256 burnAmount = 5;

        _createSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, quantity, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1, uint32(block.timestamp));
        nft.crosschainBurn(user, user, TOKEN_ID_1, burnAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), quantity - burnAmount);
    }

    function test_OnlyBridgeCanCrosschainBurn() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        vm.prank(user);
        vm.expectRevert();
        nft.crosschainBurn(user, user, TOKEN_ID_1, 5);
    }

    function test_CrosschainMint() public {
        uint256 mintAmount = 10;

        _createSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.crosschainMint(user, TOKEN_ID_1, mintAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), mintAmount);
    }

    function test_CrosschainMintInCalledState() public {
        uint256 mintAmount = 10;

        _createSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.markCalled(SERIES_ID_1, uint32(block.timestamp));
        nft.crosschainMint(user, TOKEN_ID_1, mintAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), mintAmount);
    }

    function _callWithNotice() internal returns (uint32 deadline, uint32 closeAt) {
        uint32 callPeriod = uint32(14 days);
        _createSeries(SERIES_ID_1_DAY, callPeriod);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        uint32 calledAt = uint32(block.timestamp);
        nft.markCalled(SERIES_ID_1, calledAt);
        vm.stopPrank();
        deadline = calledAt + callPeriod;
        closeAt = deadline + nft.readData(SERIES_ID_1).callTrigger.transitMargin;
    }

    function test_CrosschainBurn_AllowedAtDeadline() public {
        (uint32 deadline,) = _callWithNotice();
        vm.warp(deadline);
        vm.prank(bridger);
        nft.crosschainBurn(user, user, TOKEN_ID_1, 5);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 5);
    }

    function test_CrosschainBurn_RevertsAfterDeadline() public {
        (uint32 deadline,) = _callWithNotice();
        vm.warp(uint256(deadline) + 1);
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeAfterDeadline.selector, TOKEN_ID_1, deadline));
        nft.crosschainBurn(user, user, TOKEN_ID_1, 5);
    }

    function test_CrosschainMint_LandsUntilTransitClose() public {
        (, uint32 closeAt) = _callWithNotice();
        vm.warp(closeAt);
        vm.prank(bridger);
        nft.crosschainMint(user, TOKEN_ID_1, 5);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 15);
    }

    function test_CrosschainMint_RevertsAfterTransitClose() public {
        (, uint32 closeAt) = _callWithNotice();
        vm.warp(uint256(closeAt) + 1);
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeAfterDeadline.selector, TOKEN_ID_1, closeAt));
        nft.crosschainMint(user, TOKEN_ID_1, 10);
    }

    function test_Issue_AllowedOnCalledSeriesUntilTransitClose() public {
        (, uint32 closeAt) = _callWithNotice();
        vm.warp(closeAt);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 20);
    }

    function test_Issue_RevertsAfterTransitClose() public {
        (, uint32 closeAt) = _callWithNotice();
        vm.warp(uint256(closeAt) + 1);
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.IssueAfterDeadline.selector, TOKEN_ID_1, closeAt));
        nft.issueIntex(user, 10, SERIES_ID_1);
    }

    /// @dev A notice period near `type(uint32).max` pushes the deadline past uint32. Nothing may
    ///      overflow.
    function test_Deadline_BeyondUint32_DoesNotOverflow() public {
        _createSeries(SERIES_ID_1_DAY, type(uint32).max);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        nft.markCalled(SERIES_ID_1, uint32(block.timestamp));
        nft.issueIntex(user, 10, SERIES_ID_1);
        nft.crosschainBurn(user, user, TOKEN_ID_1, 5);
        nft.crosschainMint(user, TOKEN_ID_1, 5);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, TOKEN_ID_1), 20);
    }

    function test_OnlyBridgeCanCrosschainMint() public {
        _createSeries(SERIES_ID_1_DAY, 0);

        vm.prank(user);
        vm.expectRevert();
        nft.crosschainMint(user, TOKEN_ID_1, 10);
    }

    function test_CrosschainMintRevertsNonexistentSeries() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.NonexistentToken.selector, TOKEN_ID_1));
        nft.crosschainMint(user, TOKEN_ID_1, 10);
    }

    // --- burnIntex: the burn of a settlement or a Gem position on Outbe ---

    /// @dev Helper: grant BURNER_ROLE on the deployed Intex contract to `account`.
    function _grantBurnerRole(address account) internal {
        bytes32 role = nft.BURNER_ROLE();
        vm.prank(admin);
        nft.grantRole(role, account);
    }

    function test_BurnIntex_TransferredUnitsBurnFromTheirNewOwner() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        vm.prank(user);
        nft.safeTransferFrom(user, user2, TOKEN_ID_1, 4, "");
        _grantBurnerRole(address(this));

        nft.burnIntex(user2, SERIES_ID_1, 3);

        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 1);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 6, "the sender's units are untouched");

        // The new owner cannot burn more than it holds by drawing on the sender's units.
        vm.expectRevert();
        nft.burnIntex(user2, SERIES_ID_1, 2);
        assertEq(nft.balanceOf(user, TOKEN_ID_1), 6);
    }

    function test_BurnIntex_RevertsOnZeroHolder() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        _grantBurnerRole(address(this));
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.ZeroAddress.selector, "owner", address(0)));
        nft.burnIntex(address(0), SERIES_ID_1, 1);
    }

    function test_BurnIntex_FreesCapRoom() public {
        uint32 cap = 10;
        vm.startPrank(bridger);
        nft.createSeries(CreateSeriesLib.params(SERIES_ID_1_DAY, cap, 0));
        nft.issueIntex(user, 10, SERIES_ID_1);
        vm.stopPrank();
        _grantBurnerRole(address(this));

        nft.burnIntex(user, SERIES_ID_1, 4);

        // Deliberate: the contract enforces the cap against live totalSupply, so a burn frees mint
        // room.
        vm.prank(bridger);
        nft.issueIntex(user2, 4, SERIES_ID_1);
        assertEq(nft.totalSupply(TOKEN_ID_1), 10);
    }

    // --- Series enumeration ---

    function test_SeriesAtListsSeriesInCreationOrder() public {
        assertEq(nft.totalSeries(), 0);

        _createSeries(SERIES_ID_1_DAY, 0);
        _createSeries(SERIES_ID_2_DAY, 0);

        assertEq(nft.totalSeries(), 2);
        assertEq(nft.seriesAt(0), SERIES_ID_1);
        assertEq(nft.seriesAt(1), SERIES_ID_2);
    }

    function test_SeriesAtPastTheEndReverts() public {
        _createSeries(SERIES_ID_1_DAY, 0);

        vm.expectRevert(IIntexNFT1155.SeriesIndexOutOfRange.selector);
        nft.seriesAt(1);
    }

    /// @dev A batch that names one token id twice must move the whole amount. The hook walks the
    ///      pair, so a mishandled duplicate would double-count or drop half the transfer.
    function test_BatchTransferWithDuplicateTokenIds() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        uint256[] memory ids = new uint256[](2);
        ids[0] = TOKEN_ID_1;
        ids[1] = TOKEN_ID_1;

        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 5;
        amounts[1] = 5;

        vm.prank(user);
        nft.safeBatchTransferFrom(user, user2, ids, amounts, "");

        assertEq(nft.balanceOf(user, TOKEN_ID_1), 0, "sender keeps nothing");
        assertEq(nft.balanceOf(user2, TOKEN_ID_1), 10, "receiver gets both halves");
    }

    // ============================================================
    // Owner-side pagination windows
    // ============================================================

    // ============================================================
    // totalSupply mid-callback consistency (read-only-reentrancy)
    // ============================================================

    function test_Issue_TotalSupplyConsistentMidCallback() public {
        _createSeries(SERIES_ID_1_DAY, 0);
        MidCallbackSnapshotReceiver receiver = new MidCallbackSnapshotReceiver(nft);

        vm.prank(bridger);
        nft.issueIntex(address(receiver), 7, SERIES_ID_1);

        assertTrue(receiver.observed(), "callback did not fire");
        assertEq(receiver.observedBalance(), 7, "balance updated mid-callback");
        assertEq(
            receiver.observedTotalSupply(), receiver.observedBalance(), "totalSupply must equal balance mid-callback"
        );
    }

    function test_CrosschainMint_TotalSupplyConsistentMidCallback() public {
        _createSeries(SERIES_ID_1_DAY, 0);

        MidCallbackSnapshotReceiver receiver = new MidCallbackSnapshotReceiver(nft);

        vm.prank(bridger);
        nft.crosschainMint(address(receiver), TOKEN_ID_1, 9);

        assertTrue(receiver.observed(), "callback did not fire");
        assertEq(receiver.observedBalance(), 9, "balance updated mid-callback");
        assertEq(
            receiver.observedTotalSupply(), receiver.observedBalance(), "totalSupply must equal balance mid-callback"
        );
    }
}
