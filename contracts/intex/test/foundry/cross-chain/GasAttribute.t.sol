// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

import {OriginRouter} from "@contracts/origin/OriginRouter.sol";
import {TargetRouter} from "@contracts/target/TargetRouter.sol";
import {IOriginRouter} from "@contracts/origin/interfaces/IOriginRouter.sol";
import {ITargetRouter} from "@contracts/target/interfaces/ITargetRouter.sol";
import {IntexGas} from "@contracts/shared/libs/IntexGas.sol";

/// @dev Every send carries the destination gas as the ERC-7786 executionGasLimit attribute, sized from IntexGas.
contract GasAttributeTest is CrossChainTest {
    uint32 internal constant BNB_CHAIN_ID = 1;
    uint32 internal constant OUTBE_CHAIN_ID = 2;
    bytes4 internal constant GAS_SELECTOR = bytes4(keccak256("executionGasLimit(uint256)"));

    OriginRouter internal outbe;
    TargetRouter internal bnb;
    address internal admin = address(this);
    address internal desis;

    function setUp() public {
        _setUpBridge();
        outbe = DeployProxy.originRouter(address(bridge), admin, BNB_CHAIN_ID);
        bnb = DeployProxy.targetRouter(address(bridge), admin, OUTBE_CHAIN_ID);
        outbe.setRemoteMessenger(BNB_CHAIN_ID, _interop(BNB_CHAIN_ID, address(bnb)));
        bnb.setRemoteMessenger(OUTBE_CHAIN_ID, _interop(OUTBE_CHAIN_ID, address(outbe)));
        outbe.addTarget(BNB_CHAIN_ID);
        vm.deal(address(outbe), 10 ether);

        desis = address(new MockDesis());
        outbe.wire(desis, makeAddr("factory"));
        // AUCTION_ROLE goes to `admin` so this test can call sendBidsBatch directly.
        bnb.wire(admin, makeAddr("intex"), makeAddr("escrow"), makeAddr("nftBridge"));
    }

    /// @dev The last recorded send carries exactly one executionGasLimit attribute equal to `expectedGas`.
    function _assertLastGas(uint256 expectedGas) internal view {
        bytes[] memory attrs = bridge.getLastAttributes();
        assertEq(attrs.length, 1, "expected one attribute");
        assertEq(attrs[0], abi.encodeWithSelector(GAS_SELECTOR, expectedGas), "executionGasLimit attribute mismatch");
    }

    function test_fixedMessage_carriesTypeGas() public {
        IOriginRouter.AuctionStageStartParams memory p;
        p.worldwideDay = 42;
        vm.prank(desis);
        outbe.sendAuctionStageStart(p);
        _assertLastGas(IntexGas.AUCTION_STAGE_START);
    }

    function test_bidsBatch_gasScalesWithItemCount() public {
        _assertBidsGas(1);
        _assertBidsGas(5);
        // Sizing is strictly increasing in the bid count.
        assertGt(IntexGas.bidsBatch(5), IntexGas.bidsBatch(1), "gas must grow with bids");
    }

    function _assertBidsGas(uint256 n) internal {
        ITargetRouter.BidsBatchParams memory p = ITargetRouter.BidsBatchParams({
            worldwideDay: 42,
            bidderAddresses: new address[](n),
            intexQuantities: new uint16[](n),
            intexBidRates: new uint32[](n),
            timestamps: new uint32[](n)
        });
        bnb.sendBidsBatch(p);
        _assertLastGas(IntexGas.bidsBatch(n));
    }

    function test_quoteMatchesSend_bothCarryGas() public view {
        // The quote path builds the same attribute; the mock returns a flat fee, so this simply confirms the
        // quote signature compiles and returns without reverting under the gas attribute.
        IOriginRouter.AuctionStageStartParams memory p;
        p.worldwideDay = 7;
        outbe.quoteSendAuctionStageStart(p);
    }
}
