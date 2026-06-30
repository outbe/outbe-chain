//! Compile-time parameters of the shielded gratis pool.
//!
//! These values are baked into the `outbe-commitment-nullifier-circuit`
//! Noir program shipped by `outbe-circuits`. Any change here requires a
//! matching change in that circuit (upstream repo) and a regenerated
//! canonical verification key in `outbe-zk-canonical`.

use alloy_primitives::U256;
use outbe_primitives::units::ONE_COEN;

use crate::errors::GratisPoolError;

/// A supported gratis denomination.
///
/// Each variant is a separate Tornado-style anonymity pool with a fixed deposit
/// amount; fixed amounts ensure the pledge amount itself is not a unique
/// fingerprint that could link a `pledgeGratis` to a later spend.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenomAmount {
    Gratis1 = 1,
    Gratis10 = 2,
    Gratis100 = 3,
    Gratis1k = 4,
    Gratis10k = 5,
}

impl DenomAmount {
    pub const ALL: [DenomAmount; 5] = [
        Self::Gratis1,
        Self::Gratis10,
        Self::Gratis100,
        Self::Gratis1k,
        Self::Gratis10k,
    ];

    pub fn from_id(denom_id: u8) -> Option<Self> {
        Self::try_from(denom_id).ok()
    }

    pub const fn id(self) -> u8 {
        self as u8
    }

    pub fn amount(self) -> U256 {
        let gratis = |g: u64| U256::from(g) * ONE_COEN;
        match self {
            Self::Gratis1 => gratis(1),
            Self::Gratis10 => gratis(10),
            Self::Gratis100 => gratis(100),
            Self::Gratis1k => gratis(1_000),
            Self::Gratis10k => gratis(10_000),
        }
    }

    pub fn anadosis_denomination(self) -> U256 {
        self.amount() / U256::from(10u64)
    }
}

impl TryFrom<u8> for DenomAmount {
    type Error = GratisPoolError;

    /// Resolves the on-chain `denom_id`, or [`GratisPoolError::DenomUnknown`].
    fn try_from(denom_id: u8) -> Result<Self, Self::Error> {
        Self::ALL
            .iter()
            .copied()
            .find(|denom| denom.id() == denom_id)
            .ok_or(GratisPoolError::DenomUnknown)
    }
}

/// Number of leaves the per-denomination Merkle tree can hold.
///
/// Depth 20 → 2^20 ≈ 1.05M commitments per pool. Plenty of headroom for the
/// PoC; the on-chain cost is one `outbe_poseidon` 2-input hash per level on
/// insert (~20 hashes per `deposit_user` / `insert_reclaim`).
pub const MERKLE_DEPTH: u32 = 20;

/// Number of recent root snapshots retained per denomination.
///
/// A spend proof's `merkle_root` public input must match one of the last
/// `ROOT_WINDOW` roots. Matches (~30 roots
/// supports concurrent operations against slightly stale state).
pub const ROOT_WINDOW: u32 = 30;

// ---------------------------------------------------------------------------
// Poseidon domain-separator tags
// ---------------------------------------------------------------------------
//
// Without a tag a hash collision in one context (commitment) could be reused
// in another (nullifier, Merkle inner node).
// Tags are deployment-fixed small distinct field elements; the same constants
// must match the `outbe-commitment-nullifier-circuit` source in
// `outbe-circuits`.

/// `commitment = poseidon(TAG_COMMIT_GRATIS, secret, nullifier_secret, denom_id)`.
pub const TAG_COMMIT_GRATIS: u64 = 0x6E0_001;

/// `nullifier_hash = poseidon(TAG_NULLIFIER_GRATIS, nullifier_secret)`.
pub const TAG_NULLIFIER_GRATIS: u64 = 0x6E0_002;

/// `node = poseidon(TAG_MERKLE_GRATIS, left, right)` — Merkle inner-node hash.
///
/// Hard-coded into the Noir circuit's `merkle_root_from_path` helper.
pub const TAG_MERKLE_GRATIS: u64 = 0x6E0_003;

/// `receiver_binding = poseidon(TAG_BINDING, action_tag, target_address, chain_id, nonce)`.
///
/// Recomputed by the runtime from on-chain inputs and asserted against the
/// proof's public input. A copy-paste attacker who lifts the proof from the
/// mempool cannot reuse it because they cannot re-prove with a different
/// `target_address` without the secret.
pub const TAG_BINDING: u64 = 0x6E0_004;

// ---------------------------------------------------------------------------
// Spend-path action tags
// ---------------------------------------------------------------------------

/// `action_tag` value for `requestCredis` proofs.
///
/// The runtime recomputes `receiver_binding` with this value and
/// `target_address = msg.sender` for `requestCredis` calls.
pub const ACTION_REQUEST_CREDIS: u64 = 1;

/// `action_tag` value for `unpledgeGratis` proofs.
///
/// The runtime recomputes `receiver_binding` with this value and
/// `target_address = destination` for `unpledgeGratis` calls.
pub const ACTION_UNPLEDGE: u64 = 2;

/// Initial value used to pad Merkle-tree subtrees below the current frontier.
///
/// The tree is fully populated from level 0 up: every empty leaf and every
/// empty subtree above an empty leaf is the recursive hash of this constant.
/// `0` is the standard choice — it matches what the Noir circuit witnesses
/// when computing a sibling path past the frontier.
pub const ZERO_LEAF: U256 = U256::ZERO;
