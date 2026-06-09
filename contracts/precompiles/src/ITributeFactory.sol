// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITributeFactory {
    // `zkProof`, `zkVerificationKey`, `zkPublicKey`, `zkMerkleRoot`, and
    // `tributeOwnerL1` are ABI stubs reserved for the future ZK-verification path.
    // Dispatch ignores them today; clients must pass well-formed bytes, but no
    // verification runs. Do not remove from the ABI without a migration plan —
    // the fields are part of the external contract surface.
    function offerTribute(
        bytes calldata cipherText,
        bytes calldata nonce,
        uint256 ephemeralPubkey,
        uint16 referenceCurrency,
        bytes calldata zkProof,
        bytes calldata zkVerificationKey,
        bytes calldata zkPublicKey,
        bytes calldata zkMerkleRoot
    ) external returns (uint256 tributeId);
}
