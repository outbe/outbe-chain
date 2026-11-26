// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {CrossChainTest} from "../helpers/CrossChainTest.sol";

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IIntexNFT1155} from "@contracts/shared/interfaces/IIntexNFT1155.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {SendParam, MultiRecipientSendParam, RemoteRouter} from "@contracts/shared/interfaces/IIntexNFT1155Router.sol";
import {DeployProxy} from "../helpers/DeployProxy.sol";
import {OpenSeriesLib} from "../helpers/OpenSeriesLib.sol";

/// @dev A Called series still changes hands until its settlement deadline, on a chain and across
///      the bridge. Past the deadline its unpaid units are forfeited and stay where they are.
contract CalledSeriesTransfersTest is CrossChainTest {
    uint32 internal constant DST_CHAIN_ID = 2;
    uint32 internal constant EMISSION_DAY = 20260501;
    bytes14 internal constant SERIES_ID = "20260501-USD-U";

    IntexNFT1155 internal intex;
    IntexNFT1155Router internal nftRouter;
    address internal admin = address(this);
    address internal owner = address(0xCAFE);
    address internal stranger = address(0xBEEF);
    uint256 internal intexId;

    function setUp() public {
        _setUpBridge();
        address nftRouterProxy = DeployProxy.reserve();
        intex = DeployProxy.intexNFT1155(admin, DeployProxy.NftLinks(admin, nftRouterProxy, admin, admin, admin));
        RemoteRouter[] memory remotes = new RemoteRouter[](1);
        remotes[0] = RemoteRouter(DST_CHAIN_ID, _interop(DST_CHAIN_ID, address(0xD57)));
        nftRouter = DeployProxy.intexNFT1155RouterAt(nftRouterProxy, address(intex), address(bridge), admin, remotes);

        intex.openSeries(OpenSeriesLib.params(EMISSION_DAY, 10_000, 1 days));
        intex.issueIntex(owner, 10, SERIES_ID);
        intexId = intex.issuedIntexId(SERIES_ID);
        intex.callSeries(SERIES_ID, uint32(block.timestamp));
    }

    function _param(address to, uint256 amount) internal view returns (SendParam memory) {
        return SendParam({dstChainId: DST_CHAIN_ID, to: bytes32(uint256(uint160(to))), intexId: intexId, units: amount});
    }

    function test_ABridgeHopToAnotherAddressPasses() public {
        vm.prank(owner);
        nftRouter.send(_param(stranger, 4));

        assertEq(intex.balanceOf(owner, intexId), 6, "burned on the source, bound for the new owner");
    }

    function test_AMultiSendFansOutToOthers() public {
        bytes32[] memory to = new bytes32[](1);
        to[0] = bytes32(uint256(uint160(stranger)));
        uint256[] memory ids = new uint256[](1);
        ids[0] = intexId;
        uint256[] memory amounts = new uint256[](1);
        amounts[0] = 1;

        vm.prank(owner);
        nftRouter.multiSend(
            MultiRecipientSendParam({dstChainId: DST_CHAIN_ID, recipients: to, intexIds: ids, units: amounts})
        );
        assertEq(intex.balanceOf(owner, intexId), 9);
    }

    function test_APlainTransferPassesUntilTheDeadline() public {
        vm.warp(block.timestamp + 1 days);
        vm.prank(owner);
        intex.safeTransferFrom(owner, stranger, intexId, 1, "");

        assertEq(intex.balanceOf(stranger, intexId), 1);
    }

    function test_APlainTransferPastTheDeadlineIsRefused() public {
        uint32 deadline = uint32(block.timestamp + 1 days);
        vm.warp(uint256(deadline) + 1);
        vm.expectRevert(abi.encodeWithSelector(IIntexNFT1155.TransferAfterDeadline.selector, intexId, deadline));
        vm.prank(owner);
        intex.safeTransferFrom(owner, stranger, intexId, 1, "");
    }

    function test_BeforeTheCallTheBridgeStillCarriesToAnyone() public {
        intex.openSeries(OpenSeriesLib.params(20260502, 10_000, 1 days));
        bytes14 open = "20260502-USD-U";
        intex.issueIntex(owner, 5, open);

        uint256 openIntexId = intex.issuedIntexId(open);
        vm.prank(owner);
        nftRouter.send(
            SendParam({
                dstChainId: DST_CHAIN_ID, to: bytes32(uint256(uint160(stranger))), intexId: openIntexId, units: 2
            })
        );

        assertEq(intex.balanceOf(owner, openIntexId), 3, "an uncalled series stays freely bridgeable");
    }
}
