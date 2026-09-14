// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITributeFactory {
    // Every caller must be a registered L2 operator. `zkVerificationKey` holds
    // raw key bytes whose keccak256 must match a vk_hash explicitly enabled
    // in L2_CIRCUITS_REGISTRY for that operator's L2.
    // `zkProof` uses bb-keccak-v1 with four public inputs in order:
    // derived_owner, nft_hash, binding_hash, merkle_root. The hashes must retain
    // the TributeDraft and caller/L1-chain binding semantics checked by the enclave.
    // `zkPublicKey` remains ignored; the root-signing key comes from L2Registry.
    //
    // `signature` is the L2 committee's compressed BLS MinSig signature in G1
    // (48 bytes) over the 32-byte `zkMerkleRoot`. L2Registry stores the
    // corresponding compressed G2 group key (96 bytes). Only registered
    // networks with ZK disabled may pass empty ZK fields.
    function offerTribute(
        bytes calldata cipherText,
        bytes calldata nonce,
        uint256 ephemeralPubkey,
        uint32 worldwideDay,
        uint16 tributeCurrency,
        uint16 referenceCurrency,
        bool excludeFromIntexIssuance,
        bytes calldata zkProof,
        bytes calldata zkVerificationKey,
        bytes calldata zkPublicKey,
        bytes calldata zkMerkleRoot,
        bytes calldata signature
    ) external returns (uint256 tributeId);
}
