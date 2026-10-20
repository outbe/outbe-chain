// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title CodeRegistry V1
/// @notice Permissionless, append-only registry of public Heartwood repositories.
/// @dev maxRepositories() == type(uint32).max denotes an unlimited configured capacity.
interface ICodeRegistry {
    /// @dev payload is the exact canonical V1 Vote subject; execution and Merge are separate.
    struct PatchApprovalInfo {
        bool approved;
        string payload;
        uint64 approvedHeight;
        uint64 approvedTimestamp;
    }

    event RepositoryRegistered(bytes20 indexed repoId, address indexed registrant, uint32 index, uint64 generation);

    function registerRepository(bytes20 repoId) external;
    function repositoryCount() external view returns (uint32);
    function repositoryAt(uint32 index) external view returns (bytes20);
    function repositoryRegistrant(bytes20 repoId) external view returns (address);
    function registryGeneration() external view returns (uint64);
    function maxRepositories() external view returns (uint32);
    function getPatchApproval(uint256 proposalId) external view returns (PatchApprovalInfo memory);
    /// @notice 0 = no approved decision; 1 = execution pending. This is not Merge status.
    function patchExecutionStage(uint256 proposalId) external view returns (uint8);
}
