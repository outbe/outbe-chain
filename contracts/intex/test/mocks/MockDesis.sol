// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC165} from "@openzeppelin/contracts/utils/introspection/IERC165.sol";

import {IDesis} from "@contracts/origin/interfaces/IDesis.sol";

/// @notice Minimal stand-in that advertises the `IDesis` interface via ERC-165.
/// @dev Lets `OriginRouter.wire` accept it during tests without pulling in the full
///      `Desis` dependency graph. Outbound-direction tests prank the wired address. No test
///      invokes the bid-processing path, so the mock does not implement the interface methods.
///      The mock intentionally avoids `is IDesis` to stay light.
contract MockDesis {
    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == type(IDesis).interfaceId || interfaceId == type(IERC165).interfaceId;
    }

    /// @dev Accepts every BIDS_BATCH delivery and discards it. Tests that exercise the inbound
    ///      OM dispatch path rely on this no-op so `_handleBidsBatch` can complete without
    ///      pulling in the full Desis dependency graph.
    function collectIntexAuctionBids(
        bytes32, /* receiveId */
        uint32, /* emissionDay */
        uint32, /* srcChainId */
        uint16, /* pageIndex */
        uint16, /* totalPages */
        address[] calldata, /* bidders */
        uint256[] calldata /* packedBids */
    )
        external {}

    /// @dev Accepts every BIDS_DONE completeness marker and discards it, as `collectIntexAuctionBids`
    ///      does.
    function completeIntexAuctionBids(
        bytes32, /* receiveId */
        uint32, /* emissionDay */
        uint32, /* srcChainId */
        uint16, /* totalPages */
        uint32 /* totalBids */
    )
        external {}

    /// @dev Open for intake by default, so a day under relay behaves as the common case.
    IDesis.AuctionStage private _stage = IDesis.AuctionStage.RevealingBids;

    function setAuctionStage(IDesis.AuctionStage stage) external {
        _stage = stage;
    }

    function auctionStage(
        uint32 /* seriesId */
    )
        external
        view
        returns (IDesis.AuctionStage)
    {
        return _stage;
    }

    function intexAuctionBidsCount(
        uint32 /* seriesId */
    )
        external
        pure
        returns (uint256)
    {
        return 0;
    }
}
