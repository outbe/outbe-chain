// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IIntexAuction} from "./interfaces/IIntexAuction.sol";
import {IIntexNFT1155} from "../shared/interfaces/IIntexNFT1155.sol";
import {IEscrowAdapter} from "./interfaces/IEscrowAdapter.sol";
import {IERC7786TokenBridge} from "./interfaces/IERC7786TokenBridge.sol";
import {IVwapRegistry} from "./interfaces/IVwapRegistry.sol";

/// @notice How far a day's bids relay has got: the span the first round froze, the next chunk to send and
///         whether the completeness marker has left. Five bytes, so one slot.
struct BidsRelayProgress {
    uint16 nextBatch;
    uint16 totalBatches;
    bool done;
}

/// @notice An issuance parked because a recipient's ERC-1155 receiver hook reverted; retried via
///         `applyParkedIssuance`.
struct ParkedIssuance {
    bytes14 seriesId;
    address recipient;
    uint256 quantity;
    bool exists;
    bool done;
}

/// @notice How far a day's chunk run has got: the declared span and how many chunks landed.
struct ChunkProgress {
    uint16 totalChunks;
    uint16 chunksSeen;
}

/// @notice As {ChunkProgress}, plus the proceeds a refund run has accrued so far.
struct RefundProgress {
    uint16 totalChunks;
    uint16 chunksSeen;
    uint128 proceedsAccrued;
}

/// @custom:storage-location erc7201:outbe.intex.TargetRouter
struct TargetRouterStorage {
    /// @dev Auction contract that originates outbound bids and receives inbound stage transitions.
    IIntexAuction auction;
    /// @dev IntexNFT1155 contract that issuance and mark-called messages apply to.
    IIntexNFT1155 intex;
    /// @dev EscrowAdapter contract that refund instructions are forwarded to for finalization.
    IEscrowAdapter escrowAdapter;
    /// @dev Registry the daily VWAPs from Outbe are recorded in; a day arriving while it is unset reverts.
    IVwapRegistry vwapRegistry;
    /// @dev Composed-transfer token bridge that routes auction proceeds to Outbe.
    IERC7786TokenBridge tokenBridge;
    /// @dev OriginRouter address on Outbe that receives and distributes the proceeds.
    address originRouter;
    /// @dev Per-day counter stamped on every BIDS_BATCH of the day's relay. Bumped once, when the first
    ///      round starts: every round of the same day shares it, so the receiver collects them together.
    mapping(uint32 worldwideDay => uint32 generation) bidsRelayGeneration;
    /// @dev Per-day bids relay progress: a redelivered CLEARING resumes it rather than starting over.
    mapping(uint32 worldwideDay => BidsRelayProgress) bidsRelay;
    /// @dev Issuance-run progress for a day on this chain: the span the first applied chunk declared and
    ///      how many landed. Four bytes, so one slot rather than two.
    mapping(uint32 worldwideDay => ChunkProgress) issuanceProgress;
    /// @dev Bit per applied issuance chunk, so a repeat neither issues nor counts. Mirrors
    ///      `refundChunksApplied`; one word covers `MAX_CHUNKS`.
    mapping(uint32 worldwideDay => uint256 bitmap) issuanceChunksApplied;
    /// @dev Winners already issued their allocation of a series; a repeated instruction for the pair is ignored.
    mapping(bytes14 seriesId => mapping(address recipient => bool issued)) issued;
    /// @dev Parked issuances awaiting permissionless retry, keyed by enqueue index.
    mapping(uint256 idx => ParkedIssuance) parkedIssuance;
    /// @dev Next index to assign in `parkedIssuance`; also the count ever enqueued.
    uint256 nextParkedIssuanceIdx;
    /// @dev Origin's call time of a Called mark waiting for its series to land here (0 = none), so a slot
    ///      applied later derives the same deadline.
    mapping(bytes14 seriesId => uint32 calledAt) parkedMarks;
    /// @dev Refund-run progress for a day: the span the first applied chunk declared (a chunk claiming
    ///      another total is a conflict, so an under-totaled header cannot close the day early), how many
    ///      landed, and the proceeds accrued so far - routed as one transfer once every chunk has arrived,
    ///      because the origin marks a chain paid on first delivery and a partial sum would close the
    ///      creator-reward fan-in early. Twenty bytes, so one slot rather than three.
    mapping(uint32 worldwideDay => RefundProgress) refundProgress;
    /// @dev Bit per applied refund chunk, so a redelivered one neither re-counts nor
    ///      completes the day. One word covers `MAX_CHUNKS`.
    mapping(uint32 worldwideDay => uint256 bitmap) refundChunksApplied;
    /// @dev Parked proceeds routes awaiting permissionless retry, keyed by enqueue index.
    mapping(uint256 idx => ParkedProceeds) parkedProceeds;
    /// @dev Next index to assign in `parkedProceeds`; also the count ever enqueued.
    uint256 nextParkedProceedsIdx;
}

/// @notice A proceeds route parked because its outbound send reverted (e.g. relay float too low); retried
///         via `resendParkedProceeds`. The WCOEN is already held here, so only series+amount is snapshotted.
struct ParkedProceeds {
    uint32 worldwideDay;
    uint128 amount;
    bool exists;
    bool done;
}
