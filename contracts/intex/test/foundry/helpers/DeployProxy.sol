// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {Vm} from "forge-std/Vm.sol";

/// @dev Deploys implementation + ERC1967 proxy pairs for the UUPS contracts under test.
///      Returned handles point at the proxy; `bridger` receives the roles each contract gates its operator on.
library DeployProxy {
    Vm private constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));

    function intexNFT1155(address defaultAdmin, address bridger) internal returns (IntexNFT1155) {
        IntexNFT1155 impl = new IntexNFT1155();
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(IntexNFT1155.initialize, (defaultAdmin)));
        IntexNFT1155 nft = IntexNFT1155(address(proxy));
        bytes32 router = nft.ROUTER_ROLE();
        bytes32 bridge = nft.BRIDGE_ROLE();
        bytes32 auction = nft.AUCTION_ROLE();
        vm.startPrank(defaultAdmin);
        nft.grantRole(router, bridger);
        nft.grantRole(bridge, bridger);
        nft.grantRole(auction, bridger);
        vm.stopPrank();
        return nft;
    }

    function intexAuction(address defaultAdmin, address bridger) internal returns (IntexAuction) {
        IntexAuction impl = new IntexAuction();
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(IntexAuction.initialize, (defaultAdmin)));
        IntexAuction auction = IntexAuction(address(proxy));
        bytes32 role = auction.RELAYER_ROLE();
        vm.prank(defaultAdmin);
        auction.grantRole(role, bridger);
        return auction;
    }

    function escrowAdapter(address defaultAdmin, address bridger) internal returns (EscrowAdapter) {
        EscrowAdapter impl = new EscrowAdapter();
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(EscrowAdapter.initialize, (defaultAdmin)));
        EscrowAdapter escrow = EscrowAdapter(address(proxy));
        bytes32 role = escrow.RELAYER_ROLE();
        vm.prank(defaultAdmin);
        escrow.grantRole(role, bridger);
        return escrow;
    }

    function targetRouter(address bridge, address delegate, uint32 outbeChainId)
        internal
        returns (IntexAuctionTargetRouter)
    {
        return targetRouter(bridge, delegate, outbeChainId, address(0x7E57B12D6E));
    }

    function targetRouter(address bridge, address delegate, uint32 outbeChainId, address wcoenBridge)
        internal
        returns (IntexAuctionTargetRouter)
    {
        IntexAuctionTargetRouter impl = new IntexAuctionTargetRouter(bridge, outbeChainId, wcoenBridge);
        ERC1967Proxy proxy =
            new ERC1967Proxy(address(impl), abi.encodeCall(IntexAuctionTargetRouter.initialize, (delegate)));
        return IntexAuctionTargetRouter(payable(address(proxy)));
    }

    function intexNFT1155Bridge(address tokenAddr, address bridge, address delegate)
        internal
        returns (IntexNFT1155Router)
    {
        IntexNFT1155Router impl = new IntexNFT1155Router(tokenAddr, bridge);
        ERC1967Proxy proxy = new ERC1967Proxy(address(impl), abi.encodeCall(IntexNFT1155Router.initialize, (delegate)));
        return IntexNFT1155Router(payable(address(proxy)));
    }
}
