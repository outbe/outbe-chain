// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// @title ISpendingUnitV2
/// @notice Native precompile interface for SpendingUnitV2 at
///         0x5200000000000000000000000000000000000008.
interface ISpendingUnitV2 {
    struct Seal {
        uint32 poolEpoch;
        uint64 count;
        bytes32 fFold;
        bytes handleSig;
    }

    struct Batch {
        bytes32 ctx;
        uint64 sequence;
        bytes commitmentRoot;
        uint64 worldwideDay;
        uint64 count;
        bytes32 fFold;
        bytes settleSig;
    }

    struct Mint {
        uint256 suId;
        bytes32 derivedOwner;
        address referrerAddress;
        bytes32 enclavePk;
        bytes32 providerId;
        uint16 currency;
        uint64 amountBase;
        uint64 amountAtto;
    }

    struct SpendingUnitV2Entity {
        uint256 suId;
        bytes32 derivedOwner;
        address attesterAddress;
        address referrerAddress;
        uint16 currency;
        uint64 worldwideDay;
        uint64 amountBase;
        uint64 amountAtto;
        bytes32 ctx;
        bytes32 sealId;
    }

    // SpendingUnitV2 events
    event SUCreated(
        uint256 indexed suId,
        bytes32 indexed ctx,
        bytes32 indexed settlementId,
        address attester,
        bytes32 derivedOwner,
        uint64 worldwideDay,
        uint16 currency,
        uint64 amountBase,
        uint64 amountAtto
    );
    event SealSettled(bytes32 indexed ctx, bytes32 indexed sealId, uint32 poolEpoch);

    // Inherited events (AttesterGated, Pausable, Ownable)
    event RegistryUpdated(address indexed registry);
    event Paused(address account);
    event Unpaused(address account);
    event OwnershipTransferred(address indexed previousOwner, address indexed newOwner);

    // CommitmentToken errors
    error AlreadyExists();
    error InvalidTokenId();

    // AttesterGated errors
    error AttesterNotActive();
    error AttesterNotPermitted();

    // Pausable errors
    error EnforcedPause();
    error ExpectedPause();

    // Ownable errors
    error OwnableUnauthorizedAccount(address account);
    error OwnableInvalidOwner(address owner);

    // PsoProtocol errors
    error NonCanonicalFieldElement(bytes32 value);

    // SpendingUnitV2 errors
    error InvalidAmount();
    error InvalidCurrency();
    error InvalidDerivedOwner();
    error AlreadySettled(bytes32 ctx, bytes32 settledId);
    error BatchTotalsMismatch();
    error EnclaveNotActiveFor(bytes32 enclavePk, address attester);
    error PoolEpochRetired(uint32 poolEpoch);
    error PoolEpochAfterDay(uint32 poolEpoch, uint64 worldwideDay);
    error HandleSignatureInvalid(uint32 poolEpoch);
    error SettlementSignatureInvalid(bytes32 enclavePk);
    error MalformedInput();
    error MulticallMethodNotAllowed(uint256 index);

    // Core SpendingUnitV2 API
    function submitSU(Mint calldata mint, Batch calldata batch, Seal[] calldata seals) external;
    function multicall(bytes[] calldata data) external returns (bytes[] memory results);
    function getData(uint256 suId) external view returns (SpendingUnitV2Entity memory);
    function settlements(bytes32 ctx) external view returns (bytes32);
    function enclaveRegistry() external view returns (address);
    function fsEpochRegistry() external view returns (address);
    function enclaveRegistryContract() external view returns (address);
    function fsEpochRegistryContract() external view returns (address);

    // Constants
    function VERSION() external pure returns (string memory);
    function MAX_SEALS() external pure returns (uint256);
    function POOL_RETENTION_DAYS() external pure returns (uint256);

    // ICommitmentToken + IERC165
    function exists(uint256 id) external view returns (bool);
    function total() external view returns (uint256);
    function supportsInterface(bytes4 interfaceId) external view returns (bool);

    // Pausable
    function paused() external view returns (bool);
    function pause() external;
    function unpause() external;

    // Ownable
    function owner() external view returns (address);
    function transferOwnership(address newOwner) external;
    function renounceOwnership() external;

    // AttesterGated
    function attesterRegistry() external view returns (address);
    function setAttestersRegistry(address _attesterRegistry) external;
    function getAttestersRegistry() external view returns (address registry);
}
