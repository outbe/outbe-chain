// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {CreateSeriesLib} from "../helpers/CreateSeriesLib.sol";
import {MarkBatchLib} from "../helpers/MarkBatchLib.sol";

/// @dev The origin buys destination gas from `IntexGas` before it knows what the target will spend.
///      These pin the formula against the real consumption of a full batch, on both the applying and
///      the parking path — a formula that drifts under actual cost strands the message in redelivery.
contract MarkGasBudgetTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    uint32 internal constant WORLDWIDE_DAY = 20260501;
    bytes14 internal constant SERIES_PREFIX = "20260501-USD-U";
    bytes14 internal constant POISON = "20260501-EUR-U";

    TargetRouter internal router;
    IntexNFT1155 internal intex;
    address internal admin = address(this);
    address internal originPeer = address(0x0B1);

    function setUp() public {
        _setUpBridge();
        intex = DeployProxy.intexNFT1155(admin, admin);
        router = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        router.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, originPeer));
        router.wire(makeAddr("auction"), address(intex), makeAddr("escrow"));
        intex.grantRole(intex.RELAYER_ROLE(), address(router));
    }

    function _measure(bytes14[] memory batch) internal returns (uint256 spent) {
        bytes memory packet = BridgeMsgCodec.encodeMarkCalled(WORLDWIDE_DAY, uint32(block.timestamp), batch);
        uint256 before = gasleft();
        _deliver(OUTBE_CHAIN_ID, originPeer, address(router), packet);
        spent = before - gasleft();
    }

    function _seed(bytes14[] memory batch) internal {
        for (uint256 i = 0; i < batch.length; ++i) {
            IIntexNFT1155.CreateSeriesParams memory p = CreateSeriesLib.params(WORLDWIDE_DAY, 10_000, 0);
            p.seriesId = batch[i];
            intex.createSeries(p);
        }
    }

    function test_TheQuoteCoversAFullBatchThatApplies() public {
        bytes14[] memory batch = MarkBatchLib.sized(SERIES_PREFIX, BridgeMsgCodec.MAX_SERIES_PER_MARK);
        _seed(batch);

        uint256 spent = _measure(batch);

        assertLt(spent, IntexGas.markCalled(batch.length), "a full applying batch must fit the quote");
    }

    function test_TheQuoteCoversAFullBatchThatParks() public {
        // No series exist, so every mark reverts and parks — the heavier of the two paths.
        bytes14[] memory batch = MarkBatchLib.sized(SERIES_PREFIX, BridgeMsgCodec.MAX_SERIES_PER_MARK);

        uint256 spent = _measure(batch);

        assertEq(router.nextPendingMarkIdx(), batch.length, "every series parked");
        assertLt(spent, IntexGas.markCalled(batch.length), "a full parking batch must fit the quote");
    }

    // A series whose mark runs away must not take its batch mates with it. The delivery runs inside the
    // gas the quote actually buys — with an uncapped child that budget is gone before the parking write,
    // and the whole message reverts into redelivery.
    function test_ARunawaySeriesIsParkedAndTheBatchContinues() public {
        GasBurningIntex poisoned = new GasBurningIntex(POISON);
        TargetRouter mocked = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        mocked.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, originPeer));
        mocked.wire(makeAddr("auction"), address(poisoned), makeAddr("escrow"));

        bytes memory packet = BridgeMsgCodec.encodeMarkCalled(
            WORLDWIDE_DAY, uint32(block.timestamp), MarkBatchLib.two(POISON, SERIES_PREFIX)
        );
        bytes memory call = abi.encodeCall(
            bridge.deliverAs,
            (
                _interop(OUTBE_CHAIN_ID, originPeer),
                _interop(uint32(block.chainid), address(mocked)),
                packet
            )
        );
        (bool ok,) = address(bridge).call{gas: IntexGas.markCalled(2)}(call);

        assertTrue(ok, "the message must land, not bounce back into redelivery");
        assertEq(mocked.nextPendingMarkIdx(), 1, "only the runaway parked");
        (bytes14 parked,,,,) = mocked.pendingMarks(0);
        assertEq(parked, POISON, "the runaway is the parked one");
        assertEq(poisoned.markedCount(), 1, "its batch mate still applied");
    }
}

/// @dev Stands in for IntexNFT1155 so one series can be made to burn everything it is given.
contract GasBurningIntex {
    bytes14 private immutable poison;
    uint256 public markedCount;

    constructor(bytes14 _poison) {
        poison = _poison;
    }

    function markCalled(bytes14 seriesId, uint32) external {
        if (seriesId == poison) {
            // Spin until the frame's gas is gone.
            while (true) {
                markedCount = markedCount;
            }
        }
        markedCount++;
    }
}
