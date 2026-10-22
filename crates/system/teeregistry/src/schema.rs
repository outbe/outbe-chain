use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::TEE_REGISTRY_ADDRESS;

/// EVM storage layout for the TEE Registry.
///
/// Slots 0..=7 hold the block-1 bootstrap scalars.
/// Slots 8..=9 hold the founding DKG group public key.
/// Slots 10..=17 hold the active V1 policy and binding state.
/// Slots 18..=43 continue the registry state.
/// Slots 44..=58 hold strict-upgrade and upgrade-candidate state.
/// Fresh genesis uses this packed layout. New fields take the next `order`.
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

    /// slot 6: committee snapshot block that bootstrap read from.
    #[attribute(order = 6)]
    pub committee_snapshot_block: outbe_primitives::storage::dsl::Value<u64>,

    /// slot 7: committee snapshot hash that bootstrap was bound to.
    #[attribute(order = 7)]
    pub committee_snapshot_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 8: the founding committee's DKG group public key from the canonical
    /// bootstrap payload, chunked into 32-byte words. Empty until bootstrap.
    #[attribute(order = 8)]
    pub group_public_key: outbe_primitives::storage::dsl::Map<u32, B256>,

    /// slot 9: byte length of [`Self::group_public_key`] (0 when unset).
    #[attribute(order = 9)]
    pub group_public_key_len: outbe_primitives::storage::dsl::Value<u32>,

    /// slot 10: canonical byte length of the installed V1 active policy.
    #[attribute(order = 10)]
    pub active_v1_policy_len: outbe_primitives::storage::dsl::Value<u32>,

    /// slot 11: canonical V1 active-policy bytes, stored as 32-byte chunks.
    #[attribute(order = 11)]
    pub active_v1_policy_chunk: outbe_primitives::storage::dsl::Map<u32, B256>,

    /// slot 12: canonical NodeHost hash associated with a validator EVM address.
    #[attribute(order = 12)]
    pub validator_v1_node_hash: outbe_primitives::storage::dsl::Map<Address, B256>,

    /// slot 13: current enclave id by canonical node-id hash.
    #[attribute(order = 13)]
    pub v1_node_enclave_id: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 14: reverse one-to-one owner of an enclave id.
    #[attribute(order = 14)]
    pub v1_enclave_node_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 15: current binding id by node-id hash.
    #[attribute(order = 15)]
    pub v1_node_binding_id: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 16: reverse one-use owner of a binding id.
    #[attribute(order = 16)]
    pub v1_binding_node_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 17: exact canonical current registration-intent hash.
    #[attribute(order = 17)]
    pub v1_node_intent_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 18: policy hash that admitted the current binding.
    #[attribute(order = 18)]
    pub v1_node_policy_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 19: current binding version.
    #[attribute(order = 19)]
    pub v1_node_binding_version: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 20: current registration version.
    #[attribute(order = 20)]
    pub v1_node_registration_version: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 21: current renewal nonce.
    #[attribute(order = 21)]
    pub v1_node_renewal_nonce: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 22: current measurement-transition nonce.
    #[attribute(order = 22)]
    pub v1_node_transition_nonce: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 23: binding lease expiry as consensus Unix time.
    #[attribute(order = 23)]
    pub v1_node_valid_until: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 24: earliest authenticated collateral expiry returned by QVL.
    #[attribute(order = 24)]
    pub v1_node_collateral_valid_until: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 25: quote-bound recipient X25519 public key.
    #[attribute(order = 25)]
    pub v1_node_recipient_x25519: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 26: quote-bound Ed25519 attestation public key.
    #[attribute(order = 26)]
    pub v1_node_attestation_ed25519: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 27: quote-bound Noise responder X25519 public key.
    #[attribute(order = 27)]
    pub v1_node_noise_responder_x25519: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 28: QVL-authenticated MRENCLAVE.
    #[attribute(order = 28)]
    pub v1_node_mrenclave: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 29: QVL-authenticated MRSIGNER.
    #[attribute(order = 29)]
    pub v1_node_mrsigner: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 30: QVL-authenticated ISV product id.
    #[attribute(order = 30)]
    pub v1_node_isv_prod_id: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 31: QVL-authenticated ISV SVN.
    #[attribute(order = 31)]
    pub v1_node_isv_svn: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 32: admitted Platform TCB status discriminant.
    #[attribute(order = 32)]
    pub v1_node_platform_tcb_status: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 33: hash of the complete canonical stable QVL verdict.
    #[attribute(order = 33)]
    pub v1_node_verdict_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 34: hash of the exact canonical evidence admitted for current replay.
    #[attribute(order = 34)]
    pub v1_node_evidence_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 35: hash of the installed canonical V1 active policy.
    #[attribute(order = 35)]
    pub active_v1_policy_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 36: consensus timestamp at which the current lease began.
    #[attribute(order = 36)]
    pub v1_node_lease_started_at: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// slot 37: persistent single-NodeHost authorization committed by the quote.
    #[attribute(order = 37)]
    pub v1_node_host_authorization_hash: outbe_primitives::storage::dsl::Map<B256, B256>,

    /// slot 38: canonical byte length of the one staged successor V1 policy.
    #[attribute(order = 38)]
    pub staged_v1_policy_len: outbe_primitives::storage::dsl::Value<u32>,

    /// slot 39: canonical staged successor policy bytes as 32-byte chunks.
    #[attribute(order = 39)]
    pub staged_v1_policy_chunk: outbe_primitives::storage::dsl::Map<u32, B256>,

    /// slot 40: hash of the staged canonical successor policy.
    #[attribute(order = 40)]
    pub staged_v1_policy_hash: outbe_primitives::storage::dsl::Value<B256>,

    /// slot 41: Update vote proposal that exclusively owns the staged policy.
    #[attribute(order = 41)]
    pub staged_v1_policy_proposal_id: outbe_primitives::storage::dsl::Value<U256>,

    /// slot 42: redundant activation anchor authenticated against policy bytes.
    #[attribute(order = 42)]
    pub staged_v1_policy_activation_height: outbe_primitives::storage::dsl::Value<u64>,

    /// slot 43: Update proposal that most recently promoted the active policy.
    #[attribute(order = 43)]
    pub active_v1_policy_proposal_id: outbe_primitives::storage::dsl::Value<U256>,

    /// Hard-retirement authority. Zero proposal preserves historical V1 semantics.
    #[attribute(order = 44)]
    pub strict_upgrade_proposal: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 45)]
    pub strict_upgrade_height: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 46)]
    pub strict_upgrade_mrenclave: outbe_primitives::storage::dsl::Value<B256>,
    #[attribute(order = 47)]
    pub strict_upgrade_successor: outbe_primitives::storage::dsl::Value<B256>,
    #[attribute(order = 48)]
    pub strict_upgrade_predecessor: outbe_primitives::storage::dsl::Value<B256>,
    /// Replay guard for each proposal/validator, independent of jail/unjail state.
    #[attribute(order = 49)]
    pub strict_upgrade_penalized: outbe_primitives::storage::dsl::Map<B256, bool>,
    #[attribute(order = 50)]
    pub strict_upgrade_swept: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 51)]
    pub last_enclave_retirement_height: outbe_primitives::storage::dsl::Value<u64>,
    /// Finalized authorization to deliver the permanent key to a pending candidate.
    #[attribute(order = 52)]
    pub upgrade_candidate_context: outbe_primitives::storage::dsl::Map<B256, B256>,
    #[attribute(order = 53)]
    pub upgrade_candidate_expiry: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 54)]
    pub upgrade_candidate_source: outbe_primitives::storage::dsl::Map<B256, B256>,
    #[attribute(order = 55)]
    pub upgrade_candidate_target: outbe_primitives::storage::dsl::Map<B256, B256>,
    /// Monotonic prepare nonce, retained after cancellation and promotion.
    #[attribute(order = 56)]
    pub upgrade_candidate_nonce: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 57)]
    pub upgrade_candidate_evidence: outbe_primitives::storage::dsl::Map<B256, B256>,
    /// A prepared binding can never fall back to the legacy copy-root transition.
    #[attribute(order = 58)]
    pub upgrade_binding_requires_candidate: outbe_primitives::storage::dsl::Map<B256, bool>,
}
