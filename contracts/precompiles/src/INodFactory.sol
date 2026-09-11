// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface INodFactory {
    event NodIssued(
        address indexed owner,
        uint256 nodId,
        uint256 worldwideDay,
        uint256 leagueId,
        uint256 floorPriceMinor,
        uint256 gratisLoadMinor,
        uint256 entryPriceMinor,
        uint256 costAmountMinor
    );

    event NodExercised(address indexed owner, uint256 nodId, uint256 gratisLoadMinor);

    event NodBurned(address indexed owner, uint256 nodId, uint256 gratisLoadMinor);

    event NodMaterializationProgress(
        uint64 indexed queueSequence,
        uint32 indexed worldwideDay,
        uint64 generation,
        uint32 firstNodOrdinal,
        uint32 nextNodOrdinal,
        bool completed,
        uint64 blockNumber
    );

    error NodMaterializationRejected(uint8 code);

    /// @notice Emitted when a Nod's cost is discharged by burning a PayNote.
    /// Names the spent nullifier instead of a payer address: the note is what
    /// pays, and it is deliberately not linkable to a payer.
    event NodPaid(address indexed owner, uint256 nodId, address asset, bytes32 nullifier, uint256 amountCovered);

    /// @notice Constant-size owner event for one certified OCOMP generation.
    /// There is deliberately no matching public installation selector.
    event CertifiedNodGenerationInstalled(
        bytes32 indexed activationCallId,
        uint32 indexed worldwideDay,
        uint64 targetGeneration,
        bytes32 namespaceRootBefore,
        uint32 tributeCount,
        uint32 nodCount,
        uint32 bucketCount,
        bytes32 nodRoot,
        bytes32 bucketRoot,
        bytes32 outputManifestRoot,
        uint256 nodAmountTotal,
        uint256 nodGratisConsumed,
        uint64 issuedAt,
        bytes32 stateEventDigest
    );

    /// @notice Pay the caller-owned qualified Nod at or before its settlement deadline.
    /// Preserves the Nod as a paid entitlement. No PoW or mint authorization is needed.
    function settleNod(uint256 nodId, bytes calldata payNoteProof) external;

    /// @notice Exercise a caller-owned paid Nod and mint its Gratis load.
    /// Requires valid PoW and the owner's current Gratis mint authorization.
    /// Paid entitlements have no mining deadline and require no further payment.
    /// @param nonce PoW over `sha256(nodId_be32 || nonce_be8)` with the required leading zero bytes.
    /// @param mac Gratis mint authorization under the owner's modify key.
    /// @param opNonce The owner's current Gratis operation nonce, bound by `mac`.
    function mineGratis(uint256 nodId, uint64 nonce, bytes32 mac, uint64 opNonce) external returns (uint256);

    /// @notice Materialize the current certified FIFO head from one canonical
    /// proof-backed OCOMP batch.
    function materializeCertifiedNods(bytes calldata canonicalBatch) external;

    /// @notice Return the canonical current FIFO head, or `exists=false` when empty.
    function materializationHead() external view returns (bool exists, bytes memory canonicalHead);
}
