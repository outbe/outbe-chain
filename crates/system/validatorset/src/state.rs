//! `CommitteeSnapshotStore` — state-backed canonical committee snapshots.
//!
//! The V2 certified-parent accounting flow
//! must verify finalized-parent certificates against the *historical* committee
//! that signed them, not against the current parent-state validator set. At
//! reshare boundaries the parent-state set and the signing set differ, so the
//! verifier needs a deterministic, indexable copy of every active committee
//! keyed by `(epoch, committee_set_hash)`.
//!
//! This module owns the helpers that translate a [`CommitteeSnapshot`] into:
//!
//! * the canonical V2 committee hash ([`committee_set_hash_v2`]); the formula
//!   binds domain, epoch, committee length, ordered `(Address, MinPk pubkey)`
//!   entries, `vrf_material_version`, and the raw encoded VRF group public key
//!   so any drift is a chain split rather than a silent re-encoding;
//! * the storage key ([`committee_snapshot_key`]); the namespace prefix is a
//!   separate domain string so that the snapshot key never collides with the
//!   committee hash itself, even when they share the same `(epoch, hash)`
//!   inputs.
//!
//! The store layout is fixed to ValidatorSet storage slots 31..40 (see
//! [`schema::ValidatorSet`](crate::schema::ValidatorSet)). Writes are
//! field-by-field and **end with the `exists` flag**, so a partial write
//! observed via a checkpoint-rolled-back transaction is never reachable: the
//! reader gates every other slot behind `exists`.

use alloy_primitives::{Address, B256};

use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

use crate::errors::ActivationError;
use crate::schema::ValidatorSet;

// Canonical V2 committee types and pure-function hashers live in
// `outbe-consensus-proof` (the wire-codec crate). They are re-exported here so
// existing `outbe_validatorset::state::{...}` callers keep compiling, and
// internal storage helpers (`write/read_committee_snapshot`,
// `snapshot_identity`) reference them through the canonical crate.
pub use outbe_consensus::proof::{
    committee_set_hash_v2, committee_snapshot_key, CommitteeEntry, CommitteeSnapshot,
    OUTBE_COMMITTEE_SET_HASH_V2_DOMAIN, OUTBE_COMMITTEE_SNAPSHOT_KEY_V2_DOMAIN,
    VRF_MATERIAL_VERSION_GENESIS,
};

/// Returns the next `vrf_material_version` after a successful reshare activation.
///
/// invariant: the version is strictly
/// monotonic, incremented by exactly 1, and **never saturates**. Overflow at
/// `u64::MAX` is a deterministic activation error — both proposer and
/// validator paths reject the activation rather than silently capping the
/// value, which would otherwise let two distinct DKG outputs share a version
/// and break the V2 metadata binding.
pub fn next_vrf_material_version(previous: u64) -> std::result::Result<u64, ActivationError> {
    previous
        .checked_add(1)
        .ok_or(ActivationError::VrfVersionOverflow)
}

/// Splits the 48-byte BLS MinPk pubkey into the schema's `lo`/`hi` halves.
fn split_pubkey(pubkey: &[u8; 48]) -> (B256, B256) {
    let lo = B256::from_slice(&pubkey[..32]);
    let mut hi_bytes = [0u8; 32];
    hi_bytes[..16].copy_from_slice(&pubkey[32..48]);
    (lo, B256::from(hi_bytes))
}

/// Rejoins the schema's `lo`/`hi` halves back into a 48-byte pubkey.
fn join_pubkey(lo: B256, hi: B256) -> [u8; 48] {
    let mut pubkey = [0u8; 48];
    pubkey[..32].copy_from_slice(&lo.0);
    pubkey[32..48].copy_from_slice(&hi.0[..16]);
    pubkey
}

/// Number of recent epochs whose committee snapshots stay live. Every reader
/// (`read_committee_snapshot`) only touches the current finalized epoch ± the
/// K-block late-finalize window (≪ 1 epoch), so this is a generous retention;
/// `write_committee_snapshot` prunes older snapshots to bound state growth.
/// Changing it is a hard fork (it changes which slots are zero → the state root).
pub const COMMITTEE_SNAPSHOT_RETAIN_EPOCHS: u64 = 8;

/// Zeroes every slot of the committee snapshot at `key` — the inverse of
/// [`write_committee_snapshot`] — reclaiming its EVM storage (a slot set to its
/// default is empty in the state trie). `exists` is cleared FIRST so any read
/// during the clear sees the snapshot as already gone. No-op if absent.
pub fn clear_committee_snapshot(storage: StorageHandle, key: B256) -> Result<()> {
    let vs = ValidatorSet::new(storage);
    if !vs.committee_snapshot_exists.read(&key)? {
        return Ok(());
    }
    vs.committee_snapshot_exists.write(&key, false)?;

    let committee_len = vs.committee_snapshot_len.read(&key)?;
    for i in 0..committee_len {
        vs.committee_snapshot_address_at
            .get_nested(&key)
            .write(&i, Address::ZERO)?;
        vs.committee_snapshot_pubkey_lo_at
            .get_nested(&key)
            .write(&i, B256::ZERO)?;
        vs.committee_snapshot_pubkey_hi_at
            .get_nested(&key)
            .write(&i, B256::ZERO)?;
    }
    vs.committee_snapshot_len.write(&key, 0)?;

    let vrf_pk_len = vs.committee_snapshot_vrf_group_public_key_len.read(&key)?;
    let num_chunks = if vrf_pk_len > 0 {
        vrf_pk_len.div_ceil(32)
    } else {
        0
    };
    for i in 0..num_chunks {
        vs.committee_snapshot_vrf_group_public_key_chunk_at
            .get_nested(&key)
            .write(&i, B256::ZERO)?;
    }
    vs.committee_snapshot_vrf_material_version.write(&key, 0)?;
    vs.committee_snapshot_vrf_group_public_key_hash
        .write(&key, B256::ZERO)?;
    vs.committee_snapshot_vrf_group_public_key_len
        .write(&key, 0)?;
    Ok(())
}

/// Writes a committee snapshot into the store at the canonical
/// `(epoch, committee_set_hash)` key derived from `snapshot`.
///
/// Returns `(committee_set_hash, snapshot_key)`. The function is "atomic per
/// boundary block" in the sense that all writes happen inside the current EVM
/// journal — wrap the caller in a [`outbe_primitives::storage::CheckpointGuard`]
/// to roll back on artifact rejection.
///
/// The `exists` flag is intentionally written *last*: even if a checkpoint
/// commit observes a partial write (e.g., because of an out-of-gas error
/// mid-write), no reader will treat the half-written snapshot as present.
pub fn write_committee_snapshot(
    storage: StorageHandle,
    epoch: u64,
    snapshot: &CommitteeSnapshot,
) -> Result<(B256, B256)> {
    let hash = committee_set_hash_v2(epoch, snapshot);
    let key = committee_snapshot_key(epoch, hash);

    let committee_len: u64 = snapshot
        .committee
        .len()
        .try_into()
        .map_err(|_| PrecompileError::Revert("committee snapshot length exceeds u64".into()))?;
    let vrf_pk_len: u64 = snapshot
        .vrf_group_public_key_bytes
        .len()
        .try_into()
        .map_err(|_| PrecompileError::Revert("vrf group pk bytes length exceeds u64".into()))?;
    let vrf_pk_hash = alloy_primitives::keccak256(&snapshot.vrf_group_public_key_bytes);

    let vs = ValidatorSet::new(storage.clone());

    vs.committee_snapshot_len.write(&key, committee_len)?;
    for (i, entry) in snapshot.committee.iter().enumerate() {
        let idx = i as u64;
        vs.committee_snapshot_address_at
            .get_nested(&key)
            .write(&idx, entry.address)?;

        let (lo, hi) = split_pubkey(&entry.consensus_pubkey);
        vs.committee_snapshot_pubkey_lo_at
            .get_nested(&key)
            .write(&idx, lo)?;
        vs.committee_snapshot_pubkey_hi_at
            .get_nested(&key)
            .write(&idx, hi)?;
    }
    vs.committee_snapshot_vrf_material_version
        .write(&key, snapshot.vrf_material_version)?;
    vs.committee_snapshot_vrf_group_public_key_hash
        .write(&key, vrf_pk_hash)?;
    vs.committee_snapshot_vrf_group_public_key_len
        .write(&key, vrf_pk_len)?;
    for (i, chunk) in snapshot.vrf_group_public_key_bytes.chunks(32).enumerate() {
        let idx = i as u64;
        let mut buf = [0u8; 32];
        buf[..chunk.len()].copy_from_slice(chunk);
        vs.committee_snapshot_vrf_group_public_key_chunk_at
            .get_nested(&key)
            .write(&idx, B256::from(buf))?;
    }

    // `exists` LAST: gates every read path on a fully-written snapshot.
    vs.committee_snapshot_exists.write(&key, true)?;

    // Prune ring: retain only the last COMMITTEE_SNAPSHOT_RETAIN_EPOCHS epochs.
    // A boundary writes outgoing(epoch-1) + incoming(epoch) — distinct epochs →
    // distinct ring slots. Writing epoch E evicts the snapshot from epoch E-RETAIN.
    let ring_idx = epoch % COMMITTEE_SNAPSHOT_RETAIN_EPOCHS;
    let evicted = vs.committee_snapshot_key_ring.read(&ring_idx)?;
    if evicted != B256::ZERO && evicted != key {
        clear_committee_snapshot(storage.clone(), evicted)?;
    }
    vs.committee_snapshot_key_ring.write(&ring_idx, key)?;

    Ok((hash, key))
}

/// Reads a previously-written committee snapshot from the store, or returns
/// `Ok(None)` when no snapshot exists at `snapshot_key`.
///
/// Returns the snapshot data without `epoch`; the caller already supplied the
/// `(epoch, committee_set_hash)` pair that produced `snapshot_key`.
pub fn read_committee_snapshot(
    storage: StorageHandle,
    snapshot_key: B256,
) -> Result<Option<CommitteeSnapshot>> {
    let vs = ValidatorSet::new(storage);
    if !vs.committee_snapshot_exists.read(&snapshot_key)? {
        return Ok(None);
    }

    let committee_len = vs.committee_snapshot_len.read(&snapshot_key)?;
    let mut committee = Vec::with_capacity(committee_len as usize);
    for i in 0..committee_len {
        let address = vs
            .committee_snapshot_address_at
            .get_nested(&snapshot_key)
            .read(&i)?;
        let lo: B256 = vs
            .committee_snapshot_pubkey_lo_at
            .get_nested(&snapshot_key)
            .read(&i)?;
        let hi: B256 = vs
            .committee_snapshot_pubkey_hi_at
            .get_nested(&snapshot_key)
            .read(&i)?;
        committee.push(CommitteeEntry {
            address,
            consensus_pubkey: join_pubkey(lo, hi),
        });
    }

    let vrf_material_version = vs
        .committee_snapshot_vrf_material_version
        .read(&snapshot_key)?;
    let vrf_pk_len = vs
        .committee_snapshot_vrf_group_public_key_len
        .read(&snapshot_key)?;
    let vrf_pk_len_usize: usize = vrf_pk_len
        .try_into()
        .map_err(|_| PrecompileError::Revert("vrf group pk length exceeds usize".into()))?;
    let mut vrf_group_public_key_bytes = Vec::with_capacity(vrf_pk_len_usize);
    if vrf_pk_len > 0 {
        let num_chunks = vrf_pk_len.div_ceil(32);
        let last_chunk_take = (vrf_pk_len % 32) as usize;
        let last_chunk_take = if last_chunk_take == 0 {
            32
        } else {
            last_chunk_take
        };
        for i in 0..num_chunks {
            let chunk: B256 = vs
                .committee_snapshot_vrf_group_public_key_chunk_at
                .get_nested(&snapshot_key)
                .read(&i)?;
            let take = if i + 1 == num_chunks {
                last_chunk_take
            } else {
                32
            };
            vrf_group_public_key_bytes.extend_from_slice(&chunk.0[..take]);
        }
    }

    Ok(Some(CommitteeSnapshot {
        committee,
        vrf_material_version,
        vrf_group_public_key_bytes,
    }))
}

/// Pre-computes `(committee_set_hash, snapshot_key)` without touching storage.
///
/// Useful for callers that need the key before deciding whether to write
/// (e.g., dedup checks).
pub fn snapshot_identity(epoch: u64, snapshot: &CommitteeSnapshot) -> (B256, B256) {
    let hash = committee_set_hash_v2(epoch, snapshot);
    let key = committee_snapshot_key(epoch, hash);
    (hash, key)
}
