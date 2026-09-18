// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.30;

interface ITributeFactory {
    // The caller need not be a registered L2 operator.
    // `chainId` selects a registered L2 and `version` selects one compiled-in
    // verification key registered for (`chainId`, tribute); there is no runtime
    // key registration. Devnet may use its development binding stub;
    // verification is real.
    // `zkProof` uses bb-keccak-v1 with the tribute claim's four public inputs
    // in order: owner, nft_hash, binding_hash, merkle_root
    // (claims/tribute/abi.json in outbe-l2-claims).
    // `nft_hash` is the tribute draft claim's entity hash. The enclave folds it
    // from the decrypted draft and the cleartext day and currency, and the
    // offer is rejected unless it equals the proof's public input - which is
    // what binds those cleartext fields to the L2-attested draft.
    // `binding_hash` = poseidon2([1, sender, draft_id_lo128, draft_id_hi128,
    // hostChainId, chainId]) - it binds the proof to the caller, the draft,
    // this host chain AND the L2 named by `chainId`, so a proof made for one L2
    // does not verify as another's even under a byte-identical circuit. The
    // enclave recomputes it too, and the offer is rejected on a mismatch.
    // `zkPublicKey` remains ignored; the root-signing key comes from L2Registry.
    //
    // `signature` is the L2 committee's compressed BLS MinSig signature in G1
    // (48 bytes) over the 32-byte `zkMerkleRoot`. L2Registry stores the
    // corresponding compressed G2 group key (96 bytes). Proof, root, signature,
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
