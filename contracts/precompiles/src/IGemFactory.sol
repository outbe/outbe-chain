// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface IGemFactory {
    function settleGem(uint256 gemId) external;
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
}
