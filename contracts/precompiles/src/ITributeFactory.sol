// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITributeFactory {
    // `zkProof`, `zkVerificationKey`, and `zkPublicKey` are ABI stubs reserved
    // for the future ZK-verification path. Dispatch ignores them today; clients
    // must pass well-formed bytes, but no verification runs. Do not remove from
    // the ABI without a migration plan — the fields are part of the external
    // contract surface.
    //
    // `signature` is the L2 network's BLS MinPk signature (96 bytes) over
    // `zkMerkleRoot`. When the caller is registered in the L2Registry as an L1
    // operator address and that network has ZK verification enabled, the
    // signature must verify against the network's registered public key or the
    // call reverts. Unregistered callers (and networks with ZK disabled) may
    // pass empty bytes.
    function offerTribute(
        bytes calldata cipherText,
        bytes calldata nonce,
        uint256 ephemeralPubkey,
        uint16 referenceCurrency,
        bool excludeFromIntexIssuance,
        bytes calldata zkProof,
        bytes calldata zkVerificationKey,
        bytes calldata zkPublicKey,
        bytes calldata zkMerkleRoot,
        bytes calldata signature
    ) external returns (bytes memory tributeId);
}
