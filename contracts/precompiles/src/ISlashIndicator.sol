// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title ISlashIndicator
/// @notice Slash indicator precompile at 0x000000000000000000000000000000000000EE01
interface ISlashIndicator {
    /// Emitted when a proposer miss triggers a felony (forced exit + slash).
    event ProposerFelony(address indexed validator, uint64 missCount, uint64 felonyCount);

    /// Emitted when a proposer miss reaches the misdemeanor threshold.
    event ProposerMisdemeanor(address indexed validator, uint64 missCount);

    /// Emitted when a voter miss reaches the misdemeanor threshold.
    event VoterMisdemeanor(address indexed validator, uint64 missCount);

    /// Emitted when a voter miss triggers a felony (forced exit + slash).
    event VoterFelony(address indexed validator, uint64 missCount, uint64 felonyCount);

    /// Emitted when evidence-based felony is applied (forced exit + slash + reward).
    event EvidenceFelonyApplied(
        address indexed validator, address indexed submitter, uint256 slashedAmount, uint256 submitterReward
    );

    /// Emitted when byzantine behavior is detected by the consensus layer
    /// (equivocation: ConflictingNotarize/ConflictingFinalize/NullifyFinalize).
    event ByzantineFelony(address indexed validator, uint256 slashedAmount, uint64 felonyCount);

    /// Emitted when `submitInvalidVrfProofEvidence` accepts a VRF-class
    /// verifier failure and applies the felony to the child block's Phase 1
    /// tx signer. `failureCode` is the canonical class re-derived from
    /// `verify_v2_proof` (NOT the submitter's hint).
    event InvalidVrfProofEvidenceApplied(
        address indexed proposer, address indexed submitter, bytes32 indexed childBlockHash, uint16 failureCode
    );

    function submitDoubleProposalEvidence(bytes calldata block1, bytes calldata block2) external;
    function submitConflictingVoteEvidence(bytes calldata vote1, bytes calldata vote2) external;
    /// Submit evidence of an invalid threshold VRF proof in a child
    /// block's Phase 1 system transaction. `evidence` is the wire form of
    /// `InvalidVrfProofEvidence` (see `vrf_evidence.rs`).
    function submitInvalidVrfProofEvidence(bytes calldata evidence) external;
    function getProposerMissCount(address validator) external view returns (uint64);
    function getVoterMissCount(address validator) external view returns (uint64);
    function getFelonyCount(address validator) external view returns (uint64);
}
