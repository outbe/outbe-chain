// SPDX-License-Identifier: GPL-2.0-or-later
pragma solidity ^0.8.0;

/// @title IPayNote
/// @notice Shielded pool at 0x0000000000000000000000000000000000001019.
/// A deposit pulls `amount` of `asset` from the caller into this pool. Then it
/// appends a note commitment to an incremental Merkle tree. The pool keeps the
/// value until a spend. A settlement spend routes the spent amount through
/// VaultRouter into the reserve vault of the asset. A withdrawal pays the spent
/// amount to a recipient. The value of a change note stays in the pool. The runtime always derives the commitment from the
/// transfer it actually performed. Opaque caller-supplied commitments are
/// prohibited, so Merkle membership attests both the asset and the amount.
///
/// Spending consumes a frozen `outbe.paynote`, version 1.4.0, UltraHonkKeccak
/// proof. Only the settlement factories (NodFactory, GemFactory, IntexFactory)
/// may call `consume`. An EOA pays with a note through the factory's
/// `settle…WithPayNote` method. Any address may relay a `withdraw`.
///
/// A transfer locks a note to a recipient's public key, which is the note serial
/// of a key the recipient holds. The recipient claims or declines it. After 30
/// days the sender may reclaim it.
interface IPayNote {
    /// @notice The pool holds no vault for this asset.
    error AssetVaultMissing(address asset);
    /// @notice A field-typed argument is not a canonical BN254 word.
    error NonCanonicalField(string field);
    /// @notice An argument that must be non-zero was zero.
    error MustBeNonZero(string field);
    /// @notice This commitment is already in the tree.
    error CommitmentExists(bytes32 commitment);
    /// @notice The commitment tree is at its 2^32-leaf capacity.
    error TreeFull();

    /// @notice Deposit `amount` of `asset` into the pool as a locked transfer
    /// note under `lock`.
    /// @dev `lock` is chosen off-chain as
    /// `P(TRANSFER_LOCK, [recipientPk, senderSerial, rho])`, the same lock a
    /// `transfer` uses. It reveals nothing about either party. The recipient
    /// resolves the note with `resolveTransfer` at any time, and the sender
    /// from `deadline` on. To deposit for themselves, the caller uses their own
    /// serial as both `recipientPk` and `senderSerial`.
    /// `asset == address(0)` deposits native COEN: `msg.value` must equal `amount`.
    /// An ERC20 deposit must send no value. A native note can be merged and
    /// withdrawn. It cannot settle a factory payment.
    /// @param asset ERC20 to deposit, which must have a registered reserve vault,
    /// or `address(0)` for native COEN.
    /// @param amount Units to deposit. For an ERC20, the caller must approve
    /// this pool, and the pool must receive exactly `amount`.
    /// @param lock Caller-supplied transfer lock. Must be a non-zero canonical
    /// BN254 field word.
    /// @param deadline Unix time from which the sender may reclaim the note.
    /// Must be at least 30 days after the block timestamp.
    function deposit(address asset, uint256 amount, bytes32 lock, uint64 deadline) external payable;

    /// @notice Atomically consume 2..maxMergeInputs same-asset notes and append
    /// one ordinary note for their exact sum. Any address may relay the proof.
    /// @dev Accepts the active outbe.paynote.merge combined proof. Amounts and
    /// keys remain private. No tokens move and no settlement is performed.
    function mergePayNotes(bytes calldata proof) external;

    /// @notice Spend one note for a settlement. Only NodFactory, GemFactory and
    /// IntexFactory may call it.
    /// @dev Verifies the proof, nullifies the note, appends the change note, if
    /// there is one, and routes `spendAmount` of `asset` from the pool into the
    /// reserve vault of the asset. The caller must check `asset`, `spendAmount`
    /// and `context` against the settlement it performs.
    function consume(bytes calldata proof)
        external
        returns (address asset, uint256 spendAmount, bytes32 context, bytes32 nullifier);

    /// @notice Pay `spendAmount` of one note out of the pool to `recipient`, as
    /// the note's ERC20 or, for a native note, as COEN. Any address may relay
    /// the proof.
    /// @dev Verifies the proof, nullifies the note and appends the change note,
    /// if there is one. The proof must carry the withdrawal context of
    /// `recipient`: the canonical BN254 element of
    /// `keccak256(0x04 || bytes32(recipient) || bytes32(0) || bytes32(0))`. A
    /// proof bound to another recipient or to a settlement reverts.
    /// @param proof Combined `outbe.paynote` public inputs and proof.
    /// @param recipient Receiver of the funds. Must not be zero or this pool.
    function withdraw(bytes calldata proof, address recipient) external;

    /// @notice Lock part of one note to a recipient. Any address may relay the
    /// proof.
    /// @dev Accepts the active `outbe.paynote.transfer` combined proof. Nullifies
    /// the note and appends a locked transfer note and, for a partial transfer,
    /// the change note. Amounts and both parties remain private. No tokens move.
    /// The recipient can resolve the locked note at once. The sender can
    /// reclaim it from the proof's public `deadline` on, which must be at least
    /// 30 days after the block timestamp.
    function transfer(bytes calldata proof) external;

    /// @notice Turn a locked transfer note into an ordinary note. Any address
    /// may relay the proof.
    /// @dev Accepts the active `outbe.paynote.claim` combined proof. The
    /// recipient claims with a serial of their own, or declines with the
    /// sender's refund serial. The sender reclaims once the deadline has
    /// passed. The proof's public `not_before` must not be after the block
    /// timestamp; a sender's proof holds only if the deadline is not after it.
    function resolveTransfer(bytes calldata proof) external;

    /// @notice Maximum input count supported by the active merge circuit.
    function maxMergeInputs() external view returns (uint32 count);

    /// @notice Latest commitment-tree root.
    function currentRoot() external view returns (bytes32 root);

    /// @notice Number of leaves appended so far. `0` means a pristine tree.
    function leafCount() external view returns (uint64 count);

    /// @notice Whether `root` is inside the 32-root acceptance window. Proofs
    /// are built against a root that may go stale as deposits land, so the pool
    /// accepts any root in the window, not just the latest.
    function isKnownRoot(bytes32 root) external view returns (bool known);

    /// @notice Whether `nullifier` has already been spent.
    function isSpent(bytes32 nullifier) external view returns (bool spent);

    /// @notice Whether `commitment` is already a leaf of the tree.
    function hasCommitment(bytes32 commitment) external view returns (bool present);

    /// @notice A commitment was appended to the pool's tree.
    /// @param commitment The appended commitment (indexed).
    /// @param leafIndex Zero-based leaf position of the append.
    /// @param rootAfter Tree root after the append.
    /// @param asset The note's bound ERC20 (indexed).
    /// @param noteAmount Deposited public amount. `0` is the sentinel for a
    /// change or merged note, whose value is private.
    event NewNote(
        bytes32 indexed commitment, uint32 leafIndex, bytes32 rootAfter, address indexed asset, uint256 noteAmount
    );

    /// @notice A note was spent through `consume` or `withdraw`.
    /// @param asset The spent note's bound ERC20, or zero for COEN (indexed).
    /// @param context Statement bound by the proof (indexed).
    /// @param nullifier The spent nullifier (indexed).
    /// @param amountMinor Units released by the spend.
    event NoteUsed(address indexed asset, bytes32 indexed context, bytes32 indexed nullifier, uint256 amountMinor);

    /// @notice A `withdraw` paid `amount` of `asset` to `recipient`. Follows the
    /// `NoteUsed` of the same spend.
    /// @param asset The paid ERC20, or zero for native COEN (indexed).
    /// @param recipient Receiver of the funds (indexed).
    /// @param nullifier The spent nullifier (indexed).
    /// @param amount Units paid out.
    event NoteWithdrawn(address indexed asset, address indexed recipient, bytes32 indexed nullifier, uint256 amount);

    /// @notice A note was locked to a recipient. `NewNote` follows for the
    /// locked note and for the change note, if there is one.
    /// @param asset The note's bound ERC20, or zero for COEN (indexed).
    /// @param lockedCommitment The locked transfer note (indexed).
    /// @param nullifier The spent nullifier (indexed).
    /// @param deadline Unix time from which the sender may reclaim the note.
    event NoteTransferred(
        address indexed asset, bytes32 indexed lockedCommitment, bytes32 indexed nullifier, uint64 deadline
    );

    /// @notice A locked transfer note became an ordinary note: claimed,
    /// declined or reclaimed. `NewNote` follows for the output.
    /// @param asset The note's bound ERC20, or zero for COEN (indexed).
    /// @param outputCommitment The ordinary note created (indexed).
    /// @param nullifier The nullifier of the locked note (indexed).
    event TransferResolved(address indexed asset, bytes32 indexed outputCommitment, bytes32 indexed nullifier);

    /// @notice These canonical nullifiers were consumed to create one note.
    /// @dev Input commitments and all amounts remain private. NewNote supplies
    /// the output's tree index and root to existing indexers.
    event NotesMerged(address indexed asset, bytes32 indexed outputCommitment, bytes32[] nullifiers);
}
