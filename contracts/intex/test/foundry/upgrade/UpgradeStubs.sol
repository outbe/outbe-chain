// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IntexNFT1155} from "@contracts/shared/IntexNFT1155.sol";
import {IntexAuction} from "@contracts/target/IntexAuction.sol";
import {EscrowAdapter} from "@contracts/target/EscrowAdapter.sol";
import {OriginMessenger} from "@contracts/origin/OriginMessenger.sol";
import {TargetMessenger} from "@contracts/target/TargetMessenger.sol";
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
    constructor(address tokenAddr, address lzEndpoint) ONFT1155Adapter(tokenAddr, lzEndpoint) {}

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

/// @dev v1.1 stub that exercises the `upgradeToAndCall` init-data path: it appends a field in a
///      fresh ERC-7201 namespace and sets it from a `reinitializer(2)` migration entrypoint.
contract IntexNFT1155V2Reinit is IntexNFT1155 {
    /// @custom:storage-location erc7201:outbe.intex.IntexNFT1155V2Reinit
    struct V2Storage {
        uint256 migratedFlag;
    }

    // keccak256(abi.encode(uint256(keccak256("outbe.intex.IntexNFT1155V2Reinit")) - 1)) & ~bytes32(uint256(0xff))
    bytes32 private constant _V2_SLOT = 0xa6131e184e5aae318840a83507194e5ed64c56b50a1ac526e8c519cdd8bb2200;

    function _v2() private pure returns (V2Storage storage $) {
        // solhint-disable-next-line no-inline-assembly
        assembly ("memory-safe") {
            $.slot := _V2_SLOT
        }
    }

    function initializeV2(uint256 flag) external reinitializer(2) {
        _v2().migratedFlag = flag;
    }

    function migratedFlag() external view returns (uint256) {
        return _v2().migratedFlag;
    }
}
