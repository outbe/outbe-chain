// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../utils/CrossChainTest.sol";
import {DeployProxy} from "../utils/DeployProxy.sol";
import {OpenSeriesLib} from "../utils/OpenSeriesLib.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {BatchSendParam, RemoteRouter} from "@contracts/shared/interfaces/IIntexNFT1155Router.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";

/// @dev Intex units between two chains when their mint fails or their series forfeits. Chain A
///      issues the series, settles some units and sends some to the GemFactory before the call;
///      the rest are open. At each step the open units are on a chain, in a kept failed mint, or
///      burned by the forfeit, each one once.
contract IntexUnitsInTransitTest is CrossChainTest {
    uint32 private constant A_CHAIN_ID = 1;
    uint32 private constant B_CHAIN_ID = 2;
    uint32 private constant DAY = 20260701;
    bytes14 private constant SERIES = "20260701-USD-U";
    uint256 private constant ID = uint256(keccak256(abi.encodePacked(SERIES)));
    uint32 private constant NOTICE = 60;

    uint256 private constant ISSUED = 100;
    uint256 private constant SETTLED = 10;
    uint256 private constant TO_GEM_FACTORY = 5;
    uint256 private constant OPEN = ISSUED - SETTLED - TO_GEM_FACTORY;

    IntexNFT1155 private tokenA;
    IntexNFT1155 private tokenB;
    IntexNFT1155Router private routerA;
    IntexNFT1155Router private routerB;
    address private user = address(0x1);

    function setUp() public {
        vm.deal(user, 1000 ether);
        vm.deal(address(this), 1000 ether);
        _setUpBridge();
        address routerAProxy = DeployProxy.reserve();
        tokenA = DeployProxy.intexNFT1155(
            address(this),
            DeployProxy.NftLinks(address(this), routerAProxy, address(this), address(this), address(this))
        );
        address routerBProxy = DeployProxy.reserve();
        tokenB = DeployProxy.intexNFT1155(
            address(this),
            DeployProxy.NftLinks(address(this), routerBProxy, address(this), address(this), address(this))
        );
        routerA = DeployProxy.intexNFT1155RouterAt(
            routerAProxy, address(tokenA), address(bridge), address(this), new RemoteRouter[](0)
        );
        routerB = DeployProxy.intexNFT1155RouterAt(
            routerBProxy, address(tokenB), address(bridge), address(this), new RemoteRouter[](0)
        );
        routerA.setRemoteRouter(B_CHAIN_ID, _interop(B_CHAIN_ID, address(routerB)));
        routerB.setRemoteRouter(A_CHAIN_ID, _interop(A_CHAIN_ID, address(routerA)));
        tokenA.openSeries(OpenSeriesLib.params(DAY, uint32(ISSUED), NOTICE));
    }

    function test_RetryBeforeTheDeadlineMintsOnTheDestination() public {
        uint256[2] memory ks = _ks();
        for (uint256 i; i < ks.length; ++i) {
            uint256 k = ks[i];
            uint256 snap = vm.snapshotState();
            bytes32 idB = _start(k);
            tokenB.openSeries(OpenSeriesLib.params(DAY, uint32(ISSUED), NOTICE));
            routerB.retryIntexCrosschainReceive(idB, 0);
            assertEq(tokenB.totalSupply(ID), k);
            _assertOpenUnits(idB, bytes32(0), 0);
            vm.revertToState(snap);
        }
    }

    function test_SendBackBeforeTheDeadlineMintsOnTheSource() public {
        uint256[2] memory ks = _ks();
        for (uint256 i; i < ks.length; ++i) {
            uint256 k = ks[i];
            uint256 snap = vm.snapshotState();
            bytes32 idB = _start(k);
            bytes32 idA = _sendBack(idB);
            assertEq(tokenA.totalSupply(ID), OPEN);
            assertEq(_kept(routerA, idA), 0);
            _assertOpenUnits(idB, idA, 0);
            vm.revertToState(snap);
        }
    }

    function test_ForfeitCountsTheKeptUnitsOnce() public {
        uint256[2] memory ks = _ks();
        for (uint256 i; i < ks.length; ++i) {
            uint256 k = ks[i];
            uint256 snap = vm.snapshotState();
            bytes32 idB = _start(k);
            uint256 forfeited = _callCloseAndForfeit();
            assertEq(forfeited, OPEN - k);
            assertEq(_kept(routerB, idB), k);
            _assertOpenUnits(idB, bytes32(0), forfeited);
            vm.revertToState(snap);
        }
    }

    function test_RetryAfterTheForfeitMintsNothing() public {
        uint256[2] memory ks = _ks();
        for (uint256 i; i < ks.length; ++i) {
            uint256 k = ks[i];
            uint256 snap = vm.snapshotState();
            bytes32 idB = _start(k);
            uint256 forfeited = _callCloseAndForfeit();
            IIntexNFT1155.SeriesData memory data = tokenB.getIntexSeries(SERIES);
            uint64 closeAt = data.calledAt + data.callTrigger.callNoticePeriod + data.callTrigger.transitMargin;
            vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.BridgeAfterDeadline.selector, ID, closeAt));
            routerB.retryIntexCrosschainReceive(idB, 0);
            assertEq(tokenB.totalSupply(ID), 0);
            assertEq(_kept(routerB, idB), k, "a refused retry keeps the failed receive");
            _assertOpenUnits(idB, bytes32(0), forfeited);
            vm.revertToState(snap);
        }
    }

    function test_SendBackAfterTheForfeitMintsNothing() public {
        uint256[2] memory ks = _ks();
        for (uint256 i; i < ks.length; ++i) {
            uint256 k = ks[i];
            uint256 snap = vm.snapshotState();
            bytes32 idB = _start(k);
            uint256 forfeited = _callCloseAndForfeit();
            bytes32 idA = _sendBack(idB);
            assertEq(tokenA.totalSupply(ID), 0, "the source mints nothing past the transit close");
            assertEq(_kept(routerB, idB), 0);
            assertEq(_kept(routerA, idA), k);
            _assertOpenUnits(idB, idA, forfeited);
            vm.revertToState(snap);
        }
    }

    /// All the open units, and a part of them.
    function _ks() private pure returns (uint256[2] memory) {
        return [OPEN, 30];
    }

    /// Issue on A, settle and send to the GemFactory before the call, then move `k` open units to
    /// B, where the series is not open yet, so the mint fails and B keeps it.
    function _start(uint256 k) private returns (bytes32 idB) {
        tokenA.issueIntex(user, ISSUED, SERIES);
        tokenA.settleIntex(user, SERIES, SETTLED);
        tokenA.sendIntexToGemFactory(user, SERIES, TO_GEM_FACTORY, 1);
        idB = _send(k);
        assertEq(_kept(routerB, idB), k);
        _assertOpenUnits(idB, bytes32(0), 0);
    }

    function _callCloseAndForfeit() private returns (uint256 forfeited) {
        tokenB.openSeries(OpenSeriesLib.params(DAY, uint32(ISSUED), NOTICE));
        uint64 calledAt = uint64(block.timestamp);
        tokenA.callSeries(SERIES, calledAt);
        tokenB.callSeries(SERIES, calledAt);
        IIntexNFT1155.SeriesData memory data = tokenA.getIntexSeries(SERIES);
        vm.warp(uint256(calledAt) + data.callTrigger.callNoticePeriod + data.callTrigger.transitMargin + 1);
        assertEq(uint8(tokenA.getIntexSeries(SERIES).state), uint8(IIntexNFT1155.IntexState.Forfeited));
        forfeited = tokenA.balanceOf(user, ID);
        if (forfeited != 0) tokenA.forfeitIntex(user, SERIES);
    }

    function _send(uint256 k) private returns (bytes32 receiveId) {
        uint256[] memory ids = new uint256[](1);
        ids[0] = ID;
        uint256[] memory units = new uint256[](1);
        units[0] = k;
        BatchSendParam memory params =
            BatchSendParam({dstChainId: B_CHAIN_ID, to: bytes32(uint256(uint160(user))), intexIds: ids, units: units});
        uint256 fee = routerA.quoteBatchSend(params);
        vm.prank(user);
        routerA.batchSend{value: fee}(params);
        bytes memory packet = bridge.lastPayload();
        receiveId = keccak256(abi.encode(_interop(A_CHAIN_ID, address(routerA)), packet));
        _deliver(A_CHAIN_ID, address(routerA), address(routerB), packet);
    }

    function _sendBack(bytes32 idB) private returns (bytes32 idA) {
        uint256 fee = routerB.quoteSendBackToSource(idB, 0);
        routerB.sendBackToSource{value: fee}(idB, 0);
        bytes memory packet = bridge.lastPayload();
        idA = keccak256(abi.encode(_interop(B_CHAIN_ID, address(routerB)), packet));
        _deliver(B_CHAIN_ID, address(routerB), address(routerA), packet);
    }

    function _kept(IntexNFT1155Router router, bytes32 receiveId) private view returns (uint256 units) {
        bool exists;
        (,, units, exists) = router.getFailedCrosschainReceive(receiveId, 0);
        if (!exists) units = 0;
    }

    /// The open units are on A, on B, in a kept failed receive or burned by the forfeit, each once.
    function _assertOpenUnits(bytes32 idB, bytes32 idA, uint256 forfeited) private view {
        assertEq(
            tokenA.totalSupply(ID) + tokenB.totalSupply(ID) + _kept(routerB, idB) + _kept(routerA, idA) + forfeited,
            OPEN,
            "open units = on chains + kept + forfeited"
        );
    }
}
