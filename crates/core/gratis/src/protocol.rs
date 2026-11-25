//! Gratis and Promis-mine protocol types, canonical hashes, and attestation preimages.
//!
//! Field order matches the historical enclave postcard and JSON. [`GratisOp`] is
//! `repr(u8)` so the modify-auth tag and the canonical-hash byte stay frozen.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_core_common::confidential_tee::{derive_account_keys_message, Ledger, OperationTag};
use outbe_fidelity::{FidelityOpOutcome, FidelityOpSection};

/// A Gratis write the enclave applies over encrypted per-account state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u8)]
pub enum GratisOp {
    /// Credit the liquid balance.
    Mint = 0,
    /// Debit the liquid balance.
    Burn = 1,
    /// Move the authorized amount from liquid to pledged.
    Pledge = 2,
    /// Move collateral from pledged back to liquid.
    ReleasePledged = 3,
    /// Burn collateral from the pledged balance.
    BurnPledged = 4,
    /// Open a pledge authorization sealed to the offer key and check its MAC.
    OpenSealedPledge = 5,
    /// Move a runtime-authorized amount from liquid to pledged.
    PledgeCollateral = 6,
}

impl OperationTag for GratisOp {
    fn tag(self) -> u8 {
        self as u8
    }
}

/// Proof that the caller holds the account's modify key, without revealing it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModifyAuth {
    /// MAC over the account, operation, amount, nonce, and chain.
    pub mac: [u8; 32],
    /// Replay counter bound by [`Self::mac`].
    pub op_nonce: u64,
}

/// A main-account pledge authorization sealed to the offer key, for one reservation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SealedPledge {
    /// Ephemeral X25519 key, AEAD nonce and ciphertext of the account and its MAC.
    pub sealed: Vec<u8>,
    /// Credis reservation the MAC binds.
    pub credis_reservation_id: U256,
}

/// Stateless transition over an account's liquid and pledged balance blobs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GratisOpRequest {
    /// Liquid, pledged, or collateral operation.
    pub op: GratisOp,
    /// Chain bound into the modify authorization.
    pub chain_id: B256,
    /// Account whose blobs change.
    pub account: Address,
    /// Amount applied to the selected blobs.
    pub amount: U256,
    /// Current encrypted liquid balance. Empty is zero.
    pub current_balance: Vec<u8>,
    /// Current encrypted pledged balance. Empty is zero.
    pub current_pledged: Vec<u8>,
    /// Owner authorization. Collateral ops carry a zero MAC.
    pub modify_auth: ModifyAuth,
    /// Co-located Fidelity section. Collateral ops must leave this empty.
    pub fidelity: Option<FidelityOpSection>,
    /// Sealed pledge of [`GratisOp::OpenSealedPledge`]. Empty for every other op.
    pub sealed_pledge: Option<SealedPledge>,
}

/// Outcome of one Gratis op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GratisOpStatus {
    /// The blobs in the result replace the account.
    Applied,
    /// Business rejection. The host reverts and writes nothing.
    Rejected {
        /// Display sentence stored as consensus output.
        reason: String,
    },
}

/// Public result of an `ApplyGratisOp`. Tag [`crate::witness`] body for tag 1.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GratisOpResult {
    /// Applied or rejected. A rejection still carries the inputs hash.
    pub status: GratisOpStatus,
    /// Replacement liquid blob. Empty when that field was not written.
    pub new_balance: Vec<u8>,
    /// Replacement pledged blob. Empty when that field was not written.
    pub new_pledged: Vec<u8>,
    /// Plaintext amount echoed for the caller. Not a wire secret by itself.
    pub event_amount: U256,
    /// Next owner nonce. Zero when the op did not consume a nonce.
    pub next_op_nonce: u64,
    /// Fidelity receipt when a section was applied.
    pub fidelity: Option<FidelityOpOutcome>,
    /// Keccak of the canonical request.
    pub inputs_canonical_hash: B256,
    /// Ed25519 tag. Cleared inside the witness payload.
    pub attestation_tag: Vec<u8>,
    /// Owner-encrypted event amount for mint, burn, and pledged burn.
    pub encrypted_event_amount: Vec<u8>,
    /// Main account an opened sealed pledge names. Empty for every other op.
    pub pledge_account: Option<Address>,
}

fn push_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(bytes);
}

/// Keccak of the canonical Gratis request framing.
#[must_use]
pub fn gratis_op_canonical_hash(req: &GratisOpRequest) -> B256 {
    let mut buf = Vec::new();
    buf.push(req.op as u8);
    buf.extend_from_slice(req.chain_id.as_slice());
    buf.extend_from_slice(req.account.as_slice());
    buf.extend_from_slice(&req.amount.to_be_bytes::<32>());
    push_bytes(&mut buf, &req.current_balance);
    push_bytes(&mut buf, &req.current_pledged);
    buf.extend_from_slice(&req.modify_auth.mac);
    buf.extend_from_slice(&req.modify_auth.op_nonce.to_be_bytes());
    match &req.fidelity {
        Some(section) => {
            buf.push(1);
            buf.push(section.op as u8);
            buf.extend_from_slice(&section.timestamp.to_be_bytes());
            buf.extend_from_slice(&section.first_qualified_start.to_be_bytes());
            push_bytes(&mut buf, &section.current_blob);
        }
        None => buf.push(0),
    }
    match &req.sealed_pledge {
        Some(pledge) => {
            buf.push(1);
            push_bytes(&mut buf, &pledge.sealed);
            buf.extend_from_slice(&pledge.credis_reservation_id.to_be_bytes::<32>());
        }
        None => buf.push(0),
    }
    keccak256(buf)
}

/// Domain-separated preimage signed for a Gratis op. Tag is cleared first.
pub fn gratis_op_attestation_preimage(
    inputs_canonical_hash: B256,
    result: &GratisOpResult,
) -> Result<Vec<u8>, serde_json::Error> {
    json_preimage(
        b"outbe/tee/gratis-attestation/v3",
        inputs_canonical_hash,
        &cleared_gratis(result),
    )
}

/// Personal-sign message for `outbe_deriveGratisKeys`.
#[must_use]
pub fn derive_gratis_keys_message(account: Address, ephemeral_pubkey: B256) -> Vec<u8> {
    derive_account_keys_message(Ledger::Gratis, account, ephemeral_pubkey)
}

fn cleared_gratis(result: &GratisOpResult) -> GratisOpResult {
    let mut probe = result.clone();
    probe.attestation_tag.clear();
    probe
}

fn json_preimage<T: serde::Serialize>(
    tag: &[u8],
    inputs_canonical_hash: B256,
    value: &T,
) -> Result<Vec<u8>, serde_json::Error> {
    let json = serde_json::to_vec(value)?;
    let mut buf = Vec::with_capacity(tag.len() + 32 + 4 + json.len());
    buf.extend_from_slice(tag);
    buf.extend_from_slice(inputs_canonical_hash.as_slice());
    buf.extend_from_slice(&(json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}
