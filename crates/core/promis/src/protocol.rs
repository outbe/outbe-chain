//! Promis protocol types, canonical hashes, and attestation preimages.
//!
//! Field order matches the historical enclave postcard and JSON. [`PromisOp`] is
//! `repr(u8)` so the modify-auth tag and the canonical-hash byte stay frozen.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_core_common::confidential_tee::{derive_account_keys_message, Ledger, OperationTag};

pub use outbe_gratis::ModifyAuth;

/// A Promis write the enclave applies over an encrypted per-account balance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum PromisOp {
    /// Credit the balance.
    Mint = 0,
    /// Debit the balance.
    Burn = 1,
}

impl OperationTag for PromisOp {
    fn tag(self) -> u8 {
        self as u8
    }
}

/// Inputs for one `ApplyPromisOp`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromisOpRequest {
    /// Mint or burn.
    pub op: PromisOp,
    /// Chain bound into the modify authorization.
    pub chain_id: B256,
    /// Account whose balance changes.
    pub account: Address,
    /// Amount applied to the balance.
    pub amount: U256,
    /// Current encrypted balance. Empty is zero.
    pub current_balance: Vec<u8>,
    /// Owner authorization.
    pub modify_auth: ModifyAuth,
}

/// Outcome of one Promis op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PromisOpStatus {
    /// The balance blob in the result replaces the account.
    Applied,
    /// Business rejection. The host reverts and writes nothing.
    Rejected {
        /// Display sentence stored as consensus output.
        reason: String,
    },
}

/// Public result of an `ApplyPromisOp`. Tag 2 body.
///
/// `encrypted_event_amount` stays last. That order is the frozen JSON preimage.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PromisOpResult {
    /// Applied or rejected. A rejection still carries the inputs hash.
    pub status: PromisOpStatus,
    /// Replacement balance blob. Empty when the op was rejected.
    pub new_balance: Vec<u8>,
    /// Plaintext amount echoed for the caller.
    pub event_amount: U256,
    /// Next owner nonce. Zero when the op did not consume a nonce.
    pub next_op_nonce: u64,
    /// Keccak of the canonical request.
    pub inputs_canonical_hash: B256,
    /// Ed25519 tag. Cleared inside the witness payload.
    pub attestation_tag: Vec<u8>,
    /// Owner-encrypted event amount.
    pub encrypted_event_amount: Vec<u8>,
}

/// Keccak of the canonical Promis request framing.
#[must_use]
pub fn promis_op_canonical_hash(req: &PromisOpRequest) -> B256 {
    let mut buf = Vec::new();
    buf.push(req.op as u8);
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&req.amount.to_be_bytes::<32>());
    buf.extend_from_slice(&(req.current_balance.len() as u32).to_be_bytes());
    buf.extend_from_slice(&req.current_balance);
    buf.extend_from_slice(&req.modify_auth.mac);
    buf.extend_from_slice(&req.modify_auth.op_nonce.to_be_bytes());
    keccak256(buf)
}

/// Domain-separated preimage signed for a Promis op. The tag is cleared first.
pub fn promis_op_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &PromisOpResult,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut probe = result.clone();
    probe.attestation_tag.clear();
    let json = serde_json::to_vec(&probe)?;
    let tag = b"outbe/tee/promis-attestation/v1";
    let mut buf = Vec::with_capacity(tag.len() + 32 + 4 + json.len());
    buf.extend_from_slice(tag);
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}

/// Personal-sign message for `outbe_derivePromisKeys`.
#[must_use]
pub fn derive_promis_keys_message(account: Address, ephemeral_pubkey: B256) -> Vec<u8> {
    derive_account_keys_message(Ledger::Promis, account, ephemeral_pubkey)
}
