// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @title IVote
/// @notice Generic on-chain proposal/voting precompile at 0x000000000000000000000000000000000000EE0C
interface IVote {
    /// Caller-facing domain revert.
    error NotValidator();
    error ProposalNotFound();
    error NotPending();
    error VotingClosed();
    error InvalidVotingWindow(uint64 actual, uint64 maximum);
    error VotingDeadlineOverflow();
    error AlreadyVoted();
    error TooManyPending();
    error TooManyPendingByValidator();
    error TooManyPendingPublicBonded();
    error TooManyPendingPublicBondedByProposer();
    error InvalidProposalStatus();
    error InvalidVoteKind();
    error InvalidElectorate();
    error ProposalCounterExhausted();
    error InvalidPayload();
    error UnknownTargetModule();
    error DuplicateTargetModule();
    error StoredPayloadInvalid(string module);

    /// Voting either reaches quorum or does not. Applied approval is Approved;
    /// no quorum is Expired. Error is an execution failure, not a negative vote.
    /// Rejected is reserved and is not produced by the current voting lifecycle.
    enum ProposalStatus {
        Pending,
        Approved,
        Rejected,
        Expired,
        Error
    }

    enum BondState {
        NoBond,
        Escrowed,
        Refunded,
        Burned
    }

    struct VoteTally {
        uint64 yes;
        uint64 no;
    }

    struct ProposalInfo {
        uint256 proposalId;
        address proposer;
        address targetModule;
        string payload;
        uint64 createdHeight;
        uint64 votingDeadlineHeight;
        ProposalStatus status;
        /// @notice Tally recomputed against the proposal's currently eligible electorate.
        /// @dev This is a live view, not a stored finalization tally, and may change
        /// after finalization. The terminal status and finalization event record the
        /// decision made using the live electorate at finalization.
        VoteTally state;
        /// @dev This struct does not carry the full list of voters.
        /// Use `getProposalVoters` to get it.
        uint256 votersCount;
    }

    /// @notice New proposal was created.
    event ProposalCreated(
        uint256 indexed proposalId,
        address indexed proposer,
        address targetModule,
        string payload,
        uint64 votingDeadlineHeight
    );

    /// @notice Validator voted on a proposal.
    event VoteCast(uint256 indexed proposalId, address indexed validator, bool approve);

    /// @notice Reserved conflict-rejection declaration; not emitted by the current lifecycle.
    event ProposalRejected(uint256 indexed proposalId, VoteTally state, uint256 indexed conflictingproposalId);

    /// @notice Voting closed without reaching quorum.
    event ProposalExpired(uint256 indexed proposalId, VoteTally state);

    /// @notice Reserved cancellation declaration; the current lifecycle has no cancellation entry.
    event ProposalCancelled(uint256 indexed proposalId, address indexed proposer);

    /// @notice Voting reached quorum and the target was applied successfully.
    event ProposalApproved(uint256 indexed proposalId, VoteTally state);

    /// @notice Target execution failed and its effects were rolled back; this is not a negative vote.
    event ProposalErrored(uint256 indexed proposalId, VoteTally state);

    /// @notice A target-specific proposal bond was recorded as escrowed.
    event ProposalBondEscrowed(uint256 indexed proposalId, address indexed owner, uint256 amount);

    /// @notice A successful target execution refunded its recorded proposal bond.
    event ProposalBondRefunded(uint256 indexed proposalId, address indexed owner, uint256 amount);

    /// @notice An expired proposal burned its recorded proposal bond.
    event ProposalBondBurned(uint256 indexed proposalId, address indexed owner, uint256 amount);

    /// @notice A proposal bond check failed; reason preserves the accounting context.
    error InvalidProposalBond(string reason);

    /// @notice Creates a generic proposal.
    /// @dev Vote enforces its raw UTF-8 payload byte limit before target decoding
    /// or storage writes. This admission-only bound is separate from decoded field limits.
    /// @param targetModule Target system module precompile address.
    /// @param payload JSON payload decoded only by the target module handler.
    function createProposal(address targetModule, string calldata payload) external payable returns (uint256 proposalId);

    /// @notice Creates a proposal with a voting duration chosen by its author.
    /// @param votingWindowBlocks Duration in 1..=the network governance voting window.
    /// @dev Admission and bond rules are identical to createProposal. The deadline
    /// is createdHeight + votingWindowBlocks, inclusive; tally runs in the next block.
    function createProposalWithVotingWindow(address targetModule, string calldata payload, uint64 votingWindowBlocks)
        external
        payable
        returns (uint256 proposalId);

    /// @notice Casts a vote on a pending proposal.
    /// @dev Only active validators may vote.
    function castVote(uint256 proposalId, bool approve) external;

    // ===============================================
    // VIEW API
    // ===============================================

    /// @notice Returns proposal details.
    function getProposal(uint256 proposalId) external view returns (ProposalInfo memory);

    /// @notice Returns the recorded bond amount and bond state for a proposal.
    function getProposalBond(uint256 proposalId) external view returns (uint256 amount, BondState state);

    /// @notice Returns the total amount of all escrowed proposal bonds.
    function escrowedBondTotal() external view returns (uint256);

    /// @notice Returns a slice of voters for a proposal, with pagination, to prevent bloating the response.
    function getProposalVoters(uint256 proposalId, uint256 index, uint256 count)
        external
        view
        returns (address[] memory);

    /// @notice Returns all tracked proposal ids.
    function listProposals(uint256 index, uint256 count) external view returns (uint256[] memory);

    /// @notice Returns proposal ids filtered by status.
    function listProposalsByStatus(ProposalStatus status, uint256 index, uint256 count)
        external
        view
        returns (uint256[] memory);
}
