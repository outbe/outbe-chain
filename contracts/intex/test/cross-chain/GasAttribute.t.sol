// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../utils/CrossChainTest.sol";
import {DeployProxy} from "../utils/DeployProxy.sol";
import {IERC7786GatewaySource} from "@openzeppelin/contracts/interfaces/draft-IERC7786.sol";

import {DESIS} from "@precompiles/IDesis.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IIntexAuction} from "@contracts/target/interfaces/IIntexAuction.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";

/// @dev Supplies a settable number of revealed bids and takes the clearing flip without effect.
contract GasAttributeBidsStub {
    uint256 public count;

    function setCount(uint256 n) external {
        count = n;
    }

    function startAuctionClearing(uint32) external {}

    function auctionStage(uint32) external pure returns (IIntexAuction.AuctionStage) {
        return IIntexAuction.AuctionStage.AwaitingResult;
    }

    function revealedIntexAuctionBidsCount(uint32) external view returns (uint256) {
        return count;
    }

    function revealedIntexAuctionBidsSlice(uint32, uint256 offset, uint256 limit)
        external
        view
        returns (IIntexAuction.SubmittedIntexAuctionBidData[] memory slice)
    {
        uint256 end = offset + limit;
        if (end > count) end = count;
        slice = new IIntexAuction.SubmittedIntexAuctionBidData[](end > offset ? end - offset : 0);
        for (uint256 i = 0; i < slice.length; ++i) {
            slice[i] = bid(offset + i);
        }
    }

    function bid(uint256 i) public view returns (IIntexAuction.SubmittedIntexAuctionBidData memory) {
        return IIntexAuction.SubmittedIntexAuctionBidData({
            bidder: address(uint160(0xBEEF + i)),
            intexBidRate: 100e6,
            intexQuantity: 1,
            timestamp: uint32(block.timestamp),
            issuanceCurrency: 840,
            referenceCurrency: 840
        });
    }
}

/// @dev Every bids send carries the destination gas as the ERC-7786 executionGasLimit attribute, sized from IntexGas.
contract GasAttributeTest is CrossChainTest {
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    bytes4 internal constant GAS_SELECTOR = bytes4(keccak256("executionGasLimit(uint256)"));

    IntexAuctionTargetRouter internal bnb;
    GasAttributeBidsStub internal stub;
    address internal admin = address(this);
    address internal outbeIca = makeAddr("outbeIca");

    function setUp() public {
        _setUpBridge();
        bnb = IntexAuctionTargetRouter(payable(DeployProxy.reserve()));
        stub = new GasAttributeBidsStub();
        DeployProxy.targetRouterAt(
            address(bnb),
            address(bridge),
            admin,
            DeployProxy.TargetRouterLinks(
                OUTBE_CHAIN_ID, DeployProxy.WCOEN_BRIDGE, address(stub), makeAddr("intex"), makeAddr("escrow"), outbeIca
            )
        );
        _wireOutbeIca(bnb, outbeIca);
        vm.deal(address(bnb), 10 ether);
    }

    function _attributes(uint256 gasLimit) internal pure returns (bytes[] memory attrs) {
        attrs = new bytes[](1);
        attrs[0] = abi.encodeWithSelector(GAS_SELECTOR, gasLimit);
    }

    /// @dev The last recorded send carries exactly one executionGasLimit attribute equal to `expectedGas`.
    function _assertLastGas(uint256 expectedGas) internal view {
        bytes[] memory attrs = bridge.getLastAttributes();
        assertEq(attrs.length, 1, "expected one attribute");
        assertEq(attrs[0], abi.encodeWithSelector(GAS_SELECTOR, expectedGas), "executionGasLimit attribute mismatch");
    }

    /// @dev Relay a one-page day of `n` bids and expect its page to buy `IntexGas.bidsPage(n)`.
    function _assertPageGas(uint32 day, uint256 n) internal {
        stub.setCount(n);
        address[] memory bidders = new address[](n);
        uint256[] memory packed = new uint256[](n);
        for (uint256 i = 0; i < n; ++i) {
            IIntexAuction.SubmittedIntexAuctionBidData memory b = stub.bid(i);
            bidders[i] = b.bidder;
            packed[i] = BridgeMsgCodec.packBid(
                b.intexQuantity, b.intexBidRate, b.timestamp, b.issuanceCurrency, b.referenceCurrency
            );
        }
        bytes memory page = BridgeMsgCodec.encodeBidsPage(day, uint32(block.chainid), 0, 1, bidders, packed);
        vm.expectCall(
            address(bridge),
            abi.encodeCall(
                IERC7786GatewaySource.sendMessage,
                (_interop(OUTBE_CHAIN_ID, DESIS), page, _attributes(IntexGas.bidsPage(n)))
            )
        );
        _deliver(OUTBE_CHAIN_ID, outbeIca, address(bnb), BridgeMsgCodec.encodeIntexAuctionClearingStart(day));
        _assertLastGas(IntexGas.BIDS_TOTALS);
    }

    function test_bidsPage_gasScalesWithItemCount() public {
        _assertPageGas(42, 1);
        _assertPageGas(43, 5);
        assertGt(IntexGas.bidsPage(5), IntexGas.bidsPage(1), "gas must grow with item count");
    }

    function test_bidsPause_carriesTypeGas() public {
        vm.prank(address(bnb));
        bnb.sendIntexAuctionBidsPause(42);
        _assertLastGas(IntexGas.BIDS_PAUSE);
    }
}
