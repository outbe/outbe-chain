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
///
/// Ids are assigned in ascending amount order. [`Gratis0_1`](Self::Gratis0_1)
/// is a reserved sub-rung that exists only as the destination for a single
/// anadosis installment's reclaim note (one decade below the smallest
/// pledgeable rung). It cannot be pledged directly — see
/// [`is_pledgeable`](Self::is_pledgeable).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenomAmount {
    /// Reserved anadosis-only sub-rung (0.1 GRATIS). Not directly pledgeable.
    Gratis0_1 = 1,
    Gratis1 = 2,
    Gratis10 = 3,
    Gratis100 = 4,
    Gratis1k = 5,
    Gratis10k = 6,
}

impl DenomAmount {
    pub const ALL: [DenomAmount; 6] = [
        Self::Gratis0_1,
        Self::Gratis1,
        Self::Gratis10,
        Self::Gratis100,
        Self::Gratis1k,
        Self::Gratis10k,
    ];

    pub const fn id(self) -> u8 {
        self as u8
    }

    pub fn amount(self) -> U256 {
        let gratis = |g: u64| U256::from(g) * ONE_COEN;
        match self {
            Self::Gratis0_1 => ONE_COEN / U256::from(10u64),
            Self::Gratis1 => gratis(1),
            Self::Gratis10 => gratis(10),
            Self::Gratis100 => gratis(100),
            Self::Gratis1k => gratis(1_000),
            Self::Gratis10k => gratis(10_000),
        }
    }

    /// Whether users may open a pledge in this denomination. The reserved
    /// sub-rung [`Gratis0_1`](Self::Gratis0_1) exists only as the destination
    /// for a single anadosis installment's reclaim note, so it cannot be
    /// pledged directly.
    pub const fn is_pledgeable(self) -> bool {
        !matches!(self, Self::Gratis0_1)
    }

    /// The denomination one decade down — the pool a single anadosis
    /// installment's reclaim note lives in. Its [`amount`](Self::amount) is
    /// exactly `self.amount() / 10` (credisfactory's `NUMBER_OF_ANADOSIS = 10`),
    /// so a later `unpledgeGratis` of that note releases one installment's
    /// share. Returns `None` for the reserved floor
    /// [`Gratis0_1`](Self::Gratis0_1), which has no decade below it and is
    /// therefore not credis-eligible.
    pub fn anadosis_denomination(self) -> Option<DenomAmount> {
        match self {
            Self::Gratis10k => Some(Self::Gratis1k),
            Self::Gratis1k => Some(Self::Gratis100),
            Self::Gratis100 => Some(Self::Gratis10),
            Self::Gratis10 => Some(Self::Gratis1),
            Self::Gratis1 => Some(Self::Gratis0_1),
            Self::Gratis0_1 => None,
        }
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
