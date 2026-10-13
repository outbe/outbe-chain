//! Poseidon2 hashes — thin sponges over the stdlib
//! `poseidon2_permutation` (bb's Poseidon2). `iv = len << 64` seeds the
//! capacity, the rate is absorbed in blocks of 3, then a final permutation
//! squeezes `state[0]`.
//!
//! This MUST stay bit-identical to `outbe_poseidon::poseidon2::hash` off-circuit
//! (same iv, same block/squeeze framing) — that equality is what makes the
//! Rust-computed public inputs match the in-circuit ones.

/// `Poseidon2(a, b)` — one rate block (2 < 3), single permutation.
pub fn hash_2(inputs: [Field; 2]) -> Field {
    let iv: Field = 36893488147419103232; // 2 << 64
    std::hash::poseidon2_permutation([inputs[0], inputs[1], 0, iv])[0]
}

/// `Poseidon2(a, b, c)` — a full rate block: absorb-permute, then squeeze-permute.
pub fn hash_3(inputs: [Field; 3]) -> Field {
    let iv: Field = 55340232221128654848; // 3 << 64
    let s1 = std::hash::poseidon2_permutation([inputs[0], inputs[1], inputs[2], iv]);
    std::hash::poseidon2_permutation(s1)[0]
}

/// Iterated Poseidon2 hash under a caller-supplied domain.
pub fn hash_multi<let N: u32>(domain: Field, inputs: [Field; N]) -> Field {
    let mut state = hash_2([domain, N as Field]);
    for input in inputs {
        state = hash_2([state, input]);
    }
    state
}
