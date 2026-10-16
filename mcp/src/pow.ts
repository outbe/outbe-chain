import { type Address, type Hex, concat, hexToBytes, numberToBytes, sha256 } from "viem";

/** Which right is mined: each signs its proof of work with its own domain tag. */
export type MiningDomain = "Gem" | "Nod";

const TAGS: Record<MiningDomain, Uint8Array> = {
  Gem: new TextEncoder().encode("OUTBE_GEM_MINING_V1"),
  Nod: new TextEncoder().encode("OUTBE_NOD_MINING_V1"),
};

/** `SHA256(tag || id || owner || sequence || nonce)` as `crates/core/common/src/pow.rs` computes it; Gem and Nod mine once, at sequence 0. */
export function miningPowHash(domain: MiningDomain, id: bigint, owner: Address, nonce: bigint): Hex {
  return sha256(
    concat([TAGS[domain], numberToBytes(id, { size: 32 }), hexToBytes(owner), new Uint8Array(8), numberToBytes(nonce, { size: 8 })]),
  );
}

/** The first nonce whose hash starts with `difficulty` zero bytes. */
export function grindMiningNonce(domain: MiningDomain, id: bigint, owner: Address, difficulty: number): bigint {
  if (difficulty > 32) throw new Error(`PoW difficulty ${difficulty} exceeds the 32-byte hash`);
  for (let nonce = 0n; nonce <= 0xffff_ffff_ffff_ffffn; nonce++) {
    if (hexToBytes(miningPowHash(domain, id, owner, nonce)).subarray(0, difficulty).every((byte) => byte === 0)) return nonce;
  }
  throw new Error("no proof-of-work nonce below 2^64");
}
