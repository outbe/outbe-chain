// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITributeFactory {
    // The caller need not be a registered L2 operator.
    // `chainId` selects a registered L2 and `version` selects an exact
    // circuit binding. Devnet may use its development binding stub; verification is real.
    // `zkProof` uses bb-keccak-v1 with four public inputs in order:
    // derived_owner, nft_hash, binding_hash, merkle_root. The hashes must retain
    // the TributeDraft and caller/L1-chain binding semantics checked by the enclave.
    // `zkPublicKey` remains ignored; the current root-signing key comes from L2Registry,
    // including any operator-authorized updatePublicKey rotation.
    //
    // `signature` is the L2 committee's compressed BLS MinSig signature in G1
    // (48 bytes) over the 32-byte `zkMerkleRoot`. L2Registry's public API uses
    // the corresponding EIP-2537 G2 group key (256 bytes); its compact internal
    // storage and the signature encoding are unchanged. Proof, root, signature,
    // and circuit selection are mandatory for every offer.
    function offerTribute(
        bytes calldata cipherText,
        bytes calldata nonce,
        uint256 ephemeralPubkey,
        uint32 worldwideDay,
        uint16 tributeCurrency,
        uint16 referenceCurrency,
        bool excludeFromIntexIssuance,
        bytes calldata zkProof,
        uint32 chainId,
        string calldata version,
        bytes calldata zkPublicKey,
        bytes calldata zkMerkleRoot,
        bytes calldata signature
    ) external returns (uint256 tributeId);
}
