use alloy_primitives::{Address, B256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::TEE_REGISTRY_ADDRESS;

/// EVM storage layout for the TEE Registry.
///
/// Global scalars (slots 1..=9) hold the bootstrap result that clients and
/// verifiers read. Per-validator maps (slots 10..=17) hold each validator's TEE
/// registration bundle keyed by validator address. Slot 0 is the reserved
/// schema version; new fields take the next `order`.
#[storage_schema]
#[contract(addr = TEE_REGISTRY_ADDRESS)]
pub struct TeeRegistry {
    /// Slot 0: reserved storage schema version.
    #[attribute(order = 0)]
    pub _reserved_schema_version: outbe_primitives::storage::dsl::Value<u32>,

    /// slot 1: set true once `write_bootstrap` runs (idempotency gate).
    #[attribute(order = 1)]
    pub bootstrapped: outbe_primitives::storage::dsl::Value<bool>,

    /// slot 2: the tribute offer public key clients encrypt to.
    #[attribute(order = 2)]
    pub tribute_offer_public_key: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 3: hash of the genesis TEE policy (mrsigner/mrenclave/min_isv_svn).
    #[attribute(order = 3)]
    pub policy_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 4: key epoch.
    #[attribute(order = 4)]
    pub key_epoch: outbe_primitives::storage::dsl::Value<u64>,

    /// slot 5: tribute-offer-key epoch (HKDF domain separation; reshare-rotation hook).
    #[attribute(order = 5)]
    pub tribute_offer_epoch: outbe_primitives::storage::dsl::Value<u64>,

    /// slot 6: DKG transcript hash.
    #[attribute(order = 6)]
    pub dkg_transcript_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 7: committee snapshot block bootstrap read from.
    #[attribute(order = 7)]
    pub committee_snapshot_block: outbe_primitives::storage::dsl::Value<u64>,

    /// slot 8: committee snapshot hash bootstrap was bound to.
    #[attribute(order = 8)]
    pub committee_snapshot_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 9: number of registered validators.
    #[attribute(order = 9)]
    pub registered_count: outbe_primitives::storage::dsl::Value<u32>,

    /// slot 10: recipient X25519 pubkey per validator.
    #[attribute(order = 10)]
    pub recipient_x25519: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 11: attestation pubkey per validator.
    #[attribute(order = 11)]
    pub attestation_pub: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 12: Noise static pubkey per validator.
    #[attribute(order = 12)]
    pub noise_static_pub: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 13: MRENCLAVE per validator.
    #[attribute(order = 13)]
    pub mrenclave: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 14: MRSIGNER per validator.
    #[attribute(order = 14)]
    pub mrsigner: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 15: ISV SVN per validator.
    #[attribute(order = 15)]
    pub isv_svn: outbe_primitives::storage::dsl::Map<Address, u64>,

    /// slot 16: keys hash (commitment over the validator's TEE keys) per validator.
    #[attribute(order = 16)]
    pub keys_hash: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 17: recipient X25519 pubkey announced via `BoundaryOutcome`
    /// (`DkgBoundaryArtifact::tee_recipient_pubkeys`), per validator. Distinct
    /// from slot 10 (`recipient_x25519`), the authoritative key written by the
    /// full `TeeBootstrap` registration: this is the boundary-channel
    /// announcement (key rotation / pre-bootstrap delivery), recorded
    /// independently of a full registration bundle.
    #[attribute(order = 17)]
    pub announced_recipient_x25519: outbe_primitives::storage::dsl::Map<Address, B256>,
}
