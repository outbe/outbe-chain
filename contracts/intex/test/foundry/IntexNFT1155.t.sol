// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {OpenSeriesLib} from "./helpers/OpenSeriesLib.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IIntexFactory} from "@precompiles/IIntexFactory.sol";
import {IERC1155} from "@openzeppelin/contracts/token/ERC1155/IERC1155.sol";
import {IERC1155Receiver} from "@openzeppelin/contracts/token/ERC1155/IERC1155Receiver.sol";
import {Test, Vm} from "forge-std/Test.sol";

/// @dev ERC1155 receiver that, during the `onERC1155Received` callback, snapshots
///      `totalSupply(intexId)` against `balanceOf(self, intexId)`. After the mint
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
    uint256 constant INTEX_ID_1 = uint256(keccak256(abi.encodePacked(SERIES_ID_1)));
    uint256 constant INTEX_ID_2 = uint256(keccak256(abi.encodePacked(SERIES_ID_2)));
    uint256 constant INTEX_ID_3 = uint256(keccak256(abi.encodePacked(SERIES_ID_3)));

    uint32 constant TOTAL_UNITS = 10_000;

    function setUp() public {
        nft = DeployProxy.intexNFT1155(admin, bridger);
    }

    /// @dev Create a series with the standard parameters and a given call period.
    function _openSeries(uint32 emissionDay, uint32 callPeriod) internal {
        vm.prank(bridger);
        nft.openSeries(OpenSeriesLib.params(emissionDay, TOTAL_UNITS, callPeriod));
    }

    function test_InitialState() public view {
        assertTrue(nft.hasRole(nft.DEFAULT_ADMIN_ROLE(), admin));
        assertTrue(nft.hasRole(nft.ROUTER_ROLE(), bridger));
        assertTrue(nft.hasRole(nft.BRIDGE_ROLE(), bridger));
    }

    function test_OpenSeries() public {
        uint32 callPeriod = uint32(30 days);
        vm.prank(bridger);
        nft.openSeries(OpenSeriesLib.params(SERIES_ID_1_DAY, TOTAL_UNITS, callPeriod));

        IIntexNFT1155.SeriesData memory data = nft.getIntexSeries(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Issued));
        assertEq(data.issuedAt, block.timestamp);
        assertEq(data.calledAt, 0);
        assertEq(data.totalSupply, 0);
        assertEq(data.totalUnits, TOTAL_UNITS);
        // The contract stores callPeriod verbatim. Defaulting/bounding is the caller's
        // (intexfactory) responsibility.
        assertEq(data.callTrigger.callNoticePeriod, callPeriod);
    }

    function test_GetIntex_ReadsTheSeriesByItsIntexId() public {
        _openSeries(SERIES_ID_1_DAY, 0);

        IIntexNFT1155.SeriesData memory byId = nft.getIntex(INTEX_ID_1);
        assertEq(byId.seriesId, SERIES_ID_1);
        assertEq(keccak256(abi.encode(byId)), keccak256(abi.encode(nft.getIntexSeries(SERIES_ID_1))));

        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.IntexNotFound.selector, INTEX_ID_1 + 1));
        nft.getIntex(INTEX_ID_1 + 1);
    }

    function test_OnlyRouterCanOpenSeries() public {
        vm.prank(user);
        vm.expectRevert();
        nft.openSeries(OpenSeriesLib.params(SERIES_ID_1_DAY, TOTAL_UNITS, 0));
    }

    function test_OpenSeriesDuplicate() public {
        _openSeries(SERIES_ID_1_DAY, 0);

        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.IntexAlreadyExists.selector, INTEX_ID_1));
        nft.openSeries(OpenSeriesLib.params(SERIES_ID_1_DAY, TOTAL_UNITS, 0));
    }

    function test_OpenSeries_RecordsEmissionDay() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        _openSeries(SERIES_ID_2_DAY, 0);

        assertEq(nft.getIntexSeries(SERIES_ID_1).emissionDay, SERIES_ID_1_DAY);
        assertEq(nft.getIntexSeries(SERIES_ID_2).emissionDay, SERIES_ID_2_DAY);
    }

    function test_OpenSeries_IndexesTheDayCurrencyPair() public {
        IIntexNFT1155.OpenSeriesParams memory lira = OpenSeriesLib.params(SERIES_ID_1_DAY, TOTAL_UNITS, 0);
        lira.seriesId = "20250505-TRY-U";
        lira.issuanceCurrency = 949;
        vm.startPrank(bridger);
        nft.openSeries(OpenSeriesLib.params(SERIES_ID_1_DAY, TOTAL_UNITS, 0));
        nft.openSeries(lira);
        vm.stopPrank();

        assertEq(nft.seriesIdOf(SERIES_ID_1_DAY, 840, 840), SERIES_ID_1);
        assertEq(nft.seriesIdOf(SERIES_ID_1_DAY, 949, 840), lira.seriesId);
        assertEq(nft.seriesIdOf(SERIES_ID_1_DAY, 840, 949), bytes14(0), "the pair is ordered");
        assertEq(nft.seriesIdOf(SERIES_ID_2_DAY, 840, 840), bytes14(0), "another day has no series");
    }

    /// @dev The contract stores the day verbatim and does not infer it from `seriesId`. The test
    ///      proves this with distinct values, so that a future composite seriesId (many series per
    ///      day) records the real day. Fails if provenance reads `params.seriesId`.
    function test_OpenSeries_StoresRealDay_DistinctFromSeriesId() public {
        bytes14 seriesId = "20250505-TRY-U";
        uint32 emissionDay = 20260101;
        IIntexNFT1155.OpenSeriesParams memory p = OpenSeriesLib.params(emissionDay, TOTAL_UNITS, 0);
        p.seriesId = seriesId; // the id's own day differs from the provenance day
        vm.prank(bridger);
        nft.openSeries(p);

        assertEq(nft.getIntexSeries(seriesId).emissionDay, emissionDay, "day stored verbatim");
    }

    function test_Issue() public {
        uint256 quantity = 10;

        _openSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, quantity, SERIES_ID_1);

        assertEq(nft.balanceOf(user, INTEX_ID_1), quantity);
        assertEq(nft.getIntexSeries(SERIES_ID_1).totalSupply, quantity);
    }

    function test_OnlyBridgeCanIssue() public {
        _openSeries(SERIES_ID_1_DAY, 0);

        vm.prank(user);
        vm.expectRevert();
        nft.issueIntex(user, 10, SERIES_ID_1);
    }

    function test_IssueToZeroAddress() public {
        _openSeries(SERIES_ID_1_DAY, 0);

        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.ZeroAddress.selector, "to", address(0)));
        nft.issueIntex(address(0), 10, SERIES_ID_1);
    }

    function test_IssueNonexistentSeries() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.IntexNotFound.selector, INTEX_ID_1));
        nft.issueIntex(user, 10, SERIES_ID_1);
    }

    function test_IssueUnitsTooLarge() public {
        _openSeries(SERIES_ID_1_DAY, 0);

        uint256 tooLarge = uint256(type(uint16).max) + 1;
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.UnitsTooLarge.selector, tooLarge));
        nft.issueIntex(user, tooLarge, SERIES_ID_1);
    }

    function test_CallSeries() public {
        uint32 customCallPeriod = uint32(14 days);
        uint32 calledAt = uint32(block.timestamp);

        _openSeries(SERIES_ID_1_DAY, customCallPeriod);
        vm.prank(bridger);
        nft.callSeries(SERIES_ID_1, uint32(block.timestamp));

        IIntexNFT1155.SeriesData memory data = nft.getIntexSeries(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Called));
        assertEq(data.calledAt, calledAt);
        assertEq(data.callTrigger.callNoticePeriod, customCallPeriod);
        assertEq(data.calledAt + data.callTrigger.callNoticePeriod, calledAt + customCallPeriod);
    }

    function test_TimesPastUint32MaxKeepTheirValue() public {
        vm.warp(uint256(type(uint32).max) + 1 days);
        uint32 period = uint32(14 days);
        _openSeries(SERIES_ID_1_DAY, period);
        uint64 calledAt = uint64(block.timestamp);

        vm.expectEmit(true, false, false, true);
        emit IIntexNFT1155.IntexSeriesCalled(SERIES_ID_1, 840, 840, calledAt, calledAt + period);
        vm.prank(bridger);
        nft.callSeries(SERIES_ID_1, calledAt);

        IIntexNFT1155.SeriesData memory data = nft.getIntexSeries(SERIES_ID_1);
        assertEq(data.issuedAt, calledAt);
        assertEq(data.calledAt, calledAt);
    }

    function test_OnlyRouterCanCallSeries() public {
        _openSeries(SERIES_ID_1_DAY, 0);

        vm.prank(user);
        vm.expectRevert();
        nft.callSeries(SERIES_ID_1, uint32(block.timestamp));
    }

    function test_CallSeriesIntexNotFound() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.IntexNotFound.selector, INTEX_ID_1));
        nft.callSeries(SERIES_ID_1, uint32(block.timestamp));
    }

    function test_CallSeriesInvalidState() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.callSeries(SERIES_ID_1, uint32(block.timestamp));
        // Re-calling on an already Called series names Issued as the only state it could be called from.
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.InvalidState.selector,
                uint8(IIntexNFT1155.IntexState.Issued),
                uint8(IIntexNFT1155.IntexState.Called)
            )
        );
        nft.callSeries(SERIES_ID_1, uint32(block.timestamp));
        vm.stopPrank();
    }

    function test_CrosschainBurnIntexNotFound() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.IntexNotFound.selector, INTEX_ID_1));
        nft.crosschainBurn(user, user, INTEX_ID_1, 5);
    }

    function test_CrosschainBurnAndMint_AllowedInIssuedState() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        // Voluntary bridging is open while the series is tradable (Issued): burn out...
        nft.crosschainBurn(user, user, INTEX_ID_1, 4);
        assertEq(nft.balanceOf(user, INTEX_ID_1), 6);

        // ...and mint in (the destination side of the same hop).
        nft.crosschainMint(user2, INTEX_ID_1, 4);
        assertEq(nft.balanceOf(user2, INTEX_ID_1), 4);
        vm.stopPrank();
    }

    function test_CrosschainBurn_AllowedInIssuedAndCalled_ForSystemRelayer() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        nft.crosschainBurn(user, user, INTEX_ID_1, 3);
        assertEq(nft.balanceOf(user, INTEX_ID_1), 7);

        nft.callSeries(SERIES_ID_1, uint32(block.timestamp));
        nft.crosschainBurn(user, user, INTEX_ID_1, 2);
        assertEq(nft.balanceOf(user, INTEX_ID_1), 5);
        vm.stopPrank();
    }

    function test_RouterRole_CannotBurnOrMint() public {
        address router = address(0x7001);
        bytes32 routerRole = nft.ROUTER_ROLE();
        vm.prank(admin);
        nft.grantRole(routerRole, router);
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        bytes32 role = nft.BRIDGE_ROLE();

        vm.startPrank(router);
        vm.expectRevert(abi.encodeWithSignature("AccessControlUnauthorizedAccount(address,bytes32)", router, role));
        nft.crosschainBurn(user, user, INTEX_ID_1, 1);
        vm.expectRevert(abi.encodeWithSignature("AccessControlUnauthorizedAccount(address,bytes32)", router, role));
        nft.crosschainMint(user, INTEX_ID_1, 1);
        vm.stopPrank();
    }

    function test_BridgeRole_CannotOpenOrCallASeries() public {
        address bridge = address(0x7002);
        bytes32 bridgeRole = nft.BRIDGE_ROLE();
        vm.prank(admin);
        nft.grantRole(bridgeRole, bridge);
        _openSeries(SERIES_ID_1_DAY, 0);
        bytes32 role = nft.ROUTER_ROLE();

        vm.startPrank(bridge);
        vm.expectRevert(abi.encodeWithSignature("AccessControlUnauthorizedAccount(address,bytes32)", bridge, role));
        nft.openSeries(OpenSeriesLib.params(SERIES_ID_2_DAY, TOTAL_UNITS, 0));
        vm.expectRevert(abi.encodeWithSignature("AccessControlUnauthorizedAccount(address,bytes32)", bridge, role));
        nft.callSeries(SERIES_ID_1, uint64(block.timestamp));
        vm.stopPrank();
    }

    /// @dev A Called series still changes holder across the bridge until its cutoff.
    function test_CrosschainBurn_InCalled_CarriesToAnotherHolder() public {
        address plainRelayer = address(0x9999);
        vm.startPrank(admin);
        nft.grantRole(nft.BRIDGE_ROLE(), plainRelayer);
        vm.stopPrank();

        _openSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        nft.callSeries(SERIES_ID_1, uint32(block.timestamp));
        vm.stopPrank();

        vm.prank(plainRelayer);
        nft.crosschainBurn(user, user2, INTEX_ID_1, 1);
        assertEq(nft.balanceOf(user, INTEX_ID_1), 9, "burned on the source, bound for another holder");
    }

    function test_GetIntexSeries() public {
        vm.prank(bridger);
        nft.openSeries(OpenSeriesLib.params(SERIES_ID_1_DAY, TOTAL_UNITS, 0));

        IIntexNFT1155.SeriesData memory data = nft.getIntexSeries(SERIES_ID_1);
        assertEq(uint8(data.state), uint8(IIntexNFT1155.IntexState.Issued));
    }

    function test_GetIntexSeriesIntexNotFound() public {
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.IntexNotFound.selector, INTEX_ID_1));
        nft.getIntexSeries(SERIES_ID_1);
    }

    function test_TransferRestrictions() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        // Token should be transferable in Issued state.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, INTEX_ID_1, 5, "");
        assertEq(nft.balanceOf(user, INTEX_ID_1), 5);
        assertEq(nft.balanceOf(user2, INTEX_ID_1), 5);

        // Transfer back to user for next test.
        vm.prank(user2);
        nft.safeTransferFrom(user2, user, INTEX_ID_1, 5, "");

        vm.prank(user);
        nft.safeTransferFrom(user, user2, INTEX_ID_1, 2, "");
        assertEq(nft.balanceOf(user2, INTEX_ID_1), 2);

        // Call the series. With no notice period the deadline is the call itself.
        uint32 deadline = uint32(block.timestamp);
        vm.prank(bridger);
        nft.callSeries(SERIES_ID_1, deadline);

        // A Called series still transfers until its settlement deadline.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, INTEX_ID_1, 3, "");
        assertEq(nft.balanceOf(user2, INTEX_ID_1), 5);

        // Past the deadline the units stay put, transit margin or not.
        vm.warp(block.timestamp + 1);
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.TransferAfterDeadline.selector, INTEX_ID_1, deadline));
        nft.safeTransferFrom(user, user2, INTEX_ID_1, 1, "");
    }

    function test_TransferRestrictionsIssued() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        // Token should be transferable in Issued state.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, INTEX_ID_1, 5, "");
        assertEq(nft.balanceOf(user, INTEX_ID_1), 5);
        assertEq(nft.balanceOf(user2, INTEX_ID_1), 5);
    }

    function test_Events() public {
        uint256 quantity = 10;
        uint32 customCallPeriod = uint32(14 days);
        uint64 settlementDeadline = uint64(block.timestamp + 14 days);

        vm.startPrank(bridger);
        vm.expectEmit();
        emit IIntexNFT1155.MetadataUpdate(INTEX_ID_1);
        nft.openSeries(OpenSeriesLib.params(SERIES_ID_1_DAY, TOTAL_UNITS, customCallPeriod));

        bytes memory expectedPayload = abi.encode(uint256(100_000 * 1e6) * quantity);
        vm.expectEmit(true, true, true, true);
        emit IIntexNFT1155.IntexIssued(INTEX_ID_1, user, SERIES_ID_1, 840, 840, uint32(quantity), expectedPayload);
        vm.recordLogs();
        nft.issueIntex(user, quantity, SERIES_ID_1);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        uint256 totalUnits;
        uint256 transferSingleValue;
        bool sawIssued;
        bool sawTransfer;
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].topics.length == 0) continue;
            if (logs[i].topics[0] == IIntexNFT1155.IntexIssued.selector) {
                (,, uint32 units,) = abi.decode(logs[i].data, (uint16, uint16, uint32, bytes));
                totalUnits = uint256(units);
                sawIssued = true;
            }
            if (logs[i].topics[0] == IERC1155.TransferSingle.selector) {
                (, uint256 value) = abi.decode(logs[i].data, (uint256, uint256));
                transferSingleValue = value;
                sawTransfer = true;
            }
        }
        assertTrue(sawIssued);
        assertTrue(sawTransfer);
        assertEq(totalUnits, transferSingleValue);

        vm.expectEmit(true, false, false, true);
        emit IIntexNFT1155.IntexSeriesCalled(SERIES_ID_1, 840, 840, uint64(block.timestamp), settlementDeadline);
        nft.callSeries(SERIES_ID_1, uint64(block.timestamp));
        vm.stopPrank();
    }

    function test_BatchTransferRestrictions() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        _openSeries(SERIES_ID_2_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        nft.issueIntex(user, 10, SERIES_ID_2);

        // Call one series. With no notice period the deadline is the call itself.
        uint32 deadline = uint32(block.timestamp);
        nft.callSeries(SERIES_ID_1, deadline);
        vm.stopPrank();

        uint256[] memory ids = new uint256[](2);
        ids[0] = INTEX_ID_1;
        ids[1] = INTEX_ID_2;

        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 5;
        amounts[1] = 5;

        // A batch containing a series past its settlement deadline reverts atomically.
        vm.warp(block.timestamp + 1);
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.TransferAfterDeadline.selector, INTEX_ID_1, deadline));
        nft.safeBatchTransferFrom(user, user2, ids, amounts, "");

        // The uncalled series still transfers on its own.
        vm.prank(user);
        nft.safeTransferFrom(user, user2, INTEX_ID_2, 5, "");
        assertEq(nft.balanceOf(user2, INTEX_ID_1), 0);
        assertEq(nft.balanceOf(user2, INTEX_ID_2), 5);
    }

    // --- Tests for CrosschainBurn/CrosschainMint ---
    function test_CrosschainBurn() public {
        uint256 quantity = 10;
        uint256 burnAmount = 5;

        _openSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, quantity, SERIES_ID_1);
        nft.crosschainBurn(user, user, INTEX_ID_1, burnAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, INTEX_ID_1), quantity - burnAmount);
    }

    function test_CrosschainBurnInCalledState() public {
        uint256 quantity = 10;
        uint256 burnAmount = 5;

        _openSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.issueIntex(user, quantity, SERIES_ID_1);
        nft.callSeries(SERIES_ID_1, uint32(block.timestamp));
        nft.crosschainBurn(user, user, INTEX_ID_1, burnAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, INTEX_ID_1), quantity - burnAmount);
    }

    function test_OnlyBridgeCanCrosschainBurn() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        vm.prank(user);
        vm.expectRevert();
        nft.crosschainBurn(user, user, INTEX_ID_1, 5);
    }

    function test_CrosschainMint() public {
        uint256 mintAmount = 10;

        _openSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.crosschainMint(user, INTEX_ID_1, mintAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, INTEX_ID_1), mintAmount);
    }

    function test_CrosschainMintInCalledState() public {
        uint256 mintAmount = 10;

        _openSeries(SERIES_ID_1_DAY, 0);
        vm.startPrank(bridger);
        nft.callSeries(SERIES_ID_1, uint32(block.timestamp));
        nft.crosschainMint(user, INTEX_ID_1, mintAmount);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, INTEX_ID_1), mintAmount);
    }

    function _callWithNotice() internal returns (uint32 deadline, uint32 closeAt) {
        uint32 callPeriod = uint32(14 days);
        _openSeries(SERIES_ID_1_DAY, callPeriod);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        uint32 calledAt = uint32(block.timestamp);
        nft.callSeries(SERIES_ID_1, calledAt);
        vm.stopPrank();
        deadline = calledAt + callPeriod;
        closeAt = deadline + nft.getIntexSeries(SERIES_ID_1).callTrigger.transitMargin;
    }

    function test_CrosschainBurn_AllowedAtDeadline() public {
        (uint32 deadline,) = _callWithNotice();
        vm.warp(deadline);
        vm.prank(bridger);
        nft.crosschainBurn(user, user, INTEX_ID_1, 5);
        assertEq(nft.balanceOf(user, INTEX_ID_1), 5);
    }

    function test_CrosschainBurn_RevertsAfterDeadline() public {
        (uint32 deadline,) = _callWithNotice();
        vm.warp(uint256(deadline) + 1);
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeAfterDeadline.selector, INTEX_ID_1, deadline));
        nft.crosschainBurn(user, user, INTEX_ID_1, 5);
    }

    function test_CrosschainMint_LandsUntilTransitClose() public {
        (, uint32 closeAt) = _callWithNotice();
        vm.warp(closeAt);
        vm.prank(bridger);
        nft.crosschainMint(user, INTEX_ID_1, 5);
        assertEq(nft.balanceOf(user, INTEX_ID_1), 15);
    }

    function test_CrosschainMint_RevertsAfterTransitClose() public {
        (, uint32 closeAt) = _callWithNotice();
        vm.warp(uint256(closeAt) + 1);
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeAfterDeadline.selector, INTEX_ID_1, closeAt));
        nft.crosschainMint(user, INTEX_ID_1, 10);
    }

    function test_Issue_AllowedOnCalledSeriesUntilTransitClose() public {
        (, uint32 closeAt) = _callWithNotice();
        vm.warp(closeAt);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        assertEq(nft.balanceOf(user, INTEX_ID_1), 20);
    }

    function test_Issue_RevertsAfterTransitClose() public {
        (, uint32 closeAt) = _callWithNotice();
        vm.warp(uint256(closeAt) + 1);
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.IssueAfterDeadline.selector, INTEX_ID_1, closeAt));
        nft.issueIntex(user, 10, SERIES_ID_1);
    }

    /// @dev A notice period near `type(uint32).max` pushes the deadline past uint32. Nothing may
    ///      overflow.
    function test_Deadline_BeyondUint32_DoesNotOverflow() public {
        _openSeries(SERIES_ID_1_DAY, type(uint32).max);
        vm.startPrank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        nft.callSeries(SERIES_ID_1, uint32(block.timestamp));
        nft.issueIntex(user, 10, SERIES_ID_1);
        nft.crosschainBurn(user, user, INTEX_ID_1, 5);
        nft.crosschainMint(user, INTEX_ID_1, 5);
        vm.stopPrank();

        assertEq(nft.balanceOf(user, INTEX_ID_1), 20);
    }

    function test_OnlyBridgeCanCrosschainMint() public {
        _openSeries(SERIES_ID_1_DAY, 0);

        vm.prank(user);
        vm.expectRevert();
        nft.crosschainMint(user, INTEX_ID_1, 10);
    }

    function test_CrosschainMintRevertsNonexistentSeries() public {
        vm.prank(bridger);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.IntexNotFound.selector, INTEX_ID_1));
        nft.crosschainMint(user, INTEX_ID_1, 10);
    }

    // --- the series and item events of the token ---

    function test_intex_token_opens_a_series_with_the_factory_shape() public {
        assertEq(IIntexNFT1155.IntexSeriesOpened.selector, IIntexFactory.IntexSeriesOpened.selector, "one shape");
        IIntexNFT1155.OpenSeriesParams memory p = OpenSeriesLib.params(SERIES_ID_1_DAY, TOTAL_UNITS, 7 days);
        p.callTrigger.callWindow = 28 days;
        p.callTrigger.callThreshold = 21 days;

        vm.expectEmit(true, true, true, true, address(nft));
        emit IIntexNFT1155.IntexSeriesOpened(
            SERIES_ID_1, 840, 840, 1e6, 200, 28 days, 21 days, 7 days, 1 hours, TOTAL_UNITS, 100_000 * 1e6
        );
        vm.prank(bridger);
        nft.openSeries(p);
    }

    function test_intex_issued_has_units_before_payload() public {
        assertEq(
            IIntexNFT1155.IntexIssued.selector,
            keccak256("IntexIssued(uint256,address,bytes14,uint16,uint16,uint32,bytes)"),
            "units come before the payload"
        );
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.recordLogs();
        vm.prank(bridger);
        nft.issueIntex(user, 3, SERIES_ID_1);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            if (logs[i].topics[0] != IIntexNFT1155.IntexIssued.selector) continue;
            (,, uint32 units, bytes memory payload) = abi.decode(logs[i].data, (uint16, uint16, uint32, bytes));
            assertEq(units, 3);
            assertEq(abi.decode(payload, (uint256)), uint256(100_000 * 1e6) * 3);
            return;
        }
        fail();
    }

    function test_intex_units_sent_to_the_gem_factory_end_with_their_event() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        _grantBurnerRole(address(this));
        uint256 gemFactoryPositionId = 0xabc;

        vm.expectEmit(true, true, true, true, address(nft));
        emit IERC1155.TransferSingle(address(this), user, address(0), INTEX_ID_1, 4);
        vm.expectEmit(true, true, true, true, address(nft));
        emit IIntexNFT1155.IntexSentToGemFactory(INTEX_ID_1, user, gemFactoryPositionId, 4);
        nft.sendIntexToGemFactory(user, SERIES_ID_1, 4, gemFactoryPositionId);

        assertEq(nft.balanceOf(user, INTEX_ID_1), 6);
        assertEq(nft.totalSupply(INTEX_ID_1), 6);
    }

    function test_SendIntexToGemFactory_OnlyBurner() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.expectRevert();
        nft.sendIntexToGemFactory(user, SERIES_ID_1, 1, 1);
    }

    function test_BurnIntex_EmitsNoGemFactoryEvent() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 2, SERIES_ID_1);
        _grantBurnerRole(address(this));
        vm.recordLogs();
        nft.settleIntex(user, SERIES_ID_1, 1);
        Vm.Log[] memory logs = vm.getRecordedLogs();
        for (uint256 i; i < logs.length; ++i) {
            assertTrue(logs[i].topics[0] != IIntexNFT1155.IntexSentToGemFactory.selector, "a settlement is no Gem send");
        }
    }

    // --- settleIntex: the burn of a settlement or a Gem position on Outbe ---

    /// @dev Helper: grant BURNER_ROLE on the deployed Intex contract to `account`.
    function _grantBurnerRole(address account) internal {
        bytes32 role = nft.BURNER_ROLE();
        vm.prank(admin);
        nft.grantRole(role, account);
    }

    function test_BurnIntex_TransferredUnitsBurnFromTheirNewOwner() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);
        vm.prank(user);
        nft.safeTransferFrom(user, user2, INTEX_ID_1, 4, "");
        _grantBurnerRole(address(this));

        nft.settleIntex(user2, SERIES_ID_1, 3);

        assertEq(nft.balanceOf(user2, INTEX_ID_1), 1);
        assertEq(nft.balanceOf(user, INTEX_ID_1), 6, "the sender's units are untouched");

        // The new owner cannot burn more than it holds by drawing on the sender's units.
        vm.expectRevert();
        nft.settleIntex(user2, SERIES_ID_1, 2);
        assertEq(nft.balanceOf(user, INTEX_ID_1), 6);
    }

    function test_BurnIntex_RevertsOnZeroHolder() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        _grantBurnerRole(address(this));
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.ZeroAddress.selector, "owner", address(0)));
        nft.settleIntex(address(0), SERIES_ID_1, 1);
    }

    function test_BurnIntex_FreesCapRoom() public {
        uint32 cap = 10;
        vm.startPrank(bridger);
        nft.openSeries(OpenSeriesLib.params(SERIES_ID_1_DAY, cap, 0));
        nft.issueIntex(user, 10, SERIES_ID_1);
        vm.stopPrank();
        _grantBurnerRole(address(this));

        nft.settleIntex(user, SERIES_ID_1, 4);

        // Deliberate: the contract enforces the cap against live totalSupply, so a burn frees mint
        // room.
        vm.prank(bridger);
        nft.issueIntex(user2, 4, SERIES_ID_1);
        assertEq(nft.totalSupply(INTEX_ID_1), 10);
    }

    /// @dev A batch that names one token id twice must move the whole amount. The hook walks the
    ///      pair, so a mishandled duplicate would double-count or drop half the transfer.
    function test_BatchTransferWithDuplicateIntexIds() public {
        _openSeries(SERIES_ID_1_DAY, 0);
        vm.prank(bridger);
        nft.issueIntex(user, 10, SERIES_ID_1);

        uint256[] memory ids = new uint256[](2);
        ids[0] = INTEX_ID_1;
        ids[1] = INTEX_ID_1;

        uint256[] memory amounts = new uint256[](2);
        amounts[0] = 5;
        amounts[1] = 5;

        vm.prank(user);
        nft.safeBatchTransferFrom(user, user2, ids, amounts, "");

        assertEq(nft.balanceOf(user, INTEX_ID_1), 0, "sender keeps nothing");
        assertEq(nft.balanceOf(user2, INTEX_ID_1), 10, "receiver gets both halves");
    }

    // ============================================================
    // Owner-side pagination windows
    // ============================================================

    // ============================================================
    // totalSupply mid-callback consistency (read-only-reentrancy)
    // ============================================================

    function test_Issue_TotalSupplyConsistentMidCallback() public {
        _openSeries(SERIES_ID_1_DAY, 0);
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
        _openSeries(SERIES_ID_1_DAY, 0);

        MidCallbackSnapshotReceiver receiver = new MidCallbackSnapshotReceiver(nft);

        vm.prank(bridger);
        nft.crosschainMint(address(receiver), INTEX_ID_1, 9);

        assertTrue(receiver.observed(), "callback did not fire");
        assertEq(receiver.observedBalance(), 9, "balance updated mid-callback");
        assertEq(
            receiver.observedTotalSupply(), receiver.observedBalance(), "totalSupply must equal balance mid-callback"
        );
    }
}
