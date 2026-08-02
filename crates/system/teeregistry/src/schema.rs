use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::TEE_REGISTRY_ADDRESS;

/// EVM storage layout for the TEE Registry.
///
/// Global scalars (slots 0..=8) hold the block-1 OST3 bootstrap result that clients
/// and verifiers read. Bootstrap committee projections occupy slots 9..=18. Active
/// V1 policy and leased-binding state occupies slots 19..=53. The layout is
/// append-only; new fields take the next `order`.
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

    /// slot 17: the active committee's DKG group public key (constant term),
    /// chunked into 32-byte words. The verification key for this committee's
    /// threshold group signatures — used to verify a prior-committee reshare
    /// endorsement before applying `DkgBoundaryArtifact::tee_reshare_registrations`.
    /// Written at bootstrap; updated on reshare activation. Empty (len 0) until set.
    #[attribute(order = 17)]
    pub group_public_key: outbe_primitives::storage::dsl::Map<u32, B256>,

    /// slot 18: byte length of [`Self::group_public_key`] (0 when unset).
    #[attribute(order = 18)]
    pub group_public_key_len: outbe_primitives::storage::dsl::Value<u32>,

    /// slot 19: canonical byte length of the installed V1 active policy.
    #[attribute(order = 19)]
    pub active_v1_policy_len: outbe_primitives::storage::dsl::Value<u32>,

    /// slot 20: canonical V1 active-policy bytes, stored as 32-byte chunks.
    #[attribute(order = 20)]
    pub active_v1_policy_chunk: outbe_primitives::storage::dsl::Map<u32, B256>,

    /// slot 21: canonical validator node-id hash by validator EVM address.
    #[attribute(order = 21)]
    pub validator_v1_node_hash: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 22: current enclave id by canonical node-id hash.
    #[attribute(order = 22)]
    pub v1_node_enclave_id: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 23: reverse one-to-one owner of an enclave id.
    #[attribute(order = 23)]
    pub v1_enclave_node_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 24: current binding id by node-id hash.
    #[attribute(order = 24)]
    pub v1_node_binding_id: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 25: reverse one-use owner of a binding id.
    #[attribute(order = 25)]
    pub v1_binding_node_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 26: exact canonical current registration-intent hash.
    #[attribute(order = 26)]
    pub v1_node_intent_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 27: policy hash that admitted the current binding.
    #[attribute(order = 27)]
    pub v1_node_policy_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 28: enclave profile discriminant.
    #[attribute(order = 28)]
    pub v1_node_profile: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 29: current binding version.
    #[attribute(order = 29)]
    pub v1_node_binding_version: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 30: current registration version.
    #[attribute(order = 30)]
    pub v1_node_registration_version: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 31: current renewal nonce.
    #[attribute(order = 31)]
    pub v1_node_renewal_nonce: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 32: current measurement-transition nonce.
    #[attribute(order = 32)]
    pub v1_node_transition_nonce: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 33: binding lease expiry as consensus Unix time.
    #[attribute(order = 33)]
    pub v1_node_valid_until: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 34: earliest authenticated collateral expiry returned by QVL.
    #[attribute(order = 34)]
    pub v1_node_collateral_valid_until: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 35: quote-bound recipient X25519 public key.
    #[attribute(order = 35)]
    pub v1_node_recipient_x25519: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 36: quote-bound Ed25519 attestation public key.
    #[attribute(order = 36)]
    pub v1_node_attestation_ed25519: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 37: quote-bound Noise responder X25519 public key.
    #[attribute(order = 37)]
    pub v1_node_noise_responder_x25519: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 38: QVL-authenticated MRENCLAVE.
    #[attribute(order = 38)]
    pub v1_node_mrenclave: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 39: QVL-authenticated MRSIGNER.
    #[attribute(order = 39)]
    pub v1_node_mrsigner: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 40: QVL-authenticated ISV product id.
    #[attribute(order = 40)]
    pub v1_node_isv_prod_id: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 41: QVL-authenticated ISV SVN.
    #[attribute(order = 41)]
    pub v1_node_isv_svn: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 42: admitted Platform TCB status discriminant.
    #[attribute(order = 42)]
    pub v1_node_platform_tcb_status: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 43: hash of the complete canonical stable QVL verdict.
    #[attribute(order = 43)]
    pub v1_node_verdict_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 44: hash of the exact canonical evidence admitted for current replay.
    #[attribute(order = 44)]
    pub v1_node_evidence_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 45: hash of the installed canonical V1 active policy.
    #[attribute(order = 45)]
    pub active_v1_policy_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 46: consensus timestamp at which the current lease began.
    #[attribute(order = 46)]
    pub v1_node_lease_started_at: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 47: persistent single-NodeHost authorization committed by the quote.
    #[attribute(order = 47)]
    pub v1_node_host_authorization_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 48: canonical byte length of the one staged successor V1 policy.
    #[attribute(order = 48)]
    pub staged_v1_policy_len: outbe_primitives::storage::dsl::Value<u32>,

    /// slot 49: canonical staged successor policy bytes as 32-byte chunks.
    #[attribute(order = 49)]
    pub staged_v1_policy_chunk: outbe_primitives::storage::dsl::Map<u32, B256>,

    /// slot 50: hash of the staged canonical successor policy.
    #[attribute(order = 50)]
    pub staged_v1_policy_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 51: Update vote proposal that exclusively owns the staged policy.
    #[attribute(order = 51)]
    pub staged_v1_policy_proposal_id: outbe_primitives::storage::dsl::Value<U256>,

    /// slot 52: redundant activation anchor authenticated against policy bytes.
    #[attribute(order = 52)]
    pub staged_v1_policy_activation_height: outbe_primitives::storage::dsl::Value<u64>,

    /// slot 53: Update proposal that most recently promoted the active policy.
    #[attribute(order = 53)]
    pub active_v1_policy_proposal_id: outbe_primitives::storage::dsl::Value<U256>,
}
