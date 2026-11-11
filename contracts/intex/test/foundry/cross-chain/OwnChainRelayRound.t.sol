// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {Test, Vm} from "forge-std/Test.sol";
import {IERC7786GatewaySource, IERC7786Recipient} from "@openzeppelin/contracts/interfaces/draft-IERC7786.sol";
import {InteroperableAddress} from "@openzeppelin/contracts/utils/draft-InteroperableAddress.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {ReferenceCurrencyPriceLib} from "../helpers/ReferenceCurrencyPriceLib.sol";
import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {IDesis} from "@contracts/origin/interfaces/IDesis.sol";
import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {IGatewayQuote} from "@contracts/shared/interfaces/IGatewayQuote.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

/// @dev Supplies a settable number of revealed bids and takes both stage flips without effect.
contract BidsStub {
    uint256 public count;

    function setCount(uint256 n) external {
        count = n;
    }

    function auctionStart(
        uint32,
        IIntexAuction.DayState,
        IIntexAuction.AuctionSchedule calldata,
        IIntexAuction.AuctionParams calldata
    ) external {}

    function startAwaitingResult(uint32) external {}

    function getAuctionStage(uint32) external pure returns (IIntexAuction.AuctionStage) {
        return IIntexAuction.AuctionStage.AwaitingResult;
    }

    function revealedBidsCount(uint32) external view returns (uint256) {
        return count;
    }

    function revealedBidsSlice(uint32, uint256 offset, uint256 limit)
        external
        view
        returns (IIntexAuction.SubmittedBidData[] memory slice)
    {
        uint256 length = count;
        if (offset >= length) return new IIntexAuction.SubmittedBidData[](0);
        uint256 end = offset + limit;
        if (end > length) end = length;
        slice = new IIntexAuction.SubmittedBidData[](end - offset);
        for (uint256 i = 0; i < slice.length; ++i) {
            slice[i] = IIntexAuction.SubmittedBidData({
                bidderAddress: address(uint160(0xBEEF + offset + i)),
                intexBidRate: 100e6,
                intexQuantity: 1,
                timestamp: uint32(block.timestamp),
                issuanceCurrency: 840,
                referenceCurrency: 840
            });
        }
    }
}

/// @dev A transport like Hyperlane: a send only announces the message, and the relayer delivers
///      it later in a transaction of its own, under the gas limit of the message.
contract RelayedBridge is IERC7786GatewaySource, IGatewayQuote {
    using InteroperableAddress for bytes;

    bytes4 private constant GAS_LIMIT_SELECTOR = bytes4(keccak256("executionGasLimit(uint256)"));

    event Sent(bytes sender, bytes recipient, bytes payload, uint256 gasLimit);

    uint256 private _nonce;

    function supportsAttribute(bytes4) external pure returns (bool) {
        return true;
    }

    function quote(bytes calldata, bytes calldata) external pure returns (uint256) {
        return 0;
    }

    function quote(bytes calldata, bytes calldata, bytes[] calldata) external pure returns (uint256) {
        return 0;
    }

    function sendMessage(bytes calldata recipient, bytes calldata payload, bytes[] calldata attributes)
        external
        payable
        returns (bytes32)
    {
        uint256 gasLimit;
        for (uint256 i = 0; i < attributes.length; i++) {
            if (bytes4(attributes[i]) == GAS_LIMIT_SELECTOR) gasLimit = abi.decode(attributes[i][4:], (uint256));
        }
        emit Sent(InteroperableAddress.formatEvmV1(block.chainid, msg.sender), recipient, payload, gasLimit);
        return keccak256(abi.encode(address(this), ++_nonce));
    }

    function deliver(bytes calldata sender, bytes calldata recipient, bytes calldata payload, uint256 gasLimit)
        external
    {
        (, address target) = recipient.parseEvmV1Calldata();
        bytes4 magic = IERC7786Recipient(target).receiveMessage{gas: gasLimit}(
            keccak256(abi.encode(sender, payload)), sender, payload
        );
        require(magic == IERC7786Recipient.receiveMessage.selector, "delivery refused");
    }
}

/// @notice Outbe as its own target, on the same relayed transport as every other target.
contract OwnChainRelayRoundTest is Test {
    uint32 internal constant DAY = 20_260_801;

    RelayedBridge internal bridge;
    OriginRouter internal origin;
    TargetRouter internal target;
    BidsStub internal stub;
    address internal desis;
    address internal admin = address(this);
    uint32 internal self;

    function setUp() public {
        vm.recordLogs();
        bridge = new RelayedBridge();
        self = uint32(block.chainid);

        origin = DeployProxy.originRouter(address(bridge), admin);
        desis = address(new MockDesis());
        origin.wire(desis, makeAddr("factory"));

        target = DeployProxy.targetRouter(address(bridge), admin, self);
        stub = new BidsStub();
        target.wire(address(stub), makeAddr("intex"), makeAddr("escrow"));

        origin.setRemoteMessenger(self, InteroperableAddress.formatEvmV1(self, address(target)));
        origin.addTarget(self);
        target.setRemoteMessenger(self, InteroperableAddress.formatEvmV1(self, address(origin)));

        vm.deal(address(origin), 100 ether);
        vm.deal(address(target), 100 ether);

        IOriginRouter.AuctionStageStartParams memory p;
        p.prices = ReferenceCurrencyPriceLib.one(840, 1, 3);
        p.emissionDay = DAY;
        p.dayState = 1;
        vm.prank(desis);
        origin.sendAuctionStageStart(p);
        _relayAll();
        assertEq(origin.parkedMessageCount(), 0, "the start leg must land");
    }

    /// @dev A day too heavy for one round: the router sends the next round as any other leg, and
    ///      the rounds carry the day without a park or a push.
    function test_AnUnfinishedOwnChainDaySendsItsNextRound() public {
        stub.setCount(1_536);
        vm.expectCall(desis, abi.encodeWithSelector(IDesis.processBidsBatch.selector, DAY, self), 24);
        vm.expectCall(desis, abi.encodeCall(IDesis.processBidsDone, (DAY, self, 24, 1_536)), 1);

        vm.prank(desis);
        origin.sendAuctionStageCollectingBids(DAY, self, IntexGas.AUCTION_STAGE_COLLECTING_BIDS);
        uint256 rounds = _relayAll();

        (uint16 nextBatch, uint16 totalBatches, bool done) = target.bidsRelay(DAY);
        assertEq(totalBatches, 24, "1,536 bids span 24 chunks");
        assertGt(rounds, 1, "one round cannot carry the day");
        assertTrue(done, "the rounds carried the day");
        assertEq(nextBatch, totalBatches, "every chunk left");
        assertEq(origin.parkedMessageCount(), 0, "no round parks");
    }

    /// @dev Every page of a 130-bid day on Outbe reaches Desis, with no push from outside.
    function test_EveryPageOfAnOwnChainDayReachesDesis() public {
        stub.setCount(130);
        vm.expectCall(desis, abi.encodeWithSelector(IDesis.processBidsBatch.selector, DAY, self), 3);
        vm.expectCall(desis, abi.encodeCall(IDesis.processBidsDone, (DAY, self, 3, 130)), 1);

        vm.prank(desis);
        origin.sendAuctionStageCollectingBids(DAY, self, IntexGas.AUCTION_STAGE_COLLECTING_BIDS);
        _relayAll();

        assertEq(origin.parkedMessageCount(), 0, "no round parks");
    }

    /// @dev Delivers every announced message, each in a call of its own, until none is left.
    ///      Returns how many clearing rounds it delivered.
    function _relayAll() internal returns (uint256 rounds) {
        for (uint256 pass = 0; pass < 32; ++pass) {
            Vm.Log[] memory logs = vm.getRecordedLogs();
            uint256 delivered;
            for (uint256 i = 0; i < logs.length; ++i) {
                if (logs[i].emitter != address(bridge) || logs[i].topics[0] != RelayedBridge.Sent.selector) continue;
                (bytes memory sender, bytes memory recipient, bytes memory payload, uint256 gasLimit) =
                    abi.decode(logs[i].data, (bytes, bytes, bytes, uint256));
                if (uint8(payload[1]) == BridgeMsgCodec.MSG_AUCTION_STAGE_COLLECTING_BIDS) ++rounds;
                bridge.deliver(sender, recipient, payload, gasLimit);
                ++delivered;
            }
            if (delivered == 0) return rounds;
        }
        revert("the relay did not settle");
    }
}
