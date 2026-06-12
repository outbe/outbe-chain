// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {TestHelperOz5} from "@layerzerolabs/test-devtools-evm-foundry/contracts/TestHelperOz5.sol";
import {Origin} from "@layerzerolabs/oapp-evm/oapp/OApp.sol";

import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";
import {ITargetMessenger} from "@contracts/bnb/interfaces/ITargetMessenger.sol";
import {IOriginMessenger} from "@contracts/outbe/interfaces/IOriginMessenger.sol";
import {BridgeMsgCodec} from "@contracts/shared/libs/BridgeMsgCodec.sol";
import {IntexAuction} from "@contracts/bnb/IntexAuction.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {MockDesis} from "@test-mocks/MockDesis.sol";

/// @title InboundDropDontBlockTest
/// @notice The messengers run ORDERED LZ lanes; a deterministic revert in decode or a downstream
///         transition is caught so the nonce still advances and later packets keep flowing.
/// @dev Calls `lzReceive` directly from the endpoint address with an explicit nonce, bypassing the
///      LZ queue so the test controls the payload and nonce.
contract InboundDropDontBlockTest is TestHelperOz5 {
    uint32 internal constant BNB_EID = 1;
    uint32 internal constant OUTBE_EID = 2;
    bytes32 internal constant GUID = bytes32(uint256(0xCAFE));

    TargetMessenger internal bnbMessenger;
    OriginMessenger internal outbeMessenger;
    ONFT1155AdapterBatch internal onftBatchBnb;
    IntexAuction internal auction;
    IntexNFT1155 internal intex;
    address internal desis;
    address internal intexFactory;
    address internal admin = address(this);

    function setUp() public override {
        super.setUp();
        setUpEndpoints(2, LibraryType.UltraLightNode);

        desis = address(new MockDesis());
        intexFactory = makeAddr("factory");
        auction = DeployProxy.intexAuction(admin, admin);
        intex = DeployProxy.intexNFT1155(admin, admin);

        bnbMessenger = TargetMessenger(
            payable(_deployOApp(
                    type(TargetMessenger).creationCode, abi.encode(address(endpoints[BNB_EID]), admin, OUTBE_EID)
                ))
        );
        outbeMessenger = OriginMessenger(
            payable(_deployOApp(
                    type(OriginMessenger).creationCode, abi.encode(address(endpoints[OUTBE_EID]), admin, BNB_EID)
                ))
        );
        onftBatchBnb = new ONFT1155AdapterBatch(address(intex), address(endpoints[BNB_EID]), admin);

        address[] memory bridge = new address[](2);
        bridge[0] = address(bnbMessenger);
        bridge[1] = address(outbeMessenger);
        this.wireOApps(bridge);

        bnbMessenger.wire(address(auction), address(intex), admin, address(onftBatchBnb));
        outbeMessenger.wire(desis, intexFactory);
    }

    function _deliver(
        address oapp,
        address endpointAddr,
        uint32 srcEid,
        address peer,
        uint64 nonce,
        bytes memory message
    ) internal {
        Origin memory origin = Origin({srcEid: srcEid, sender: bytes32(uint256(uint160(peer))), nonce: nonce});
        vm.prank(endpointAddr);
        (bool ok,) = oapp.call(
            abi.encodeWithSignature(
                "lzReceive((uint32,bytes32,uint64),bytes32,bytes,address,bytes)", origin, GUID, message, address(0), ""
            )
        );
        require(ok, "lzReceive must not revert under drop-don't-block");
    }

    function test_OM_DropAdvancesNonceThenValidLands() public {
        bytes32 peer = bytes32(uint256(uint160(address(bnbMessenger))));

        // nonce 1: an unknown msgType is dropped — the lane must still advance.
        vm.expectEmit(true, true, false, true, address(outbeMessenger));
        emit IOriginMessenger.InboundMessageDropped(
            GUID, BNB_EID, abi.encodeWithSelector(BridgeMsgCodec.UnknownMsgType.selector, 0xFE)
        );
        _deliver(address(outbeMessenger), address(endpoints[OUTBE_EID]), BNB_EID, address(bnbMessenger), 1, hex"01FE");

        assertEq(outbeMessenger.inboundNonce(BNB_EID, peer), 1, "nonce advanced past the dropped packet");
        assertEq(outbeMessenger.nextNonce(BNB_EID, peer), 2, "next nonce ready");

        // nonce 2: a valid bids batch lands (BidsBatchReceived), proving the lane was not wedged.
        bytes memory bids = BridgeMsgCodec.encodeBidsBatch(
            42, BNB_EID, true, 1, new address[](0), new uint16[](0), new uint64[](0), new uint32[](0)
        );
        vm.expectEmit(true, true, false, true, address(outbeMessenger));
        emit IOriginMessenger.BidsBatchReceived(GUID, BNB_EID, 42, 0);
        _deliver(address(outbeMessenger), address(endpoints[OUTBE_EID]), BNB_EID, address(bnbMessenger), 2, bids);

        assertEq(outbeMessenger.inboundNonce(BNB_EID, peer), 2, "second packet processed");
    }

    function test_TM_AuthenticInapplicableTransitionDropped() public {
        bytes32 peer = bytes32(uint256(uint160(address(outbeMessenger))));

        // MARK_CALLED for a series the BNB intex has never seen: intex.markCalled reverts
        // deterministically. The packet must be dropped and the lane advance, not wedge.
        bytes memory packet = BridgeMsgCodec.encodeMarkCalled(99);
        vm.expectEmit(true, true, false, false, address(bnbMessenger));
        emit ITargetMessenger.InboundMessageDropped(GUID, OUTBE_EID, "");
        _deliver(address(bnbMessenger), address(endpoints[BNB_EID]), OUTBE_EID, address(outbeMessenger), 1, packet);

        assertEq(bnbMessenger.inboundNonce(OUTBE_EID, peer), 1, "nonce advanced past the dropped transition");
        assertEq(bnbMessenger.nextNonce(OUTBE_EID, peer), 2, "next nonce ready");
    }

    function test_OM_DispatchInbound_RevertsNotSelf() public {
        vm.expectRevert(IOriginMessenger.NotSelf.selector);
        outbeMessenger.dispatchInbound(GUID, BNB_EID, hex"01FE");
    }

    function test_TM_DispatchInbound_RevertsNotSelf() public {
        vm.expectRevert(ITargetMessenger.NotSelf.selector);
        bnbMessenger.dispatchInbound(GUID, OUTBE_EID, hex"01FE");
    }
}
