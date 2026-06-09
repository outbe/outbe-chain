use alloy_primitives::{Address, B256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::TEE_REGISTRY_ADDRESS;

/// EVM storage layout for the TEE Registry.
///
/// Global scalars (slots 0..=8) hold the bootstrap result that clients and
/// verifiers read. Per-validator maps (slots 9..=15) hold each validator's TEE
/// registration bundle keyed by validator address. The layout is append-only;
/// new fields take the next `order`.
#[storage_schema]
#[contract(addr = TEE_REGISTRY_ADDRESS)]
pub struct TeeRegistry {
    /// slot 0: set true once `write_bootstrap` runs (idempotency gate).
    #[attribute(order = 0)]
    pub bootstrapped: outbe_primitives::storage::dsl::Value<bool>,

    /// slot 1: the tribute offer public key clients encrypt to.
    #[attribute(order = 1)]
    pub tribute_offer_public_key: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 2: hash of the genesis TEE policy (mrsigner/mrenclave/min_isv_svn).
    #[attribute(order = 2)]
    pub policy_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 3: key epoch.
    #[attribute(order = 3)]
    pub key_epoch: outbe_primitives::storage::dsl::Value<u64>,

    /// slot 4: tribute-offer-key epoch (HKDF domain separation; reshare-rotation hook).
    #[attribute(order = 4)]
    pub tribute_offer_epoch: outbe_primitives::storage::dsl::Value<u64>,

    /// slot 5: DKG transcript hash.
    #[attribute(order = 5)]
    pub dkg_transcript_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 6: committee snapshot block bootstrap read from.
    #[attribute(order = 6)]
    pub committee_snapshot_block: outbe_primitives::storage::dsl::Value<u64>,

    /// slot 7: committee snapshot hash bootstrap was bound to.
    #[attribute(order = 7)]
    pub committee_snapshot_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 8: number of registered validators.
    #[attribute(order = 8)]
    pub registered_count: outbe_primitives::storage::dsl::Value<u32>,

    /// slot 9: recipient X25519 pubkey per validator.
    #[attribute(order = 9)]
    pub recipient_x25519: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 10: attestation pubkey per validator.
    #[attribute(order = 10)]
    pub attestation_pub: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 11: Noise static pubkey per validator.
    #[attribute(order = 11)]
    pub noise_static_pub: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 12: MRENCLAVE per validator.
    #[attribute(order = 12)]
    pub mrenclave: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 13: MRSIGNER per validator.
    #[attribute(order = 13)]
    pub mrsigner: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 14: ISV SVN per validator.
    #[attribute(order = 14)]
    pub isv_svn: outbe_primitives::storage::dsl::Map<Address, u64>,

    /// slot 15: keys hash (commitment over the validator's TEE keys) per validator.
    #[attribute(order = 15)]
    pub keys_hash: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 16: recipient X25519 pubkey announced via `BoundaryOutcome`
    /// (`DkgBoundaryArtifact::tee_recipient_pubkeys`), per validator. Distinct
    /// from slot 9 (`recipient_x25519`), the authoritative key written by the
    /// full `TeeBootstrap` registration: this is the boundary-channel
    /// announcement (key rotation / pre-bootstrap delivery), recorded
    /// independently of a full registration bundle.
    #[attribute(order = 16)]
    pub announced_recipient_x25519: outbe_primitives::storage::dsl::Map<Address, B256>,
}
