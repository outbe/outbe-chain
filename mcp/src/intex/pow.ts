import { type Address, type Hex, concat, sha256, toBytes, toHex } from "viem";

/**
 * Proof-of-work for IntexFactory.minePromis. The precompile requires a nonce
 * such that the PoW hash has `powDifficulty()` leading zero bytes. Difficulty is
 * tiny (1 byte => ~256 tries), so a single-threaded grind is instant.
 *
 * Scheme verbatim from crates/core/intexfactory/src/runtime.rs (compute_pow_hash):
 *   preimage = holder[20] ++ promisMinor_be32 ++ seriesId[14] ++ seq_be4
 *   hash     = SHA256(preimage ++ nonce_be8)
 *   valid    = first `difficulty` bytes of hash are zero
 * `seq` is the per-(series, holder) mine counter. Read it as the count of past
 * PromisMined(series, holder) events. promisMinor = series.promisLoadMinor * units.
 */

/** The raw preimage bytes, matching the Rust concatenation. */
function preimage(holder: Address, promisMinor: bigint, seriesId: Hex, seq: number): Uint8Array {
  return concat([
    toBytes(holder),
    toBytes(toHex(promisMinor, { size: 32 })),
    toBytes(seriesId),
    toBytes(toHex(seq, { size: 4 })),
  ]);
}

export interface PowSolution {
  nonce: bigint;
  iterations: number;
  hash: Hex;
}

/** Grind a nonce whose PoW hash has `difficulty` leading zero bytes. */
export function grindNonce(
  holder: Address,
  promisMinor: bigint,
  seriesId: Hex,
  seq: number,
  difficulty: number,
): PowSolution {
  if (difficulty > 32) throw new Error(`PoW difficulty ${difficulty} exceeds the 32-byte hash`);
  const prefix = preimage(holder, promisMinor, seriesId, seq);
  // The precompile caps nonce at u64::MAX. Difficulty 1 resolves far below that.
  for (let nonce = 0n; nonce <= 0xffff_ffff_ffff_ffffn; nonce++) {
    const data = concat([prefix, toBytes(toHex(nonce, { size: 8 }))]);
    const hash = sha256(data, "bytes");
    let ok = true;
    for (let i = 0; i < difficulty; i++) {
      if (hash[i] !== 0) {
        ok = false;
        break;
      }
    }
    if (ok) return { nonce, iterations: Number(nonce) + 1, hash: toHex(hash) };
  }
  throw new Error("no PoW nonce found within u64 range");
}
