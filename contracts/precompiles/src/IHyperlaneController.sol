// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

/// @dev The HyperlaneController precompile. Matches `HYPERLANE_CONTROLLER_ADDRESS` in
///      crates/blockchain/primitives/src/addresses.rs.
address constant HYPERLANE_CONTROLLER = 0x000000000000000000000000000000000000EE14;

/// HyperlaneController precompile at `HYPERLANE_CONTROLLER`.
///
/// The Hyperlane infrastructure of the owner: the local InterchainAccountRouter
/// address, a `domain -> StorageMessageIdMultisigIsm` table (the Outbe chain
/// included under its own domain), and the liveness of the validator agents.
/// Validator sets live in the ISMs themselves. It owns no contract: the owner
/// is `CROSSCHAIN_BRIDGE`. This precompile makes every ISM and router call through
/// `CROSSCHAIN_BRIDGE.callLocal` and `CROSSCHAIN_BRIDGE.callRemote`. Its vote target adds and
/// removes the Hyperlane chains (`addHyperlaneChain`, `removeHyperlaneChain`).
interface IHyperlaneController {
    /// Caller-facing domain revert.
    error AlreadyInitialized();
    error NotInitialized();
    error LocalDomainMissing(uint32 domain);
    error NotBootstrapAdmin(address caller, address admin);
    error NotIsmOwner(address caller, address owner);
    error IsmNotPendingToOwner(address pending);
    error NotOwnedByOwner(address target, address owner);
    error InvalidAddress();
    error InvalidDomain();
    error IsmLengthMismatch();
    error HookLengthMismatch();
    error ValidatorAnnounceLengthMismatch();
    error UnknownHook(uint32 domain);
    error NotActiveValidator(address caller);
    error NotSystemTransaction(address caller);
    error SignerMismatch(address validator, address expected, address recovered);
    error SignerTaken(address signer, address validator);
    error StaleIndex(uint32 index, uint32 submitted);
    error InvalidSignatureLength(uint64 length);
    error ReportTooLong(uint64 length);
    error UnknownDomain(uint32 domain);
    error NoGasPrice(uint32 domain);
    error GasPriceOverflow(uint32 domain);
    error LocalDomainNotAllowed(uint32 domain);
    error InvalidValidatorCount(uint64 count);
    error InvalidThreshold(uint8 threshold, uint64 validators);
    error InvalidValidator(address validator);
    error UndecodableReturn(string value);
    error RemoteRouterMismatch(uint32 domain, bytes32 enrolled);
    error UnknownApprovedAction(uint256 proposalId);
    error ApprovedPayloadMismatch(uint256 proposalId);
    error ApprovedActionLocked(uint256 proposalId, uint64 unlockHeight);

    event Initialized(address indexed icaRouter);
    event DomainAdded(uint32 indexed domain, address indexed ism);
    event DomainRemoved(uint32 indexed domain);
    event ValidatorsAndThresholdApplied(uint8 threshold, uint256 validatorCount);
    event HyperlaneSignerSet(address indexed validator, address indexed signer);
    event CheckpointSubmitted(address indexed validator, uint32 indexed domain, uint32 index);
    event TargetCheckpointSubmitted(address indexed validator, uint32 indexed domain, uint32 index, bytes32 root);
    event TargetIndexSet(uint32 indexed domain, uint32 index);
    event DeliveryIndexSet(uint32 indexed domain, uint32 index);
    /// `reasons` is the sum of: 1 = no vote at the target index or a root
    /// that is not the quorum root, 2 = a quorum named the validator in
    /// `notServing`, 4 = a quorum named it in `notDelivered`, 8 = its report
    /// did not name a validator that a quorum named.
    event LivenessMiss(address indexed validator, uint32 misses, uint8 reasons);
    event LivenessJailed(address indexed validator);
    event ApprovedActionApplied(uint256 indexed proposalId);
    event ApprovedActionExpired(uint256 indexed proposalId);

    /// One-shot bootstrap. `domains[i] -> (isms[i], hooks[i], validatorAnnounces[i])`
    /// must include the Outbe domain (= chain id). `hooks` are the MerkleTreeHook
    /// addresses whose checkpoints the validators sign. `validatorAnnounces` are
    /// the ValidatorAnnounce contracts where validators publish the location of
    /// their signed checkpoints for that domain.
    /// Caller must be the bootstrap admin and the current owner of the Outbe
    /// ISM (the deployer that staged `transferOwnership` to `CROSSCHAIN_BRIDGE`).
    /// `CROSSCHAIN_BRIDGE` accepts that pending ownership, and must already own
    /// `icaRouter`.
    function initialize(
        address icaRouter,
        uint32[] calldata domains,
        address[] calldata isms,
        address[] calldata hooks,
        address[] calldata validatorAnnounces
    ) external;

    /// Registers the key this validator signs Hyperlane checkpoints with, when
    /// it differs from the validator address. Caller: the validator or its
    /// Hyperlane delegate.
    function setHyperlaneSigner(address signer) external;

    /// Runs a Hyperlane chain action that a governance vote approved. Permissionless: the vote
    /// decided, and `payload` must be the approved JSON (its keccak256 is stored). Before the
    /// approval height plus the `executionDelayBlocks` of the payload, it reverts with
    /// `ApprovedActionLocked`. An action that waited longer than the governance voting window
    /// after that delay is dropped with `ApprovedActionExpired`, so a stale approval cannot undo
    /// a newer one. A remote action pays the Hyperlane fee from the
    /// `CROSSCHAIN_BRIDGE` balance.
    function applyApproved(uint256 proposalId, bytes calldata payload) external;

    /// What a validator saw for one domain in the current liveness round.
    /// The controller does not verify these values. It uses a value only
    /// when a quorum of the active validators gives it.
    struct EpochReport {
        /// The latest checkpoint index that the validator signed. Zero when
        /// the validator gives none.
        uint32 latestIndex;
        /// Validators that do not serve their signed checkpoint at
        /// `targetIndex(domain)`.
        address[] notServing;
        /// Validators that did not deliver their message of this domain in
        /// their turn.
        address[] notDelivered;
    }

    /// Liveness proof: an active validator or its Hyperlane delegate submits
    /// a signed checkpoint for `domain`. The signature is recovered over the
    /// Hyperlane digest and must match the validator's Hyperlane signer.
    /// The call does one or both of these:
    /// - `index` equals `targetIndex(domain)`: `root` becomes the vote of the
    ///   validator for this liveness round. A new vote replaces the old one.
    /// - `index` or `report.latestIndex` is higher than
    ///   `submittedIndex(validator, domain)`: the higher value becomes the
    ///   latest index of the validator. The next target comes from these
    ///   indexes.
    /// When neither applies, it reverts with `StaleIndex`.
    /// The first accepted call of the validator for `domain` in a round also
    /// gives its report: each name in the two lists of `report` is one
    /// complaint about that validator. The lists of a later call in the same
    /// round have no effect. The two lists can hold 32 addresses together:
    /// more reverts with `ReportTooLong`.
    function submitCheckpoint(
        uint32 domain,
        bytes32 root,
        uint32 index,
        bytes32 messageId,
        bytes calldata signature,
        EpochReport calldata report
    ) external;

    /// Mirrors the active Outbe validator set into every ISM: validators =
    /// active validators, threshold = ceil(2/3 * n). No-op when the local ISM
    /// already matches. Only the epoch boundary system transaction can call
    /// it, so at most one rotation per epoch is in flight. Other callers get
    /// `NotSystemTransaction`. `CROSSCHAIN_BRIDGE` runs the ISM calls.
    /// Returns whether a rotation was dispatched.
    function sync() external returns (bool changed);

    /// InterchainAccountRouter on Outbe.
    function icaRouter() external view returns (address);

    /// ISM for `domain`, or zero when not configured.
    function ismByDomain(uint32 domain) external view returns (address);

    /// MerkleTreeHook for `domain`, or zero when not configured.
    function hookByDomain(uint32 domain) external view returns (address);

    /// ValidatorAnnounce on the chain of `domain`, or zero when not configured.
    function validatorAnnounceByDomain(uint32 domain) external view returns (address);

    function domains() external view returns (uint32[] memory);

    /// Registered Hyperlane signer, or zero when the validator address is used.
    function hyperlaneSigner(address validator) external view returns (address);

    /// Latest submitted checkpoint index of `validator` for `domain`.
    function submittedIndex(address validator, uint32 domain) external view returns (uint32);

    /// Checkpoint index that every validator must sign for `domain` in the
    /// current liveness round. Zero when there is no target.
    function targetIndex(uint32 domain) external view returns (uint32);

    /// The message index of `domain` at which the delivery check of the
    /// current liveness round starts. The controller selects it at the
    /// liveness verdict from the indexes that the target moved over. Zero
    /// when the target did not move: there is no delivery check.
    function deliveryIndex(uint32 domain) external view returns (uint32);

    /// Last vote of `validator` for `domain`: the root it signed at `index`.
    /// The vote counts only while `index` equals `targetIndex(domain)`.
    function targetVote(address validator, uint32 domain) external view returns (uint32 index, bytes32 root);

    /// The current liveness round. It starts at 1. The liveness verdict runs
    /// in the first block of each epoch and adds 1.
    function livenessRound() external view returns (uint64);

    /// The liveness round of the last report of `validator` for `domain`.
    /// Zero when there is none.
    function reportedRound(address validator, uint32 domain) external view returns (uint64);

    /// Consecutive liveness misses of `validator`.
    function missCount(address validator) external view returns (uint32);

    /// The `IGasOracle` view of the Outbe IGP. The Oracle gas price is in
    /// COEN-wei, so `tokenExchangeRate` is 1e10 (1:1). `gasPrice` is that
    /// price plus the markup of `gasMarkupBps`. Reverts with `NoGasPrice` when
    /// the Oracle has no gas price for the domain.
    function getExchangeRateAndGasPrice(uint32 destinationDomain)
        external
        view
        returns (uint128 tokenExchangeRate, uint128 gasPrice);

    /// Markup on the Oracle gas price in basis points (2000 = +20%).
    function gasMarkupBps() external view returns (uint32);
}
