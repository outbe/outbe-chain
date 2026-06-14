// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuction} from "@contracts/bnb/IntexAuction.sol";
import {EscrowAdapter} from "@contracts/bnb/EscrowAdapter.sol";
import {OriginMessenger} from "@contracts/outbe/OriginMessenger.sol";
import {TargetMessenger} from "@contracts/bnb/TargetMessenger.sol";
import {ONFT1155Adapter} from "@contracts/shared/ONFT1155Adapter.sol";
import {ONFT1155AdapterBatch} from "@contracts/shared/ONFT1155AdapterBatch.sol";

/// @dev v1.1 upgrade stubs used by the upgrade drill. Each inherits the real implementation and
///      adds a single no-op view, so an upgrade exercises a genuinely new code path while reusing
///      the existing storage layout. Test-only; never deployed to production.
uint256 constant UPGRADE_PROBE = 42;

contract IntexNFT1155V2 is IntexNFT1155 {
    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

contract IntexAuctionV2 is IntexAuction {
    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

contract EscrowAdapterV2 is EscrowAdapter {
    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

contract OriginMessengerV2 is OriginMessenger {
    constructor(address lzEndpoint, uint32 bnbEid) OriginMessenger(lzEndpoint, bnbEid) {}

    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

contract TargetMessengerV2 is TargetMessenger {
    constructor(address lzEndpoint, uint32 outbeEid) TargetMessenger(lzEndpoint, outbeEid) {}

    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

contract ONFT1155AdapterV2 is ONFT1155Adapter {
    constructor(address tokenAddr, address lzEndpoint, uint32 outbeEid)
        ONFT1155Adapter(tokenAddr, lzEndpoint, outbeEid)
    {}

    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}

contract ONFT1155AdapterBatchV2 is ONFT1155AdapterBatch {
    constructor(address tokenAddr, address lzEndpoint) ONFT1155AdapterBatch(tokenAddr, lzEndpoint) {}

    function upgradeProbe() external pure returns (uint256) {
        return UPGRADE_PROBE;
    }
}
