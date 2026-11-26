// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IERC1155} from "@openzeppelin/contracts/token/ERC1155/IERC1155.sol";
import {DeployProxy} from "./helpers/DeployProxy.sol";
import {OpenSeriesLib} from "./helpers/OpenSeriesLib.sol";

/// @notice A called series past its transit close reads as `Forfeited`, while storage
///         keeps `Called` so every freeze written against the stored field still bites.
contract IntexNFT1155ForfeitedTest is Test {
    uint32 internal constant SERIES_ID_DAY = 20260622;
    bytes14 internal constant SERIES_ID = "20260622-USD-U";
    uint32 internal constant CAP = 10_000;
    uint32 internal constant CALL_PERIOD = 21 days;

    /// erc7201:outbe.intex.IntexNFT1155; `seriesData` is the namespace's first member.
    bytes32 internal constant STORAGE_SLOT = 0xe941cbaf65abb9f7003c3006add9c5d12ba7e339abdf88d4afd5defeb8932900;

    address internal admin = makeAddr("admin");
    address internal relayer = makeAddr("relayer");
    address internal user = makeAddr("user");

    IntexNFT1155 internal token;
    uint256 internal intexId;
    uint256 internal deadline;
    uint256 internal closeAt;

    function setUp() public {
        token = DeployProxy.intexNFT1155(admin, relayer);
        vm.startPrank(relayer);
        token.openSeries(OpenSeriesLib.params(SERIES_ID_DAY, CAP, CALL_PERIOD));
        token.issueIntex(user, 10, SERIES_ID);
        token.callSeries(SERIES_ID, uint32(block.timestamp));
        vm.stopPrank();
        intexId = token.issuedIntexId(SERIES_ID);
        deadline = block.timestamp + CALL_PERIOD;
        closeAt = deadline + token.getIntexSeries(SERIES_ID).callTrigger.transitMargin;
    }

    /// The stored `state` word: `SeriesData` packs `issuedAt`, `calledAt`, `totalSupply`
    /// and `state` into the record's fourth slot, `state` at byte 20.
    function _storedState() internal view returns (uint8) {
        bytes32 base = keccak256(abi.encode(intexId, STORAGE_SLOT));
        return uint8(uint256(vm.load(address(token), bytes32(uint256(base) + 3))) >> 160);
    }

    function test_ReadsCalledUpToTheTransitCloseAndForfeitedAfterIt() public {
        vm.warp(deadline + 1);
        assertEq(uint8(token.getIntexSeries(SERIES_ID).state), uint8(IIntexNFT1155.IntexState.Called));

        vm.warp(closeAt);
        assertEq(uint8(token.getIntexSeries(SERIES_ID).state), uint8(IIntexNFT1155.IntexState.Called));

        vm.warp(closeAt + 1);
        assertEq(uint8(token.getIntexSeries(SERIES_ID).state), uint8(IIntexNFT1155.IntexState.Forfeited));
    }

    /// The whole point of deriving: storage still says `Called`, so `_update`,
    /// `crosschainBurn` and `crosschainMint` keep refusing a Forfeited series.
    function test_StorageStillHoldsCalled() public {
        assertEq(_storedState(), uint8(IIntexNFT1155.IntexState.Called), "before");
        vm.warp(closeAt + 1);
        assertEq(_storedState(), uint8(IIntexNFT1155.IntexState.Called), "after");
    }

    function test_TransferStaysFrozenAfterExpiry() public {
        vm.warp(closeAt + 1);
        vm.prank(user);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.TransferAfterDeadline.selector, intexId, uint32(deadline)));
        token.safeTransferFrom(user, makeAddr("other"), intexId, 1, "");
    }

    function test_BridgeStaysClosedAfterExpiry() public {
        vm.warp(closeAt + 1);
        vm.prank(relayer);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeAfterDeadline.selector, intexId, uint32(deadline)));
        token.crosschainBurn(user, user, intexId, 1);
    }

    function test_BurnForfeited_BurnsTheOwnersUnitsForAnyCaller() public {
        vm.warp(closeAt + 1);
        address anyone = makeAddr("anyone");
        vm.expectEmit(address(token));
        emit IERC1155.TransferSingle(anyone, user, address(0), intexId, 10);
        vm.expectEmit(address(token));
        emit IIntexNFT1155.IntexForfeited(intexId, user, 840, 840, 10);
        vm.prank(anyone);
        token.burnForfeited(user, SERIES_ID);

        assertEq(token.balanceOf(user, intexId), 0);
        assertEq(token.getIntexSeries(SERIES_ID).totalSupply, 0);
    }

    function test_BurnForfeited_RevertsUpToTheTransitClose() public {
        vm.warp(closeAt);
        vm.expectRevert(
            abi.encodeWithSelector(
                IIntexNFT1155.InvalidState.selector,
                uint8(IIntexNFT1155.IntexState.Forfeited),
                uint8(IIntexNFT1155.IntexState.Called)
            )
        );
        token.burnForfeited(user, SERIES_ID);
    }

    function test_BurnForfeited_RevertsForAnEmptyBalance() public {
        vm.warp(closeAt + 1);
        vm.expectRevert(IIntexNFT1155.ZeroUnits.selector);
        token.burnForfeited(makeAddr("other"), SERIES_ID);
    }
}
