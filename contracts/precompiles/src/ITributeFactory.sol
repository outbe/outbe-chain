// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITributeFactory {
    // The caller need not be a registered L2 operator.
    // `chainId` selects a registered L2 and `version` selects an exact
    // circuit binding. Devnet may use its development binding stub. Verification is real.
    // `zkProof` uses bb-keccak-v1 with four public inputs in order:
    // derived_owner, nft_hash, binding_hash, merkle_root. The hashes must retain
    // the TributeDraft and caller/L1-chain binding semantics that the enclave checks.
    // `zkPublicKey` remains ignored. The current root-signing key comes from L2Registry,
    // including any operator-authorized updatePublicKey rotation.
    // An unset registry key is resolved live from `l1Address.groupPubKey()`.
    //
    // `signature` is the L2 committee's compressed BLS MinSig signature in G1
    // (48 bytes) over the 32-byte `zkMerkleRoot`. L2Registry's public API uses
    // the corresponding EIP-2537 G2 group key (256 bytes). Its compact internal
    // storage and the signature encoding are unchanged. Every offer must include
    // the proof, the root, the signature and the circuit selection.
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
