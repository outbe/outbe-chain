// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "../shared/interfaces/IIntexNFT1155.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";

/// @notice Progress of a day's bids relay: the span that the first round froze, the next chunk to
///         send, and whether the completeness marker has left. Five bytes, so one slot.
struct IntexAuctionBidsSendState {
    uint16 nextPage;
    uint16 totalPages;
    bool done;
}

/// @notice Progress of a day's chunk run: the declared span and the number of chunks that landed.
struct IntexIssuanceState {
    uint16 totalPages;
    uint16 pagesSeen;
}

/// @notice Progress of a day's refund run: the declared span and the number of chunks that landed.
struct IntexAuctionReturnState {
    uint16 totalPages;
    uint16 pagesSeen;
}

/// @notice The contracts the router drives, implementation immutables passed to its inbound handlers.
struct TargetLinks {
    IIntexAuction intexAuction;
    IIntexNFT1155 intexContract;
    IEscrowAdapter escrow;
}

/// @custom:storage-location erc7201:outbe.intex.IntexAuctionTargetRouter
struct IntexAuctionTargetRouterStorage {
    /// @dev Per-day bids relay progress: a redelivered CLEARING resumes it rather than starting over.
    mapping(uint32 emissionDay => IntexAuctionBidsSendState) intexAuctionBidsSendState;
    /// @dev Issuance-run progress for a day on this chain: the span the first applied chunk declared and
    ///      how many landed. Four bytes, so one slot rather than two.
    mapping(uint32 emissionDay => IntexIssuanceState) intexIssuanceState;
    /// @dev Bit per applied issuance chunk, so a repeat is not counted twice. Mirrors
    ///      `intexAuctionWinnersPagesApplied`. One word covers `MAX_PAGES`.
    mapping(uint32 emissionDay => uint256 bitmap) intexSeriesPagesApplied;
    /// @dev Origin's call time of a Called mark waiting for its series to land here (0 = none), so a slot
    ///      applied later derives the same deadline.
    mapping(bytes14 seriesId => uint64 calledAt) failedIntexSeriesCallNoticeApplies;
    /// @dev Refund-run progress for a day. It holds:
    ///      - the span that the first applied chunk declared. A chunk that claims another total is
    ///        a conflict, so an under-totaled header cannot close the day early.
    ///      - the number of chunks that landed.
    ///      Each chunk routes its own proceeds, so nothing accrues here.
    mapping(uint32 emissionDay => IntexAuctionReturnState) intexAuctionReturnState;
    /// @dev Bit per applied refund chunk, so a redelivered one neither re-counts nor
    ///      completes the day. One word covers `MAX_PAGES`.
    mapping(uint32 emissionDay => uint256 bitmap) intexAuctionWinnersPagesApplied;
    /// @dev Parked proceeds routes awaiting permissionless retry, keyed by enqueue index.
    mapping(uint256 idx => FailedIntexAuctionProceedsSend) failedIntexAuctionProceedsSend;
    /// @dev Next index to assign in `failedIntexAuctionProceedsSend`. It is also the count ever enqueued.
    uint256 nextFailedIntexAuctionProceedsSendIdx;
    /// @dev Hash of the result the day took, so a redelivered result tells a duplicate from a conflict.
    mapping(uint32 emissionDay => bytes32) intexAuctionResultHash;
}

/// @notice A proceeds route parked because its outbound send reverted (e.g. relay float too low).
///         `retryIntexAuctionProceedsSend` retries it. The router already holds the WCOEN, so the record
///         snapshots only the chunk and its amount.
struct FailedIntexAuctionProceedsSend {
    uint32 emissionDay;
    uint16 pageIndex;
    uint128 amount;
    bool exists;
    bool done;
}
