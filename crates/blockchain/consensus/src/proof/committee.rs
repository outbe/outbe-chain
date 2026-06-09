//! Canonical V2 committee snapshot types and pure-function hashers.
//!
//! This module is the **single source of truth** for the wire-visible committee
//! snapshot shape used by every V2 consensus-proof path (Phase 1 verifier,
//! certified-parent proof store, Rewards/Slash fingerprints, slashing evidence
//! dedup, and the `apply_boundary_outcome` writer that seeds
//! `CommitteeSnapshotStore`). Everything in this file is pure data and pure
//! arithmetic — no storage, no errors, no async — so it can be reused by full
//! nodes that have no validator runtime, and by the EVM executor that has no
//! consensus stack.
//!

use alloy_primitives::{Address, B256};

/// Initial VRF material version at genesis.
pub const VRF_MATERIAL_VERSION_GENESIS: u64 = 0;

/// Domain separation tag for the V2 committee set hash.
///
/// Must remain byte-for-byte stable: it is part of the V2 consensus binding
/// and any change forks the chain.
pub const OUTBE_COMMITTEE_SET_HASH_V2_DOMAIN: &[u8] = b"OUTBE_COMMITTEE_SNAPSHOT_V2";

/// Domain separation tag for the V2 committee snapshot storage key.
///
/// Distinct from [`OUTBE_COMMITTEE_SET_HASH_V2_DOMAIN`] so the snapshot key
/// and the committee hash are statistically and semantically unrelated.
pub const OUTBE_COMMITTEE_SNAPSHOT_KEY_V2_DOMAIN: &[u8] = b"OUTBE_COMMITTEE_SNAPSHOT_KEY_V2";

/// One ordered entry of a committee snapshot.
///
/// The pubkey is the 48-byte BLS12-381 MinPk consensus public key, matching
/// the `val_consensus_pubkey_lo/hi` schema layout in
/// `outbe-validatorset::schema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitteeEntry {
    pub address: Address,
    pub consensus_pubkey: [u8; 48],
}

/// Canonical, ordered V2 committee snapshot.
///
/// `committee` is in Commonware `ordered::Set<bls12381::PublicKey>` order
/// (the same order used by the certificate signer bitmap). It is not
/// registration order and not sorted-by-address order. Re-ordering the
/// vector changes [`committee_set_hash_v2`].
///
/// `vrf_group_public_key_bytes` is the raw output of
/// `commonware_codec::Encode::encode(polynomial.public())` for the active
/// DKG output. Storing the raw bytes (instead of just a hash) lets full
/// nodes reconstruct the VRF verifier from state alone, without rerunning
/// the DKG.
///
/// The epoch is intentionally *not* part of the snapshot struct because the
/// snapshot store is keyed by `(epoch, committee_set_hash)`; callers pass
/// `epoch` to the hashing/keying helpers explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitteeSnapshot {
    pub committee: Vec<CommitteeEntry>,
    pub vrf_material_version: u64,
    pub vrf_group_public_key_bytes: Vec<u8>,
}

impl CommitteeSnapshot {
    /// Returns the canonical V2 committee hash for this snapshot at `epoch`.
    pub fn committee_set_hash_v2(&self, epoch: u64) -> B256 {
        committee_set_hash_v2(epoch, self)
    }

    /// Returns the canonical V2 storage key for this snapshot at `epoch`.
    pub fn snapshot_key(&self, epoch: u64) -> B256 {
        committee_snapshot_key(epoch, self.committee_set_hash_v2(epoch))
    }
}

/// Canonical V2 committee set hash.
///
/// Layout:
///
/// ```text
/// keccak256(
///     "OUTBE_COMMITTEE_SNAPSHOT_V2"
///  || epoch.to_be_bytes()                       // 8  bytes, big-endian
///  || (committee.len() as u64).to_be_bytes()    // 8  bytes, big-endian
///  || ( address_20_bytes || min_pk_pubkey_48_bytes ) * committee.len()
///  || vrf_material_version.to_be_bytes()        // 8  bytes, big-endian
///  || (vrf_group_pk.len() as u64).to_be_bytes() // 8  bytes, big-endian
///  || vrf_group_public_key_bytes                // variable
/// )
/// ```
///
/// The domain prefix and the per-entry pubkey both differ from the legacy
/// address-only `hash_active_set`; equality between the two hashes for any
/// committee is a fingerprint mismatch (tested explicitly).
pub fn committee_set_hash_v2(epoch: u64, snapshot: &CommitteeSnapshot) -> B256 {
    let committee_len_bytes = (snapshot.committee.len() as u64).to_be_bytes();
    let vrf_pk_len_bytes = (snapshot.vrf_group_public_key_bytes.len() as u64).to_be_bytes();

    let capacity = OUTBE_COMMITTEE_SET_HASH_V2_DOMAIN.len()
        + 8
        + 8
        + snapshot.committee.len() * (20 + 48)
        + 8
        + 8
        + snapshot.vrf_group_public_key_bytes.len();
    let mut buf = Vec::with_capacity(capacity);
    buf.extend_from_slice(OUTBE_COMMITTEE_SET_HASH_V2_DOMAIN);
    buf.extend_from_slice(&epoch.to_be_bytes());
    buf.extend_from_slice(&committee_len_bytes);
    for entry in &snapshot.committee {
        buf.extend_from_slice(entry.address.as_slice());
        buf.extend_from_slice(&entry.consensus_pubkey);
    }
    buf.extend_from_slice(&snapshot.vrf_material_version.to_be_bytes());
    buf.extend_from_slice(&vrf_pk_len_bytes);
    buf.extend_from_slice(&snapshot.vrf_group_public_key_bytes);
    alloy_primitives::keccak256(&buf)
}

/// Canonical V2 committee snapshot storage key.
///
/// Layout:
///
/// ```text
/// keccak256(
///     "OUTBE_COMMITTEE_SNAPSHOT_KEY_V2"
///  || epoch.to_be_bytes()        // 8  bytes, big-endian
///  || committee_set_hash         // 32 bytes
/// )
/// ```
pub fn committee_snapshot_key(epoch: u64, committee_set_hash: B256) -> B256 {
    let capacity = OUTBE_COMMITTEE_SNAPSHOT_KEY_V2_DOMAIN.len() + 8 + 32;
    let mut buf = Vec::with_capacity(capacity);
    buf.extend_from_slice(OUTBE_COMMITTEE_SNAPSHOT_KEY_V2_DOMAIN);
    buf.extend_from_slice(&epoch.to_be_bytes());
    buf.extend_from_slice(committee_set_hash.as_slice());
    alloy_primitives::keccak256(&buf)
}
