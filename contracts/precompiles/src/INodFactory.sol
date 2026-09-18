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
        uint256 settlementCostMinor
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

    /// @notice Emitted when a Nod is paid. ERC20 payments use a zero nullifier;
    /// PayNote payments identify the spent note by its nullifier.
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
        uint256 lysisAllocationMinor,
        uint64 issuedAt,
        bytes32 stateEventDigest
    );

    /// @notice Pay a qualified Nod in ERC20 base units of `asset`.
    /// The asset must have a reserve vault and report the Nod's reference or
    /// issuance ISO 4217 code. Issuance-currency payment converts the
    /// reference-currency entry cost at the current COEN cross rate.
    function settleNod(uint256 nodId, address asset) external;

    /// @notice Pay a qualified Nod at or before its settlement deadline.
    /// The PayNote proof must name the caller as its owner and carry an asset
    /// the Nod accepts on either currency rail.
    function settleNodWithPayNote(uint256 nodId, bytes calldata payNoteProof) external;

    /// @notice What settling `nodId` with `asset` costs, and which of the Nod's
    /// two currencies that asset settles on. Reverts for an asset the Nod
    /// does not accept.
    /// @return settlementCurrency ISO 4217 code the payment is denominated in.
    /// @return payableUnits Amount to pay, in `asset`'s own minor units.
    function quoteSettlement(uint256 nodId, address asset)
        external
        view
        returns (uint16 settlementCurrency, uint256 payableUnits);

    /// @notice Exercise a paid Nod and mint its Gratis load to the Nod owner.
    /// @param nonce PoW over `sha256(nodId_be32 || owner_20 || miningSequence_be8 || nonce_be8)`
    /// with `miningSequence = 0` and the required leading zero bytes. The owner is the Nod owner.
    /// @param mac Gratis mint authorization under the owner's modify key.
    /// @param opNonce The owner's current Gratis operation nonce, bound by `mac`.
    function mineGratis(uint256 nodId, uint64 nonce, bytes32 mac, uint64 opNonce) external returns (uint256);

    /// @notice Materialize the current certified FIFO head from one canonical
    /// proof-backed OCOMP batch.
    function materializeCertifiedNods(bytes calldata canonicalBatch) external;

    /// @notice Return the canonical current FIFO head, or `exists=false` when empty.
    function materializationHead() external view returns (bool exists, bytes memory canonicalHead);
}
