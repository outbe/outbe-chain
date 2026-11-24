// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// @title IAttestersRegistry
/// @notice Native precompile interface for the AttestersRegistry at
///         0x5200000000000000000000000000000000000001.
interface IAttestersRegistry {
    struct AttesterRecord {
        bool active;
        uint32 permissionMask;
        uint64 registeredAt;
        bool isRotationCandidate;
        bytes32 consensusKey;
        uint256 p2pAddr;
    }

    enum PermissionBit {
        SubmitSpendingUnit,
        SubmitSpendingRecord,
        SubmitAmendmentRecord,
        Heartbeat
    }

    event AttesterRegistered(
        address indexed attester, uint32 permissionMask, uint64 registeredAt, bool isRotationCandidate
    );
    event AttesterRevoked(address indexed attester);
    event AttesterMaskUpdated(address indexed attester, uint32 oldMask, uint32 newMask);
    event AttesterRotationCandidacyUpdated(address indexed attester, bool isRotationCandidate);
    event AttesterConsensusIdentitySet(address indexed attester, bytes32 consensusKey, uint256 p2pAddr);
    event AdminTransferInitiated(address indexed from, address indexed to);
    event AdminTransferCompleted(address indexed newAdmin);
    event ReshareRequested(uint64 indexed generation, uint64 blockNumber, address[] skipDealers);
    event GenerationAttested(uint64 indexed generation, address indexed attester, bytes32 outputHash, uint32 count);
    event GenerationActivated(uint64 indexed generation, bytes32 outputHash, uint32 attestations, uint64 blockNumber);

    error NotAdmin();
    error AlreadyRegistered(address attester);
    error NotRegistered(address attester);
    error NotRotationCandidate(address attester);
    error WrongGeneration(uint64 attested, uint64 requested);
    error GenerationAlreadyActive(uint64 generation);
    error AlreadyAttested(uint64 generation, address attester);
    error ZeroOutputHash();
    error ZeroAddress();
    error InvalidMask();

    function admin() external view returns (address);
    function pendingAdmin() external view returns (address);

    function register(
        address attester,
        uint32 permissionMask,
        bool isRotationCandidate,
        bytes32 consensusKey,
        uint256 p2pAddr
    ) external;
    function revoke(address attester) external;
    function updateMask(address attester, uint32 newMask) external;
    function setRotationCandidate(address attester, bool isRotationCandidate) external;
    function setConsensusIdentity(address attester, bytes32 consensusKey, uint256 p2pAddr) external;

    function initiateAdminTransfer(address newAdmin) external;
    function acceptAdminTransfer() external;

    function forceReshare(address[] calldata skipDealers) external returns (uint64);
    function reshareGeneration() external view returns (uint64);
    function reshareSkipDealers() external view returns (address[] memory);
    function activatedGeneration() external view returns (uint64);
    function activatedOutputHash() external view returns (bytes32);
    function attestGeneration(uint64 generation, bytes32 outputHash) external;
    function attestationQuorum() external view returns (uint32);
    function effectiveAttestCount(uint64 generation, bytes32 outputHash) external view returns (uint32);
    function attestationCount(uint64 generation, bytes32 outputHash) external view returns (uint32);

    function getRecord(address attester) external view returns (AttesterRecord memory);
    function isActive(address attester) external view returns (bool);
    function hasPermission(address attester, PermissionBit bit) external view returns (bool);
    function allActors() external view returns (address[] memory);
    function actorCount() external view returns (uint256);
    function rotationCandidatesSorted() external view returns (address[] memory);
}
