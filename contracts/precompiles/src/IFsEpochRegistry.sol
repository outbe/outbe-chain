// SPDX-License-Identifier: MIT
pragma solidity ^0.8.30;

/// @title IFsEpochRegistry
/// @notice Native precompile interface for the FsEpochRegistry at
///         0x5200000000000000000000000000000000000003.
interface IFsEpochRegistry {
    struct Epoch {
        bytes pkAtt;
        bytes dst;
        uint32 startWwd;
        bytes32 membersHash;
        bytes32 prevHash;
    }

    struct SignedEpoch {
        uint32 epoch;
        Epoch entry;
        bytes sigPrev;
    }

    event EpochAdvanced(uint32 indexed epoch, uint32 startWwd, bytes32 digest);
    event EpochsTruncated(uint32 indexed from);
    event AdminTransferInitiated(address indexed from, address indexed to);
    event AdminTransferCompleted(address indexed newAdmin);

    error NotAdmin();
    error ZeroAddress();
    error AlreadyAnchored();
    error NotAnchored();
    error InvalidKeyLength();
    error InvalidKey();
    error InvalidDst();
    error EpochMismatch(uint32 epoch);
    error BadPrevHash(uint32 epoch);
    error StartWwdNotIncreasing(uint32 epoch);
    error BadEpochSignature(uint32 epoch);
    error EpochGap(uint32 epoch, uint32 head);
    error EpochUnknown(uint32 epoch);

    function admin() external view returns (address);
    function pendingAdmin() external view returns (address);
    function anchor(Epoch calldata epoch0) external;
    function truncate(uint32 from) external;
    function initiateAdminTransfer(address newAdmin) external;
    function acceptAdminTransfer() external;

    function advance(SignedEpoch[] calldata chain) external;

    function head() external view returns (uint32);
    function anchored() external view returns (bool);
    function adminNext() external view returns (bool);
    function keyFor(uint32 e)
        external
        view
        returns (bytes memory pkAtt, bytes memory dst, uint32 startWwd, uint32 endWwdExclusive);
    function digestAt(uint32 e) external view returns (bytes32);
    function digestOf(uint32 e, Epoch memory entry) external pure returns (bytes32);
}
