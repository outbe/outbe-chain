// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "../shared/interfaces/IIntexNFT1155.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";
import {IERC7786TokenBridge} from "./interfaces/IERC7786TokenBridge.sol";

/// @notice Progress of a day's bids relay: the span that the first round froze, the next chunk to
///         send, and whether the completeness marker has left. Five bytes, so one slot.
struct BidsRelayProgress {
    uint16 nextBatch;
    uint16 totalBatches;
    bool done;
}

/// @notice Progress of a day's chunk run: the declared span and the number of chunks that landed.
struct ChunkProgress {
    uint16 totalChunks;
    uint16 chunksSeen;
}

/// @notice Progress of a day's refund run: the declared span and the number of chunks that landed.
struct RefundProgress {
    uint16 totalChunks;
    uint16 chunksSeen;
}

/// @custom:storage-location erc7201:outbe.intex.TargetRouter
struct TargetRouterStorage {
    /// @dev Auction contract that originates outbound bids and receives inbound stage transitions.
    IIntexAuction auction;
    /// @dev IntexNFT1155 contract that issuance and mark-called messages apply to.
    IIntexNFT1155 intex;
    /// @dev EscrowAdapter contract that refund instructions are forwarded to for finalization.
    IEscrowAdapter escrowAdapter;
    /// @dev Composed-transfer token bridge that routes auction proceeds to Outbe.
    IERC7786TokenBridge tokenBridge;
    /// @dev OriginRouter address on Outbe that receives and distributes the proceeds.
    address originRouter;
    /// @dev Per-day bids relay progress: a redelivered CLEARING resumes it rather than starting over.
    mapping(uint32 worldwideDay => BidsRelayProgress) bidsRelay;
    /// @dev Issuance-run progress for a day on this chain: the span the first applied chunk declared and
    ///      how many landed. Four bytes, so one slot rather than two.
    mapping(uint32 worldwideDay => ChunkProgress) issuanceProgress;
    /// @dev Bit per applied issuance chunk, so a repeat is not counted twice. Mirrors
    ///      `refundChunksApplied`. One word covers `MAX_CHUNKS`.
    mapping(uint32 worldwideDay => uint256 bitmap) issuanceChunksApplied;
    /// @dev Origin's call time of a Called mark waiting for its series to land here (0 = none), so a slot
    ///      applied later derives the same deadline.
    mapping(bytes14 seriesId => uint32 calledAt) parkedMarks;
    /// @dev Refund-run progress for a day. It holds:
    ///      - the span that the first applied chunk declared. A chunk that claims another total is
    ///        a conflict, so an under-totaled header cannot close the day early.
    ///      - the number of chunks that landed.
    ///      - the proceeds accrued so far. The router routes them as one transfer once every chunk
    ///        has arrived, so the day pays one bridge fee.
    ///      Twenty bytes, so one slot rather than three.
    mapping(uint32 worldwideDay => RefundProgress) refundProgress;
    /// @dev Bit per applied refund chunk, so a redelivered one neither re-counts nor
    ///      completes the day. One word covers `MAX_CHUNKS`.
    mapping(uint32 worldwideDay => uint256 bitmap) refundChunksApplied;
    /// @dev Parked proceeds routes awaiting permissionless retry, keyed by enqueue index.
    mapping(uint256 idx => ParkedProceeds) parkedProceeds;
    /// @dev Next index to assign in `parkedProceeds`. It is also the count ever enqueued.
    uint256 nextParkedProceedsIdx;
}

/// @notice A proceeds route parked because its outbound send reverted (e.g. relay float too low).
///         `resendParkedProceeds` retries it. The router already holds the WCOEN, so the record
///         snapshots only the chunk and its amount.
struct ParkedProceeds {
    uint32 worldwideDay;
    uint16 chunkIndex;
    uint128 amount;
    bool exists;
    bool done;
}
