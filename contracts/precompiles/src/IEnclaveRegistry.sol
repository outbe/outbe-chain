// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// @title IEnclaveRegistry
/// @notice Native precompile interface for the EnclaveRegistry at
///         0x5200000000000000000000000000000000000002.
interface IEnclaveRegistry {
    enum DeploymentStatus {
        None,
        Active,
        Retiring
    }

    struct ProviderPolicy {
        bool allowed;
        bool requireRelease;
        bytes32 baseUrlHash;
        bytes32 environmentHash;
    }

    struct Deployment {
        address attester;
        bytes32 rmDigest;
        bytes32 providerId;
        bytes32 baseUrlHash;
        uint64 registeredAt;
        uint64 retiringUntil;
        DeploymentStatus status;
        uint64 tcb;
        uint8 product;
        bytes32[] measurements;
        bytes32[] chipIds;
    }

    event DeploymentRegistered(
        bytes32 indexed enclavePk,
        address indexed attester,
        bytes32 indexed providerId,
        bytes32 measurementHash,
        bytes32 rmDigest
    );
    event MeasurementAdded(bytes32 indexed enclavePk, bytes32 measurementHash, bytes32 chipIdHash);
    event Reattested(bytes32 indexed enclavePk, bytes32 measurementHash, bytes32 chipIdHash, uint64 tcb);
    event DeploymentRetiring(bytes32 indexed enclavePk, uint64 retiringUntil);
    event DeploymentRevoked(bytes32 indexed enclavePk);

    event MeasurementSet(bytes32 indexed measurementHash, bool allowed);
    event ProviderSet(bytes32 indexed providerId, ProviderPolicy policy);
    event TcbFloorSet(uint8 product, uint64 tcbFloor);
    event ChipIdRevoked(bytes32 indexed chipIdHash);
    event RetiringGraceSet(uint64 blocks);
    event MaxSchemaSet(uint16 maxSchema);
    event MinRegistrationVersionSet(uint8 version);

    event AdminTransferInitiated(address indexed from, address indexed to);
    event AdminTransferCompleted(address indexed newAdmin);

    error NotAdmin();
    error ZeroAddress();
    error AlreadyInitialized();
    error InputTooLarge();

    error InvalidRegistrationSignature();
    error UnsupportedRegistrationVersion(uint8 version);
    error MalformedRegistration();
    error AttesterKeyMismatch();
    error NotTheAttester();
    error EnclaveDidNotBindTheAttester(bytes32 enclavePk, address attester);
    error AttesterNotActive();
    error AttesterNotPermitted();

    error AlreadyRegistered(bytes32 enclavePk);
    error NotRegistered(bytes32 enclavePk);
    error DeploymentNotLive(bytes32 enclavePk);

    error ReportRefused();
    error IdentityBindingMismatch();
    error DebugPolicy();
    error MigrationPolicy();
    error ChipRevoked(bytes32 chipIdHash);
    error NotVmpl0();
    error TcbBelowFloor(uint64 reported, uint64 floor);
    error ProductMismatch(uint8 reported, uint8 recorded);

    error MeasurementNotAllowed(bytes32 measurementHash);
    error MeasurementNotCertified(bytes32 measurementHash);
    error AlreadyKnownMeasurement(bytes32 measurementHash);
    error UnknownMeasurement(bytes32 measurementHash);
    error TooManyEntries();

    error ManifestMismatch();
    error MalformedManifest();
    error UnsupportedManifestSchema(uint16 schema);
    error ProviderNotAllowed(bytes32 providerId);
    error BaseUrlMismatch();
    error EnvironmentNotAllowed();
    error NotReleaseBuild();

    error DevnetRegistrationDisabled();

    function register(
        bytes calldata registration,
        bytes calldata report,
        bytes calldata vcek,
        uint8 product,
        bytes calldata manifest
    ) external;
    function addMeasurement(bytes32 enclavePk, bytes calldata report, bytes calldata vcek, uint8 product) external;
    function reattest(bytes32 enclavePk, bytes calldata report, bytes calldata vcek, uint8 product) external;
    function retire(bytes32 enclavePk) external;
    function revoke(bytes32 enclavePk) external;
    function adminRegister(
        bytes32 enclavePk,
        address attester,
        bytes32 rmDigest,
        bytes32 providerId,
        bytes32 baseUrlHash,
        bytes32 measurementHash
    ) external;

    function admin() external view returns (address);
    function pendingAdmin() external view returns (address);
    function attestersRegistry() external view returns (address);
    function initialize(address _admin, address _attestersRegistry) external;

    function setMeasurement(bytes32 measurementHash, bool allowed) external;
    function setProvider(bytes32 providerId, ProviderPolicy calldata policy) external;
    function setTcbFloor(uint8 product, uint64 floor) external;
    function revokeChip(bytes32 chipIdHash) external;
    function setRetiringGrace(uint64 blocks) external;
    function setMaxSchema(uint16 schema) external;
    function setMinRegistrationVersion(uint8 version) external;

    function initiateAdminTransfer(address newAdmin) external;
    function acceptAdminTransfer() external;

    function allowedMeasurements(bytes32 measurementHash) external view returns (bool);
    function providers(bytes32 providerId)
        external
        view
        returns (bool allowed, bool requireRelease, bytes32 baseUrlHash, bytes32 environmentHash);
    function tcbFloor(uint8 product) external view returns (uint64);
    function revokedChips(bytes32 chipIdHash) external view returns (bool);
    function retiringGrace() external view returns (uint64);
    function maxSchema() external view returns (uint16);
    function minRegistrationVersion() external view returns (uint8);
    function devnetAdminRegister() external view returns (bool);

    function getDeployment(bytes32 enclavePk) external view returns (Deployment memory);
    function isActive(bytes32 enclavePk) external view returns (bool);
    function isActiveFor(bytes32 enclavePk, address attester, bytes32 providerId) external view returns (bool);
    function measurementsOf(bytes32 enclavePk) external view returns (bytes32[] memory);
    function chipIdsOf(bytes32 enclavePk) external view returns (bytes32[] memory);
    function allEnclaves(uint256 offset, uint256 limit) external view returns (bytes32[] memory);
    function enclaveCount() external view returns (uint256);
}
