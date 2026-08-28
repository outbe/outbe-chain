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

    /// @notice Emitted when a Nod's cost is discharged by burning a Paynote.
    ///         Names the spent nullifier instead of a payer address: the note
    ///         is what pays, and it is deliberately not linkable to a payer.
    event NodPaid(address indexed owner, uint256 nodId, address asset, bytes32 nullifier, uint256 amountCovered);

    /// @notice Constant-size owner event for one certified OCOMP generation.
    ///         There is deliberately no matching public installation selector.
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

    /// @notice Burn the caller-owned Nod and mint its gratis load to the
    ///         caller. Authorized by the caller's Gratis modify key: `mac =
    ///         HMAC(modifyKey, op-preimage)` where `opNonce` MUST equal the
    ///         caller's current on-chain gratis op-nonce. The Nod owner is the
    ///         gratis recipient, so they can always supply this authorization.
    ///
    ///         The Nod's cost is discharged here, by spending a Paynote rather
    ///         than by a prior transparent payment. `paynoteProof` MUST be an
    ///         `outbe.paynote` spend proof naming the caller as its spender,
    ///         carrying the asset the VaultRouter has registered under the
    ///         Nod's `referenceCurrency`, and covering `costAmountMinor`. The
    ///         underlying value already reached the reserve vault when the note
    ///         was deposited, so this call moves no tokens; it burns the note's
    ///         nullifier and logs `NodPaid`.
    ///
    ///         Empty `paynoteProof` bytes are accepted only for a zero-cost
    ///         Nod, and a zero-cost Nod rejects a non-empty proof.
    function mineGratis(uint256 nodId, uint64 nonce, bytes32 mac, uint64 opNonce, bytes calldata paynoteProof)
        external
        returns (uint256);

    /// @notice Materialize the current certified FIFO head from one canonical
    ///         proof-backed OCOMP batch.
    function materializeCertifiedNods(bytes calldata canonicalBatch) external;

    /// @notice Return the canonical current FIFO head, or `exists=false` when empty.
    function materializationHead() external view returns (bool exists, bytes memory canonicalHead);
}
