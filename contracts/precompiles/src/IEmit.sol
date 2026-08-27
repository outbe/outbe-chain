// SPDX-License: UNLICENSED
pragma solidity ^0.8.30;

/// @title IEmit
/// @notice Emit private-note tree precompile at
///         0x000000000000000000000000000000000000EE12
interface IEmit {
    /// Burn native COEN into a private note. The commitment is derived from
    /// the runtime chain ID, `noteSn`, and the caller-supplied value; the note
    /// itself (owner, spend key) is chosen off-chain and proven later at mint.
    /// `msg.value` must be a positive amount fitting `uint128` (native base
    /// units, 1:1 with circuit units). Initializes the tree on the first call.
    function burn(bytes32 noteSn) external payable;

    /// Redeem a private note: prove membership under an accepted root,
    /// nullify the note, credit `mintUnits` to `payoutRecipient`, and — when
    /// the note holds more than `mintUnits` — append the circuit-derived
    /// deterministic change commitment. The caller must be `noteOwner`; the
    /// embedded proof statement must equal the explicit calldata fields, and
    /// `chainId` must equal the runtime chain ID. `proof` is the combined
    /// UltraHonkKeccak wire for the frozen Emit mint circuit
    /// (`outbe.emit.mint`, version 1.3.0), enforced at its exact frozen
    /// length.
    function mint(
        address payoutRecipient,
        uint64 chainId,
        bytes32 root,
        bytes32 nullifier,
        address noteOwner,
        uint128 mintUnits,
        bytes32 changeCommitment,
        bytes calldata proof
    ) external;

    /// @notice A commitment was appended to the chain's Emit tree.
    /// @param commitment The appended commitment (indexed).
    /// @param leafIndex Zero-based leaf position of the append.
    /// @param rootAfter Tree root after the append.
    /// @param noteAmount Burned public amount; `0` is the sentinel for a
    ///        partial mint's change note, whose remaining value is private.
    event NewNote(bytes32 indexed commitment, uint32 leafIndex, bytes32 rootAfter, uint128 noteAmount);

    /// @notice A note was spent via mint.
    /// @param noteOwner Owner proven by the mint proof (indexed).
    /// @param payoutRecipient Recipient credited with `mintAmount` (indexed).
    /// @param nullifier The spent nullifier (indexed).
    /// @param mintAmount Credited native base units.
    event NoteUsed(
        address indexed noteOwner, address indexed payoutRecipient, bytes32 indexed nullifier, uint128 mintAmount
    );
}
