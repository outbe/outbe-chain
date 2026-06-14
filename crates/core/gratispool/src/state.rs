//! Merkle-tree + nullifier-set state operations for the shielded gratis pool.
//!
//! All hashes go through `outbe_poseidon::Poseidon::<Fr>::new_circom(n)`, the
//! same Circom parameter set the Noir circuit witnesses inside the proof.
//! That parity is the load-bearing assumption that lets `verify_and_spend_*`
//! check a proof's `merkle_root` public input against an on-chain-recomputed
//! root without re-hashing field-element-by-field-element on both sides.

use alloy_primitives::{Address, U256};
use ark_bn254::Fr;
use ark_ff::{BigInteger, PrimeField};
use outbe_poseidon::{Poseidon, PoseidonHasher};

use outbe_primitives::error::Result;

use crate::constants::{
    MERKLE_DEPTH, ROOT_WINDOW, TAG_BINDING, TAG_COMMIT_GRATIS, TAG_MERKLE_GRATIS,
    TAG_NULLIFIER_GRATIS, ZERO_LEAF,
};
use crate::errors::GratisPoolError;
use crate::schema::GratisPoolContract;

// ---------------------------------------------------------------------------
// Field-element conversion
// ---------------------------------------------------------------------------

/// `U256 → Fr` via BE-encoded bytes mod-reduced to the BN254 scalar field.
///
/// Matches `outbe_zkproof::poseidon::poseidon_hash` and the Noir circuit's
/// in-witness interpretation of a public input.
fn u256_to_fr(x: U256) -> Fr {
    let bytes: [u8; 32] = x.to_be_bytes();
    Fr::from_be_bytes_mod_order(&bytes)
}

/// `u64 → Fr` for tag and action constants.
fn u64_to_fr(x: u64) -> Fr {
    Fr::from(x)
}

/// `Address → Fr` — the 20-byte address is padded to 32 BE bytes and
/// mod-reduced into the scalar field.
fn address_to_fr(addr: Address) -> Fr {
    let mut buf = [0u8; 32];
    buf[12..32].copy_from_slice(addr.as_slice());
    Fr::from_be_bytes_mod_order(&buf)
}

/// `Fr → U256` via BE-encoded bytes.
fn fr_to_u256(x: Fr) -> U256 {
    let be = x.into_bigint().to_bytes_be();
    let mut out = [0u8; 32];
    let off = 32 - be.len().min(32);
    out[off..].copy_from_slice(&be[be.len().saturating_sub(32)..]);
    U256::from_be_bytes(out)
}

/// Reject a `U256` that is not the canonical (`< p`) representative of a BN254
/// scalar-field element.
///
/// The proof's field-element public inputs are transmitted as 32 raw bytes and
/// handed to the Barretenberg verifier without a canonical-form check; the
/// backend reduces them modulo the scalar field `p`, so `N`, `N + p`, … all
/// verify as the same field element. Any of those values that the runtime then
/// uses as *state* — most importantly the `nullifier_spent` set key — must be
/// canonicalised first, otherwise `N` and `N + p` are distinct keys for the
/// same nullifier and a note can be spent more than once.
pub fn require_canonical_field(x: U256) -> Result<U256> {
    if fr_to_u256(u256_to_fr(x)) != x {
        return Err(GratisPoolError::NonCanonicalFieldInput.into());
    }
    Ok(x)
}

/// Variadic Poseidon helper. Constructs a fresh `Poseidon` with `inputs.len()`
/// arity and returns the hash as a `U256` (so callers can store / compare it
/// in storage directly).
fn poseidon(inputs: &[Fr]) -> Result<U256> {
    let mut hasher = Poseidon::<Fr>::new_circom(inputs.len())
        .map_err(|e| GratisPoolError::PoseidonFailed(e.to_string()))?;
    let h = hasher
        .hash(inputs)
        .map_err(|e| GratisPoolError::PoseidonFailed(e.to_string()))?;
    Ok(fr_to_u256(h))
}

// ---------------------------------------------------------------------------
// Public helpers — formulas exposed to runtime / tests
// ---------------------------------------------------------------------------

/// `commitment = poseidon(TAG_COMMIT_GRATIS, secret, nullifier_secret, denom_id)`.
pub fn commitment_hash(secret: U256, nullifier_secret: U256, denom_id: u8) -> Result<U256> {
    poseidon(&[
        u64_to_fr(TAG_COMMIT_GRATIS),
        u256_to_fr(secret),
        u256_to_fr(nullifier_secret),
        u64_to_fr(denom_id as u64),
    ])
}

/// `nullifier_hash = poseidon(TAG_NULLIFIER_GRATIS, nullifier_secret)`.
pub fn nullifier_hash(nullifier_secret: U256) -> Result<U256> {
    poseidon(&[
        u64_to_fr(TAG_NULLIFIER_GRATIS),
        u256_to_fr(nullifier_secret),
    ])
}

/// `receiver_binding = poseidon(TAG_BINDING, action_tag, target_address, chain_id, nonce)`.
///
/// Runtime calls this on the on-chain `(action_tag, target_address,
/// chain_id, nonce)` and asserts equality against the `receiver_binding`
/// public input the prover committed to inside the proof. Mismatch means
/// the proof was built for a different action, destination, or
/// context-binding payload and must be rejected.
///
/// The `nonce` slot is the application-derived context-binding payload:
///
/// - `ACTION_REQUEST_CREDIS` — the credis position's `reclaim_commitment`.
///   This binds the proof to the *exact* reclaim leg the caller is
///   registering, so a mempool front-runner cannot substitute their own
///   reclaim commitment and capture the eventual `unpledgeGratis`.
/// - `ACTION_UNPLEDGE` — `U256::ZERO`. Unpledge is terminal with no
///   follow-up artifact, so the binding slot stays unused.
pub fn receiver_binding(
    action_tag: u64,
    target: Address,
    chain_id: u64,
    nonce: U256,
) -> Result<U256> {
    poseidon(&[
        u64_to_fr(TAG_BINDING),
        u64_to_fr(action_tag),
        address_to_fr(target),
        u64_to_fr(chain_id),
        u256_to_fr(nonce),
    ])
}

/// Per-level "empty subtree" hashes.
///
/// `merkle_zeros()[i]` is the Poseidon hash of two `merkle_zeros()[i-1]`
/// children, bottomed out by `ZERO_LEAF` at level 0. Used as the right
/// sibling when inserting a new left-child leaf, and as the canonical empty
/// subtree any unfilled level above the frontier.
///
/// Computed fresh per insert — 20 hashes amortised across the
/// [`MERKLE_DEPTH`] path-up — but is deterministic so we could memoise via a
/// `LazyLock` if profiling demands it.
fn merkle_zeros() -> Result<Vec<U256>> {
    let mut zeros = Vec::with_capacity(MERKLE_DEPTH as usize + 1);
    zeros.push(ZERO_LEAF);
    for i in 0..MERKLE_DEPTH {
        let prev = zeros[i as usize];
        let parent = poseidon(&[
            u64_to_fr(TAG_MERKLE_GRATIS),
            u256_to_fr(prev),
            u256_to_fr(prev),
        ])?;
        zeros.push(parent);
    }
    Ok(zeros)
}

/// `node = poseidon_2(TAG_MERKLE_GRATIS + left, right)`.
///
/// Matches the upstream `outbe-commitment-nullifier-circuit`'s
/// `merkle_node` byte-for-byte: arity-2 Poseidon with
/// the domain-separator tag folded into the *left* input via field
/// addition. Any deviation (different arity, tag as a separate input, etc.)
/// produces a different Merkle root for the same `(left, right)` pair and
/// breaks proof / runtime parity.
pub fn merkle_node(left: U256, right: U256) -> Result<U256> {
    let tagged_left = u64_to_fr(TAG_MERKLE_GRATIS) + u256_to_fr(left);
    let mut hasher = Poseidon::<Fr>::new_circom(2)
        .map_err(|e| GratisPoolError::PoseidonFailed(e.to_string()))?;
    let h = hasher
        .hash(&[tagged_left, u256_to_fr(right)])
        .map_err(|e| GratisPoolError::PoseidonFailed(e.to_string()))?;
    Ok(fr_to_u256(h))
}

// ---------------------------------------------------------------------------
// Pool state ops (impl block on the macro-generated GratisPoolContract<'_>)
// ---------------------------------------------------------------------------

impl GratisPoolContract<'_> {
    /// Append `commitment` as the next leaf in `denom_id`'s tree.
    ///
    /// Walks up the Tornado-style incremental Merkle state machine:
    ///   - At each level, fold `current` with either the stored
    ///     `filled_subtrees[level]` (if this is a right child) or with
    ///     `merkle_zeros[level]` (if this is a left child, in which case we
    ///     also update `filled_subtrees[level] = current`).
    ///   - The final folded value is the new root, which is pushed into the
    ///     `ROOT_WINDOW`-sized ring buffer.
    ///
    /// Returns the new tree root and the new leaf's index.
    pub(crate) fn append_leaf(&mut self, denom_id: u8, commitment: U256) -> Result<(U256, u32)> {
        // Atomic check-and-insert: `Set::insert` returns `true` iff the value
        // was newly added; `false` means the commitment was already present
        // and the call is a duplicate. No subsequent `commitment_exists`
        // write is needed.
        if !self.commitment_exists.insert(commitment)? {
            return Err(GratisPoolError::CommitmentDuplicate.into());
        }
        let leaf_index = self.next_index.read(&(denom_id as u32))?;
        let capacity: u32 = 1u32 << MERKLE_DEPTH;
        if leaf_index >= capacity {
            return Err(GratisPoolError::TreeFull.into());
        }

        let zeros = merkle_zeros()?;
        let mut current = commitment;
        let mut index = leaf_index;

        for level in 0..MERKLE_DEPTH {
            let key = GratisPoolContract::level_key(denom_id, level);
            let (left, right) = if index & 1 == 0 {
                // current is a left child: parent's right sibling is the
                // canonical empty subtree at this level; record current as
                // the new frontier for this level.
                self.filled_subtrees.write(&key, current)?;
                (current, zeros[level as usize])
            } else {
                // current is a right child: combine with the stored left
                // sibling (frontier at this level).
                let left_sibling = self.filled_subtrees.read(&key)?;
                (left_sibling, current)
            };
            current = merkle_node(left, right)?;
            index >>= 1;
        }

        self.next_index.write(&(denom_id as u32), leaf_index + 1)?;
        // commitment_exists was already updated by the `insert` at the top of
        // this function — no second write needed.
        self.push_root(denom_id, current)?;
        Ok((current, leaf_index))
    }

    /// Push `root` into the per-denomination `ROOT_WINDOW`-slot ring buffer.
    ///
    /// `current_root_index[denom_id]` is incremented modulo `ROOT_WINDOW`;
    /// the new slot is overwritten with the new root. Older roots silently
    /// drop out of the window once the buffer wraps.
    fn push_root(&mut self, denom_id: u8, root: U256) -> Result<()> {
        let head = self.current_root_index.read(&(denom_id as u32))?;
        let next = (head + 1) % ROOT_WINDOW;
        let slot_key = GratisPoolContract::level_key(denom_id, next);
        self.roots.write(&slot_key, root)?;
        self.current_root_index.write(&(denom_id as u32), next)?;
        Ok(())
    }

    /// `true` iff `root` matches any of the last [`ROOT_WINDOW`] roots
    /// recorded for `denom_id`.
    ///
    /// PoC search: linear scan over `ROOT_WINDOW` entries. With `ROOT_WINDOW
    /// = 30` that's 30 SLOADs per verify; reasonable until the window grows.
    pub(crate) fn has_root_in_window(&self, denom_id: u8, root: U256) -> Result<bool> {
        // `0` is never a legitimate root (an empty tree's root is
        // `merkle_zeros[MERKLE_DEPTH]`, which is non-zero). Reject it
        // explicitly so an unwritten ring-buffer slot — which reads back as
        // `U256::ZERO` before the window fills — can never be matched as a
        // valid historical root.
        if root == U256::ZERO {
            return Ok(false);
        }
        for slot in 0..ROOT_WINDOW {
            let key = GratisPoolContract::level_key(denom_id, slot);
            if self.roots.read(&key)? == root {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Read the current root for `denom_id` (the most-recently-written ring
    /// buffer slot). Returns [`U256::ZERO`] if no commitments have been
    /// inserted yet.
    pub fn current_root(&self, denom_id: u8) -> Result<U256> {
        let head = self.current_root_index.read(&(denom_id as u32))?;
        let key = GratisPoolContract::level_key(denom_id, head);
        self.roots.read(&key)
    }

    /// Number of commitments appended to `denom_id`'s tree so far.
    pub fn leaf_count(&self, denom_id: u8) -> Result<u32> {
        self.next_index.read(&(denom_id as u32))
    }
}
