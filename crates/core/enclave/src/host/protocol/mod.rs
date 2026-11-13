//! Domain wire-protocol types for confidential Outbe ledgers and computations.
//!
//! These types ride inside [`outbe_tee::protocol::EnclaveRequest::Payload`] and
//! [`outbe_tee::protocol::EnclaveResponse::Payload`] as canonical tag-free
//! `postcard` bytes, keeping `outbe-tee` completely domain-neutral.

use alloy_primitives::{Address, B256, U256};

pub use outbe_primitives::time::WorldwideDay;
pub use outbe_tee::protocol::eip191_hash;

mod enclave_request;
mod enclave_response;

pub use enclave_request::EnclaveRequest;
pub use enclave_response::{
    derive_account_keys_message, derive_gratis_keys_message, fidelity_cohort_attestation_preimage,
    fidelity_cohort_canonical_hash, fidelity_query_attestation_preimage,
    fidelity_query_auth_message, fidelity_query_canonical_hash,
    fidelity_snapshot_attestation_preimage, fidelity_snapshot_canonical_hash,
    gratis_op_attestation_preimage, gratis_op_canonical_hash, inputs_canonical_hash,
    mine_gratis_from_promis_attestation_preimage, mine_gratis_from_promis_canonical_hash,
    promis_op_attestation_preimage, promis_op_canonical_hash, tribute_offer_attestation_preimage,
    EnclaveResponse,
};

/// A single offer handed to the enclave.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EncryptedTributeOffer {
    /// L1 `msg.sender` that owns the resulting Tribute (public, on-chain).
    pub owner: Address,
    /// ABI `cipherText`: AEAD ciphertext of the offer payload.
    pub cipher_text: Vec<u8>,
    /// ABI `nonce`: 12-byte ChaCha20Poly1305 nonce.
    pub nonce: Vec<u8>,
    /// ABI `ephemeralPubkey` (uint256): client ephemeral X25519 public key for ECDHE, big-endian.
    pub ephemeral_pubkey: U256,
    /// ABI `worldwideDay`: UTC+14 day key (`YYYYMMDD`).
    pub worldwide_day: WorldwideDay,
    /// ABI `tributeCurrency`: ISO 4217 code the tribute amount is denominated in.
    pub tribute_currency: u16,
    /// ABI `referenceCurrency`: ISO 4217 reference currency code.
    pub reference_currency: u16,
    /// Exact WorldwideDay VWAP for `COEN/<tribute_currency>`, scale 1e6.
    pub issuance_wwd_vwap_minor: U256,
    /// Exact WorldwideDay VWAP for `COEN/<reference_currency>`, scale 1e6.
    pub reference_wwd_vwap_minor: U256,
    /// Continuous reference-currency S-curve value, scale 1e6.
    pub reference_scurve_minor: U256,
    /// Public ZK claim context supplied for every admitted offer.
    #[serde(default)]
    pub zk_context: Option<TributeZkContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TributeZkContext {
    pub derived_owner: B256,
    pub chain_id: u64,
    pub l2_chain_id: u64,
}

/// Shared four-word public claim for Demo Tribute and Niflheim Tribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TributePublicInputs {
    pub derived_owner: B256,
    pub nft_hash: B256,
    pub binding_hash: B256,
    pub merkle_root: B256,
}

impl TributePublicInputs {
    /// Map ABI-ordered words after the caller validates canonical field encoding.
    #[must_use]
    pub fn from_raw_parts(
        [derived_owner, nft_hash, binding_hash, merkle_root]: [[u8; 32]; 4],
    ) -> Self {
        Self {
            derived_owner: B256::from(derived_owner),
            nft_hash: B256::from(nft_hash),
            binding_hash: B256::from(binding_hash),
            merkle_root: B256::from(merkle_root),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TributeZkExpectedHashes {
    pub nft_hash: B256,
    pub binding_hash: B256,
}

/// Status of a single offer after enclave processing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TributeOfferStatus {
    Created,
    Rejected { reason: String },
}

/// Public result for a single offer.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TributeOfferResult {
    /// Poseidon(token_id preimage), computed in-enclave from sensitive data.
    pub token_id: B256,
    /// L1 `msg.sender` (public, on-chain).
    pub owner: Address,
    pub issuance_amount_minor: U256,
    pub nominal_amount_minor: U256,
    /// `max(reference_wwd_vwap_minor, reference_scurve_minor)`.
    pub effective_reference_price_minor: U256,
    /// SU hashes (hex).
    pub su_hashes: Vec<String>,
    /// WAA wallet addresses.
    pub wallet_addresses: Vec<String>,
    /// SRA addresses.
    pub sra_addresses: Vec<String>,
    /// Expected public hashes recomputed over the decrypted TributeDraft.
    #[serde(default)]
    pub zk_expected_hashes: Option<TributeZkExpectedHashes>,
    pub status: TributeOfferStatus,
}

/// A Gratis write operation the enclave applies over encrypted per-account state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GratisOp {
    Mint,
    Burn,
    /// Move the authorized amount from the liquid to the pledged balance.
    Pledge,
    /// Move collateral from the pledged back to the liquid balance.
    ReleasePledged,
    /// Burn collateral from the pledged balance.
    BurnPledged,
}

/// Proof that the caller holds the account's modify key, without revealing it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModifyAuth {
    pub mac: [u8; 32],
    pub op_nonce: u64,
}

/// Stateless transition over an account's liquid and pledged balance blobs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GratisOpRequest {
    pub op: GratisOp,
    pub chain_id: B256,
    pub account: Address,
    pub amount: U256,
    pub current_balance: Vec<u8>,
    pub current_pledged: Vec<u8>,
    pub modify_auth: ModifyAuth,
    pub fidelity: Option<FidelityOpSection>,
}

/// The Fidelity cohort mutation carried inside a Gratis op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FidelityCohortOp {
    /// Acquisition: push a new active cohort of the Gratis op's `amount`.
    In,
    /// Sale: consume active cohorts LIFO for the Gratis op's `amount`.
    Out,
    /// Read-only league probe (no cohort mutation, no blob rewrite).
    Probe,
}

/// Co-located Fidelity input riding in a [`GratisOpRequest`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityOpSection {
    pub op: FidelityCohortOp,
    pub timestamp: u64,
    pub first_qualified_start: u64,
    pub current_blob: Vec<u8>,
}

/// Plaintext receipt of a [`FidelityOpSection`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityOpOutcome {
    pub new_blob: Vec<u8>,
    pub qualified_start_initialized: Option<u64>,
    pub league: u16,
}

/// Inputs for a standalone `ApplyFidelityCohortOp`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityCohortRequest {
    pub chain_id: B256,
    pub account: Address,
    pub amount: U256,
    pub section: FidelityOpSection,
}

/// Public result of an `ApplyFidelityCohortOp`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityCohortResult {
    pub outcome: FidelityOpOutcome,
    pub inputs_canonical_hash: B256,
    pub attestation_tag: Vec<u8>,
}

/// Outcome of a single Gratis op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GratisOpStatus {
    Applied,
    Rejected { reason: String },
}

/// Public result of an `ApplyGratisOp`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GratisOpResult {
    pub status: GratisOpStatus,
    pub new_balance: Vec<u8>,
    pub new_pledged: Vec<u8>,
    pub event_amount: U256,
    pub next_op_nonce: u64,
    pub fidelity: Option<FidelityOpOutcome>,
    pub inputs_canonical_hash: B256,
    pub attestation_tag: Vec<u8>,
    pub encrypted_event_amount: Vec<u8>,
}

/// The confidential ledger that a key-derivation / op request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Ledger {
    Gratis,
    Promis,
    Fidelity,
    Tribute,
    Nod,
}

/// A Promis write operation the enclave applies over encrypted per-account state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PromisOp {
    Mint,
    Burn,
}

/// Inputs for a single `ApplyPromisOp`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromisOpRequest {
    pub op: PromisOp,
    pub chain_id: B256,
    pub account: Address,
    pub amount: U256,
    pub current_balance: Vec<u8>,
    pub modify_auth: ModifyAuth,
}

/// Outcome of a single Promis op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PromisOpStatus {
    Applied,
    Rejected { reason: String },
}

/// Public result of an `ApplyPromisOp`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromisOpResult {
    pub status: PromisOpStatus,
    pub new_balance: Vec<u8>,
    pub event_amount: U256,
    pub next_op_nonce: u64,
    pub inputs_canonical_hash: B256,
    pub attestation_tag: Vec<u8>,
    pub encrypted_event_amount: Vec<u8>,
}

/// Paired Promis-burn + Gratis-mint + Fidelity-cohort-in request (`IPromisFactory.mineGratis`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MineGratisFromPromisRequestV1 {
    pub chain_id: B256,
    pub account: Address,
    pub encrypted_promis_amount: Vec<u8>,
    pub current_promis_balance: Vec<u8>,
    pub promis_modify_auth: ModifyAuth,
    pub current_gratis_balance: Vec<u8>,
    pub gratis_modify_auth: ModifyAuth,
    pub fidelity: FidelityOpSection,
}

/// Outcome of a [`MineGratisFromPromisRequestV1`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MineGratisFromPromisStatusV1 {
    Applied,
    PromisRejected { reason: String },
    GratisRejected { reason: String },
}

/// Public result of [`MineGratisFromPromisRequestV1`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MineGratisFromPromisResultV1 {
    pub status: MineGratisFromPromisStatusV1,
    pub new_promis_balance: Vec<u8>,
    pub encrypted_promis_event_amount: Vec<u8>,
    pub next_promis_op_nonce: u64,
    pub new_gratis_balance: Vec<u8>,
    pub encrypted_gratis_event_amount: Vec<u8>,
    pub next_gratis_op_nonce: u64,
    pub fidelity: Option<FidelityOpOutcome>,
    pub inputs_canonical_hash: B256,
    pub attestation_tag: Vec<u8>,
}

/// One owner's encrypted cohort blob in a [`FidelitySnapshotRequest`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelitySnapshotEntry {
    pub owner: Address,
    pub cohort_blob: Vec<u8>,
}

/// Inputs for a `SnapshotFidelityLeagues` batch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelitySnapshotRequest {
    pub timestamp: u64,
    pub first_qualified_start: u64,
    pub entries: Vec<FidelitySnapshotEntry>,
}

/// One owner's plaintext league and RCFI in a snapshot result, in request order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityLeagueEntry {
    pub owner: Address,
    pub league: u16,
    #[serde(default)]
    pub rcfi: U256,
}

/// Inputs for a `QueryFidelityIndex`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityQueryRequest {
    pub chain_id: B256,
    pub account: Address,
    pub cohort_blob: Vec<u8>,
    pub query_timestamp: u64,
    pub block_timestamp: u64,
    pub first_qualified_start: u64,
    pub expiry: u64,
    pub owner_sig: Vec<u8>,
}

/// Plaintext result of a `QueryFidelityIndex`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FidelityQueryResult {
    pub rcfi: U256,
    pub efficiency: U256,
    pub league: u16,
    pub inputs_canonical_hash: B256,
    pub attestation_tag: Vec<u8>,
}
