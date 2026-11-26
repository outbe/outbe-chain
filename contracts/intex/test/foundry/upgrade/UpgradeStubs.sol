// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {IntexAuctionTargetRouter} from "@contracts/target/IntexAuctionTargetRouter.sol";
import {IntexNFT1155Router} from "@contracts/shared/IntexNFT1155Router.sol";

/// @dev v1.1 upgrade stubs that the upgrade drill uses. Each stub inherits the real implementation
///      and adds a single no-op view. An upgrade thus exercises a genuinely new code path and
///      reuses the existing storage layout. Test-only. Never deployed to production.
uint256 constant UPGRADE_PROBE = 42;

contract IntexNFT1155V2 is IntexNFT1155 {
    constructor(address targetRouter, address nftRouter, address auction, address intexFactory, address gemFactory)
        IntexNFT1155(targetRouter, nftRouter, auction, intexFactory, gemFactory)
    {}

    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

contract IntexAuctionV2 is IntexAuction {
    constructor(address targetRouter, address escrow, address nft) IntexAuction(targetRouter, escrow, nft) {}

    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

contract EscrowAdapterV2 is EscrowAdapter {
    constructor(address auction, address targetRouter) EscrowAdapter(auction, targetRouter) {}

    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

contract IntexAuctionTargetRouterV2 is IntexAuctionTargetRouter {
    constructor(
        address bridge,
        uint32 outbeChainId,
        address wcoenBridge,
        address auction,
        address nft,
        address escrow,
        address outbeIca
    ) IntexAuctionTargetRouter(bridge, outbeChainId, wcoenBridge, auction, nft, escrow, outbeIca) {}

    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

contract IntexNFT1155RouterV2 is IntexNFT1155Router {
    constructor(address token, address bridge) IntexNFT1155Router(token, bridge) {}

    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

/// @dev v1.1 stub that exercises the `upgradeToAndCall` init-data path. It appends a field in a
///      fresh ERC-7201 namespace and sets it from a `reinitializer(2)` migration entrypoint.
contract IntexNFT1155V2Reinit is IntexNFT1155 {
    constructor(address targetRouter, address nftRouter, address auction, address intexFactory, address gemFactory)
        IntexNFT1155(targetRouter, nftRouter, auction, intexFactory, gemFactory)
    {}

    /// @custom:storage-location erc7201:outbe.intex.IntexNFT1155V2Reinit
    struct V2Storage {
        uint256 migratedFlag;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.IntexNFT1155V2Reinit")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _V2_SLOT = 0xa6131e184e5aae318840a83507194e5ed64c56b50a1ac526e8c519cdd8bb2200;

    function _v2() private pure returns (V2Storage storage $) {
        assembly ("memory-safe") {
            $.slot := _V2_SLOT
        }
    }

    function initializeV2(uint256 flag) external reinitializer(2) {
        _v2().migratedFlag = flag;
    }
}
