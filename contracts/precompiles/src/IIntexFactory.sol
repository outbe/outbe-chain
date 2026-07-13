// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IIntexFactory
/// @notice User-facing call surface for the IntexFactory runtime precompile:
///         settlement, Promis mining, and the dual-wallet authorized-settler
///         setter. Issuance is a module-to-module call (Desis → IntexFactory)
///         exposed through the Rust `api`, not a precompile selector. Series
///         identity + lifecycle live in Intex; this precompile owns
///         settlement bookkeeping and the autonomous qualification index.
interface IIntexFactory {
    /// @notice Settle `amount` Issued Intexes of `seriesId` held by
    ///         `intexHolder`. Caller must be the holder or its authorized
    ///         settler. Allowed in Qualified (voluntary) and Called (forced).
    function settle(uint32 seriesId, address intexHolder, uint256 amount) external;

    /// @notice Burn settled Intexes and mint Promis, gated by off-chain proof
    ///         of work. Caller is the holder. Returns the minted Promis amount.
    function minePromis(uint32 seriesId, uint256 amount, uint256 nonce) external returns (uint256 promisAmount);

    /// @notice Authorize `settler` to settle the caller's position in `seriesId`.
    function setAuthorizedSettler(uint32 seriesId, address settler) external;

    /// @notice Distribute auction proceeds (native COEN, sent as msg.value) to
    ///         the contributing tribute owners of `seriesId`, proportional to
    ///         each owner's Tribute Nominal Amount. Callable only by the
    ///         OriginRouter. Registers the distribution; every payout is drained
    ///         over later blocks by the begin-block hook.
    function distribute(uint32 seriesId) external payable;

    /// @notice A new series was created from a cleared auction.
    event SeriesIssued(uint32 indexed seriesId, uint32 issuedIntexCount, uint256 entryPrice);

    /// @notice `amount` Issued Intexes of `seriesId` were settled.
    event Settled(uint32 indexed seriesId, address indexed intexHolder, address indexed settler, uint256 amount);

    /// @notice Settled Intexes were burned and `promisAmount` Promis minted.
    event PromisMined(uint32 indexed seriesId, address indexed holder, uint256 amount, uint256 promisAmount);

    /// @notice The series qualified (Issued → Qualified).
    event SeriesQualified(uint32 indexed seriesId);

    /// @notice The series was force-called (Qualified → Called).
    event SeriesCalled(uint32 indexed seriesId, uint32 calledAt);

    /// @notice Auction proceeds for `seriesId` were fully paid out to
    ///         `contributors` tribute owners, totalling `amount` native COEN.
    event ProceedsDistributed(uint32 indexed seriesId, uint256 amount, uint32 contributors);
}
