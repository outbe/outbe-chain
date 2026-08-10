// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGemFactory {
    /// @notice Park the caller's Intex series `sourceIntexId` (burning `amount`
    ///         units via IntexNFT1155) and mint a GemPosition NFT to the caller.
    ///         Returns the new `positionId`.
    function mintGemPosition(uint64 sourceIntexId, uint256 amount) external returns (uint256 positionId);
    /// @notice Issue one Merchant gem to `owner`, draining the position's
    ///         capacity. Only the position's merchant (the caller) may call.
    function mintMerchantGem(uint256 positionId, address owner, uint256 gemLoad) external returns (uint256 gemId);

    /// @notice Settle a gem, paying its cost into the Reserve in `asset` (the
    ///         settlement stablecoin supplied by the caller).
    function settleGem(uint256 gemId, address asset) external;
    /// @notice Burn a settled gem and mint confidential Promis to the caller,
    ///         gated by off-chain proof of work. Authorized by the caller's Promis
    ///         modify key: `mac = HMAC(modifyKey, op-preimage)` where `opNonce`
    ///         MUST equal the caller's current on-chain promis op-nonce (fetch via
    ///         `outbe_deriveKeys` + `IPromis.opNonceOf`) and the bound amount is the
    ///         gem's load. Returns the minted Promis amount.
    function mineGemPromis(uint256 gemId, uint256 nonce, bytes32 mac, uint64 opNonce) external returns (uint256);
    function getStatistics() external view returns (uint256 totalGemsIssued, uint256 totalIntexParked);

    // --- GemPosition NFT (ERC-721-style, non-transferable; owner = merchant) ---
    /// @notice Number of GemPositions owned by `owner`.
    function balanceOf(address owner) external view returns (uint256);
    /// @notice Merchant that owns the position `positionId`.
    function ownerOf(uint256 positionId) external view returns (address);
    /// @notice Metadata URI for the position `positionId`.
    function tokenURI(uint256 positionId) external view returns (string memory);
    /// @notice `positionId` at `index` within `owner`'s positions.
    function tokenOfOwnerByIndex(address owner, uint256 index) external view returns (uint256);

    // --- Events (emitted by the GemFactory precompile) ---
    /// @notice A new gem was minted (agent reward, merchant, or genesis flow).
    event GemIssued(
        uint256 indexed gemId,
        uint8 gemType,
        address owner,
        uint256 gemLoad,
        uint256 entryPrice,
        uint256 costAmount,
        uint256 floorPrice,
        uint64 issuedAt
    );
    /// @notice A gem's Cost Amount was settled into the Reserve.
    event GemSettled(uint256 indexed gemId, address owner, uint256 amountPaid, uint16 issuanceCurrency);
    /// @notice A settled gem was burned to mine confidential Promis.
    event GemBurned(uint256 indexed gemId, address owner, uint256 gemLoad);
}
