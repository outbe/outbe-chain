// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test} from "forge-std/Test.sol";

import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";

interface TargetRouter {
    function auction() external view returns (address);
}

/// @notice What the stage sends of Desis cost on the deployed testnet stack: router, hub, Hyperlane
///         adapter, mailbox and hooks. The tests skip without `OUTBE_TESTNET_RPC_URL`.
abstract contract OriginStageSendGasBase is Test {
    IOriginRouter internal constant ROUTER = IOriginRouter(0x62C1EFaCBfB20a864342B61A6dEC6f4d80729D18);
    address internal constant DESIS = 0x0000000000000000000000000000000000001016;
    address internal constant GOVERNANCE = 0x000000000000000000000000000000000000EE15;
    uint32 internal constant BSC = 97;
    uint32 internal constant SEPOLIA = 11155111;
    uint32 internal constant OUTBE = 54322345;
    uint32 internal constant DAY = 20991201;

    function setUp() public virtual {
        string memory rpc = vm.envOr("OUTBE_TESTNET_RPC_URL", string(""));
        if (bytes(rpc).length == 0) {
            vm.skip(true);
            return;
        }
        vm.createSelectFork(rpc);
        vm.deal(address(ROUTER), 1_000 ether);
    }

    function _params(uint32 day) internal view returns (IOriginRouter.AuctionStageStartParams memory p) {
        uint16[6] memory isoCodes = [uint16(840), 978, 392, 826, 156, 344];
        p.prices = new IOriginRouter.ReferenceCurrencyPrice[](isoCodes.length);
        for (uint256 i; i < isoCodes.length; ++i) {
            p.prices[i] = IOriginRouter.ReferenceCurrencyPrice(isoCodes[i], 2_000_000, 3_000_000);
        }
        p.emissionDay = day;
        p.commitEnd = uint32(block.timestamp + 1 days);
        p.revealEnd = uint32(block.timestamp + 2 days);
        p.issuanceEnd = uint32(block.timestamp + 3 days);
        p.promisLoadMinor = 1e12;
        p.minIntexBidRate = 1_000_000;
        p.callNoticePeriod = 86_400;
        p.callWindow = 86_400;
        p.callThreshold = 3_600;
        p.minIntexBidQuantity = 1;
        p.commitBondMinor = 1_000_000;
        p.dayState = 1;
    }

    function _start(uint32 day) internal returns (uint256 spent) {
        IOriginRouter.AuctionStageStartParams memory p = _params(day);
        vm.prank(DESIS);
        uint256 before = gasleft();
        ROUTER.sendAuctionStageStart(p);
        spent = before - gasleft();
    }

    function _collect(uint32 day, uint32 chainId) internal returns (uint256 spent) {
        vm.prank(DESIS);
        uint256 before = gasleft();
        ROUTER.sendAuctionStageCollectingBids(day, chainId, 2_000_000);
        spent = before - gasleft();
    }
}

/// @notice One cold START with one, two and three targets. Run with `--isolate`.
contract OriginStageStartGasTest is OriginStageSendGasBase {
    function test_startCost() public {
        vm.startPrank(GOVERNANCE);
        ROUTER.removeTarget(OUTBE);
        ROUTER.removeTarget(SEPOLIA);
        vm.stopPrank();
        emit log_named_uint("start_bsc", _start(DAY));
        vm.prank(GOVERNANCE);
        ROUTER.addTarget(SEPOLIA);
        emit log_named_uint("start_bsc_sepolia", _start(DAY + 1));
        vm.prank(GOVERNANCE);
        ROUTER.addTarget(OUTBE);
        emit log_named_uint("start_bsc_sepolia_outbe", _start(DAY + 2));
        assertEq(ROUTER.parkedMessageCount(), 0, "a send parked");
    }
}

/// @notice The COLLECTING_BIDS fan-out as the reveal end makes it: one transaction, the day started
///         in an earlier one. Run without `--isolate`.
contract OriginCollectingFanOutGasTest is OriginStageSendGasBase {
    function setUp() public override {
        super.setUp();
        _start(DAY);
    }

    function test_collectingFanOut() public {
        emit log_named_uint("fanout_bsc_first", _collect(DAY, BSC));
        emit log_named_uint("fanout_sepolia_next", _collect(DAY, SEPOLIA));
        emit log_named_uint("fanout_outbe_loopback", _collect(DAY, OUTBE));
        assertEq(ROUTER.parkedMessageCount(), 0, "a send parked");
    }
}

/// @notice The loopback COLLECTING_BIDS leg with bids on the Outbe target, at the round gas Desis asks
///         for its own chain. The delivery runs inside the send. Run without `--isolate`.
contract OriginLoopbackCollectingGasTest is OriginStageSendGasBase {
    address internal constant OUTBE_TARGET = 0x426F62D1F01930FEbfd760B10c294dae0eDbd468;

    function setUp() public override {
        super.setUp();
        _start(DAY);
        address auction = TargetRouter(OUTBE_TARGET).auction();
        vm.mockCall(auction, abi.encodeWithSelector(IIntexAuction.startAwaitingResult.selector), "");
        vm.mockCall(
            auction, abi.encodeWithSelector(IIntexAuction.revealedBidsCount.selector), abi.encode(uint256(4096))
        );
        IIntexAuction.SubmittedBidData[] memory bids = new IIntexAuction.SubmittedBidData[](64);
        for (uint256 i; i < bids.length; ++i) {
            bids[i].bidderAddress = address(uint160(0xB1D000 + i));
            bids[i].intexBidRate = 1_000_000;
            bids[i].intexQuantity = 1;
            bids[i].timestamp = 1_796_000_000;
            bids[i].issuanceCurrency = 840;
            bids[i].referenceCurrency = 840;
        }
        vm.mockCall(auction, abi.encodeWithSelector(IIntexAuction.revealedBidsSlice.selector), abi.encode(bids));
        vm.etch(DESIS, hex"600460005260206000f3");
    }

    function _loopback(uint256 ask) internal returns (uint256 spent) {
        vm.prank(DESIS);
        uint256 before = gasleft();
        ROUTER.sendAuctionStageCollectingBids(DAY, OUTBE, ask);
        spent = before - gasleft();
    }

    function test_loopbackAtTheOwnChainRoundGas() public {
        emit log_named_uint("loopback_ask_2_3m", _loopback(2_300_000));
        assertEq(ROUTER.parkedMessageCount(), 1, "the next round did not park");
    }
}
