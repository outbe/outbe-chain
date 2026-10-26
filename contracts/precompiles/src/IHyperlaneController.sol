// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// HyperlaneController precompile at 0x000000000000000000000000000000000000EE14.
///
/// Governance-owned controller of the Hyperlane bridge. Holds only the local
/// InterchainAccountRouter address and a `domain -> StorageMessageIdMultisigIsm`
/// table (the Outbe chain included under its own domain). Validator sets live
/// in the ISMs themselves. The crosschain vote target approves owner actions,
/// and `applyApproved` runs them (see the crate README for the JSON payloads).
interface IHyperlaneController {
    /// Caller-facing domain revert.
    error AlreadyInitialized();
    error NotInitialized();
    error LocalDomainMissing(uint32 domain);
    error NotBootstrapAdmin(address caller, address admin);
    error NotIsmOwner(address caller, address owner);
    error IsmNotPendingToController(address pending);
    error NotOwnedByController(address target, address owner);
    error InvalidAddress();
    error InvalidDomain();
    error IsmLengthMismatch();
    error HookLengthMismatch();
    error UnknownHook(uint32 domain);
    error NotActiveValidator(address caller);
    error SignerMismatch(address validator, address expected, address recovered);
    error SignerTaken(address signer, address validator);
    error StaleIndex(uint32 index, uint32 submitted);
    error InvalidSignatureLength(uint64 length);
    error UnknownDomain(uint32 domain);
    error LocalDomainNotAllowed(uint32 domain);
    error InvalidValidatorCount(uint64 count);
    error InvalidThreshold(uint8 threshold, uint64 validators);
    error InvalidValidator(address validator);
    error EmptyCalls();
    error ZeroFund();
    error InsufficientBalance(uint256 required, uint256 available);
    error UndecodableReturn(string value);
    error InvalidProposalPayload(string value);
    error UnknownApprovedAction(uint256 proposalId);
    error ApprovedPayloadMismatch(uint256 proposalId);

    event Initialized(address indexed icaRouter);
    event DomainAdded(uint32 indexed domain, address indexed ism);
    event DomainRemoved(uint32 indexed domain);
    event ValidatorsAndThresholdApplied(uint8 threshold, uint256 validatorCount);
    event RemoteCallDispatched(uint32 indexed domain, bytes32 indexed messageId, uint256 fee);
    event LocalCallExecuted(address indexed target, uint256 value);
    event Funded(address indexed from, uint256 amount);
    event HyperlaneSignerSet(address indexed validator, address indexed signer);
    event CheckpointSubmitted(address indexed validator, uint32 indexed domain, uint32 index);
    event LivenessMiss(address indexed validator, uint32 misses);
    event LivenessJailed(address indexed validator);
    event ApprovedActionApplied(uint256 indexed proposalId);
    event ApprovedActionExpired(uint256 indexed proposalId);

    /// One-shot bootstrap. `domains[i] -> (isms[i], hooks[i])` must include the
    /// Outbe domain (= chain id). `hooks` are the MerkleTreeHook addresses whose
    /// checkpoints the validators sign. `validatorAnnounce` is the Outbe
    /// ValidatorAnnounce where validators publish their checkpoint bucket.
    /// Caller must be the current owner of the Outbe ISM (the deployer that
    /// staged `transferOwnership` to this precompile). The precompile accepts
    /// that pending ownership and verifies it owns `icaRouter`.
    function initialize(
        address icaRouter,
        address validatorAnnounce,
        uint32[] calldata domains,
        address[] calldata isms,
        address[] calldata hooks
    ) external;

    /// Registers the key this validator signs Hyperlane checkpoints with, when
    /// it differs from the validator address. Caller: the validator or its
    /// oracle delegate.
    function setHyperlaneSigner(address signer) external;

    /// Liveness proof: an active validator (or its oracle delegate, the feeder
    /// key) submits its latest signed checkpoint for `domain`. The signature is
    /// recovered over the Hyperlane digest and must match the validator's
    /// Hyperlane signer. Only a higher `index` than the recorded one is accepted.
    function submitCheckpoint(uint32 domain, bytes32 root, uint32 index, bytes32 messageId, bytes calldata signature)
        external;

    /// Tops up the balance that pays Interchain Account dispatch fees (IGP quote).
    function fund() external payable;

    /// Mirrors the active Outbe validator set into every ISM: validators =
    /// active validators, threshold = ceil(2/3 * n). No-op when the local ISM
    /// already matches. Permissionless: it only applies what consensus decided.
    /// Returns whether a rotation was dispatched.
    function sync() external returns (bool changed);

    /// Runs a crosschain action that a governance vote approved. Permissionless: the vote decided,
    /// and `payload` must be the approved JSON (its keccak256 is stored). An action older than the
    /// governance voting window is dropped with `ApprovedActionExpired`, so a stale approval cannot
    /// undo a newer one. A remote action pays the Hyperlane fee from the controller balance.
    function applyApproved(uint256 proposalId, bytes calldata payload) external;

    /// InterchainAccountRouter on Outbe.
    function icaRouter() external view returns (address);

    /// Interchain Account of the controller on `domain`. It executes every remote call of the
    /// controller, so it is the governance owner of contracts on that chain.
    function remoteAccount(uint32 domain) external view returns (address);

    /// ValidatorAnnounce on Outbe.
    function validatorAnnounce() external view returns (address);

    /// ISM for `domain`, or zero when not configured.
    function ismByDomain(uint32 domain) external view returns (address);

    /// MerkleTreeHook for `domain`, or zero when not configured.
    function hookByDomain(uint32 domain) external view returns (address);

    function domains() external view returns (uint32[] memory);

    /// Registered Hyperlane signer, or zero when the validator address is used.
    function hyperlaneSigner(address validator) external view returns (address);

    /// Latest submitted checkpoint index of `validator` for `domain`.
    function submittedIndex(address validator, uint32 domain) external view returns (uint32);

    /// Consecutive liveness-window misses of `validator`.
    function missCount(address validator) external view returns (uint32);
}
