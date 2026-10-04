import { type Address, type Hex, concat, sha256, toBytes, toHex } from "viem";

/**
 * Proof-of-work for NodFactory.mineGratis and GemFactory.minePromis. Scheme
 * verbatim from crates/core/common/src/pow.rs (compute_mining_pow_hash):
 *   preimage = domain_tag[19] ++ rightId_be32 ++ owner[20] ++ miningSequence_be8 ++ nonce_be8
 *   hash     = SHA256(preimage)
 *   valid    = first POW_DIFFICULTY bytes of hash are zero
 * Nod and Gem exercise once, so miningSequence is 0.
 */
export const POW_DIFFICULTY = 1; // crates/core/common/src/pow.rs
export const NOD_MINING_TAG = "OUTBE_NOD_MINING_V1";
export const GEM_MINING_TAG = "OUTBE_GEM_MINING_V1";
export const SINGLE_EXERCISE_SEQUENCE = 0n;

export function miningPowHash(
  domainTag: string,
  rightId: bigint,
  owner: Address,
  miningSequence: bigint,
  nonce: bigint,
): Hex {
  const tag = new TextEncoder().encode(domainTag);
  if (tag.length !== 19) throw new Error("mining domain tag must be 19 bytes");
  const data = concat([
    tag,
    toBytes(toHex(rightId, { size: 32 })),
    toBytes(owner),
    toBytes(toHex(miningSequence, { size: 8 })),
    toBytes(toHex(nonce, { size: 8 })),
  ]);
  return sha256(data);
}

export function meetsDifficulty(hash: Hex): boolean {
  const bytes = toBytes(hash);
  for (let i = 0; i < POW_DIFFICULTY; i++) {
    if (bytes[i] !== 0) return false;
  }
  return true;
}

/** Grind the first nonce whose hash clears POW_DIFFICULTY. */
export function grindMiningNonce(domainTag: string, rightId: bigint, owner: Address): bigint {
  for (let nonce = 0n; nonce <= 0xffff_ffff_ffff_ffffn; nonce++) {
    if (meetsDifficulty(miningPowHash(domainTag, rightId, owner, SINGLE_EXERCISE_SEQUENCE, nonce))) {
      return nonce;
    }
  }
  throw new Error("no PoW nonce found within u64 range");
}
