// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {ERC1967Proxy} from "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";
import {UUPSUpgradeable} from "@openzeppelin/contracts-upgradeable/proxy/utils/UUPSUpgradeable.sol";
import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";
import {RemoteRouter} from "@contracts/shared/interfaces/IIntexNFT1155Router.sol";

/// @dev The first implementation of a reserved proxy: anyone may move it to the real one.
contract ProxySlot is UUPSUpgradeable {
    function reserve() external {}

    function _authorizeUpgrade(address) internal override {}
}

/// @dev Deploys the UUPS contracts under test behind ERC1967 proxies. Every link is an
///      implementation immutable, so a test that links contracts to each other reserves their
///      proxies first (`reserve`) and fills each with its implementation (`*At`).
library DeployProxy {
    address internal constant WCOEN_BRIDGE = address(0x7E57B12D6E);

    /// @dev Links of IntexNFT1155, in constructor order.
    struct NftLinks {
        address targetRouter;
        address intexNFT1155Router;
        address intexAuction;
        address intexFactory;
        address gemFactory;
    }

    /// @dev Links of IntexAuctionTargetRouter besides the bridge, in constructor order.
    struct TargetRouterLinks {
        uint32 outbeChainId;
        address wcoenBridge;
        address intexAuction;
        address intexNFT1155;
        address escrow;
        address outbeIca;
    }

    /// @dev A proxy whose implementation is set later by one of the `*At` functions.
    function reserve() internal returns (address) {
        return address(new ERC1967Proxy(address(new ProxySlot()), abi.encodeCall(ProxySlot.reserve, ())));
    }

    /// @dev Every link of the collection is `caller`.
    function intexNFT1155(address owner, address caller) internal returns (IntexNFT1155) {
        return intexNFT1155(owner, NftLinks(caller, caller, caller, caller, caller));
    }

    function intexNFT1155(address owner, NftLinks memory l) internal returns (IntexNFT1155) {
        return intexNFT1155At(reserve(), owner, l);
    }

    function intexNFT1155At(address proxy, address owner, NftLinks memory l) internal returns (IntexNFT1155) {
        _fill(
            proxy,
            address(
                new IntexNFT1155(l.targetRouter, l.intexNFT1155Router, l.intexAuction, l.intexFactory, l.gemFactory)
            ),
            abi.encodeCall(IntexNFT1155.initialize, (owner))
        );
        return IntexNFT1155(proxy);
    }

    function intexAuction(address owner, address router, address escrow, address nft) internal returns (IntexAuction) {
        return intexAuctionAt(reserve(), owner, router, escrow, nft);
    }

    function intexAuctionAt(address proxy, address owner, address router, address escrow, address nft)
        internal
        returns (IntexAuction)
    {
        _fill(proxy, address(new IntexAuction(router, escrow, nft)), abi.encodeCall(IntexAuction.initialize, (owner)));
        return IntexAuction(proxy);
    }

    function escrowAdapter(address owner, address auction, address router, address compact, address asset)
        internal
        returns (EscrowAdapter)
    {
        return escrowAdapterAt(reserve(), owner, auction, router, compact, asset);
    }

    function escrowAdapterAt(
        address proxy,
        address owner,
        address auction,
        address router,
        address compact,
        address asset
    ) internal returns (EscrowAdapter) {
        _fill(
            proxy,
            address(new EscrowAdapter(auction, router)),
            abi.encodeCall(EscrowAdapter.initialize, (owner, compact, asset))
        );
        return EscrowAdapter(payable(proxy));
    }

    function targetRouter(address bridge, address owner, TargetRouterLinks memory l)
        internal
        returns (IntexAuctionTargetRouter)
    {
        return targetRouterAt(reserve(), bridge, owner, l);
    }

    function targetRouterAt(address proxy, address bridge, address owner, TargetRouterLinks memory l)
        internal
        returns (IntexAuctionTargetRouter)
    {
        _fill(
            proxy,
            address(
                new IntexAuctionTargetRouter(
                    bridge, l.outbeChainId, l.wcoenBridge, l.intexAuction, l.intexNFT1155, l.escrow, l.outbeIca
                )
            ),
            abi.encodeCall(IntexAuctionTargetRouter.initialize, (owner))
        );
        return IntexAuctionTargetRouter(payable(proxy));
    }

    function intexNFT1155Router(address token, address bridge, address owner) internal returns (IntexNFT1155Router) {
        return intexNFT1155Router(token, bridge, owner, new RemoteRouter[](0));
    }

    function intexNFT1155Router(address token, address bridge, address owner, RemoteRouter[] memory remoteRouters)
        internal
        returns (IntexNFT1155Router)
    {
        return intexNFT1155RouterAt(reserve(), token, bridge, owner, remoteRouters);
    }

    function intexNFT1155RouterAt(
        address proxy,
        address token,
        address bridge,
        address owner,
        RemoteRouter[] memory remoteRouters
    ) internal returns (IntexNFT1155Router) {
        _fill(
            proxy,
            address(new IntexNFT1155Router(token, bridge)),
            abi.encodeCall(IntexNFT1155Router.initialize, (owner, remoteRouters))
        );
        return IntexNFT1155Router(payable(proxy));
    }

    function _fill(address proxy, address implementation, bytes memory initData) private {
        UUPSUpgradeable(proxy).upgradeToAndCall(implementation, initData);
    }
}
