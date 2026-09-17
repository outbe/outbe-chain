//! Versioned PledgeNote wire format. Owner identities occur only inside envelopes.

use alloy_primitives::{keccak256, Address, B256, U256};
use serde::{Deserialize, Serialize};

use crate::protocol::{FidelityCohortOp, GratisOp, ModifyAuth};

pub const SCHEMA_VERSION: u8 = 1;
/// Fifteen 4144-byte records plus framing fit the 64 KiB Noise transport.
pub const REPLAY_BATCH_ENTRIES: usize = 15;
pub const SNAPSHOT_BATCH_OWNERS: usize = 512;
pub const QUOTE_TTL_SECONDS: u64 = 900;
pub const LIVE_STATE_BUDGET: usize = 256 * 1024 * 1024;
pub const JOURNAL_PLAINTEXT_BYTES: usize = 4096;
pub const ENVELOPE_PLAINTEXT_BYTES: usize = 2048;
pub const ENVELOPE_DOMAIN: &[u8] = b"outbe/pledgenote/envelope/v1";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub sequence: u64,
    pub root: B256,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    pub chain_id: B256,
    pub genesis_hash: B256,
    pub block_number: u64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terms {
    pub asset: Address,
    pub principal_minor: U256,
    pub gratis_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub entry_price_minor: U256,
    pub created_at: u64,
    pub valid_until: u64,
}

/// The caller-visible quote input. It is repeated inside the encrypted, MAC-bound
/// owner request so a relayer cannot change currency, principal or slippage cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    pub asset: Address,
    pub principal_minor: U256,
    pub max_gratis_minor: U256,
    pub reference_currency: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateRequest {
    pub quote: Quote,
    pub envelope: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerAction {
    Create(Quote),
    Cancel { note_id: B256 },
    Query,
    QueryAt { timestamp: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrivateRequest {
    Owner {
        chain_id: B256,
        account: Address,
        nonce: u64,
        action: OwnerAction,
        mac: B256,
    },
    Use {
        chain_id: B256,
        note_id: B256,
        owner_sa: Address,
        authorization: B256,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    Create {
        quote: Quote,
        terms: Terms,
        envelope: Vec<u8>,
    },
    Cancel {
        envelope: Vec<u8>,
    },
    Use {
        owner_sa: Address,
        envelope: Vec<u8>,
    },
    Release {
        collateral_handle: B256,
        amount: U256,
    },
    Forfeit {
        collateral_handle: B256,
        amount: U256,
    },
    Gratis {
        account: Address,
        amount: U256,
        op: GratisOp,
        auth: ModifyAuth,
        fidelity: bool,
    },
    Cohort {
        account: Address,
        amount: U256,
        op: FidelityCohortOp,
        timestamp: u64,
    },
    Query {
        envelope: Vec<u8>,
    },
    Snapshot {
        owners: Vec<Address>,
        timestamp: u64,
    },
}

impl Command {
    pub const fn is_read_only(&self) -> bool {
        matches!(self, Self::Query { .. } | Self::Snapshot { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub schema: u8,
    pub parent: Head,
    pub context: Context,
    pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayRequest {
    pub chain_id: B256,
    pub parent: Head,
    pub reset: bool,
    pub entries: Vec<Vec<u8>>,
}

pub fn replay_hash(request: &ReplayRequest) -> Result<B256, String> {
    let mut bytes = b"outbe/pledgenote/replay/v1".to_vec();
    bytes.extend(encode(request)?);
    Ok(keccak256(bytes))
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub head: Head,
    pub journal_entry: Vec<u8>,
    pub encrypted_receipt: Vec<u8>,
    pub reservation_id: B256,
    pub collateral_handle: B256,
    pub credis_id: B256,
    pub terms: Option<Terms>,
    pub amount: U256,
    pub total_supply: U256,
    pub pledged_supply: U256,
    pub first_qualified_start: u64,
    pub leagues: Vec<(Address, u16)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    Applied(Box<Outcome>),
    Rejected(String),
    NeedsReplay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub inputs_hash: B256,
    pub reply: Reply,
    pub attestation: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub note_id: B256,
    pub secret: B256,
    pub terms: Option<Terms>,
    pub balance: U256,
    pub pledged: U256,
    pub next_nonce: u64,
    pub rcfi: U256,
    pub efficiency: U256,
    pub league: u16,
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    postcard::to_allocvec(value).map_err(|e| format!("pledgenote encoding: {e}"))
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, String> {
    postcard::from_bytes(bytes).map_err(|e| format!("pledgenote decoding: {e}"))
}

pub fn request_hash(request: &Request) -> Result<B256, String> {
    let mut bytes = b"outbe/pledgenote/request/v1".to_vec();
    bytes.extend(encode(request)?);
    Ok(keccak256(bytes))
}

pub fn attestation_preimage(hash: B256, reply: &Reply) -> Result<Vec<u8>, String> {
    let mut bytes = b"outbe/pledgenote/attestation/v1".to_vec();
    bytes.extend_from_slice(hash.as_slice());
    bytes.extend(encode(reply)?);
    Ok(bytes)
}

pub fn owner_mac(
    key: &[u8; 32],
    chain: B256,
    account: Address,
    nonce: u64,
    action: &OwnerAction,
) -> Result<B256, String> {
    let mut bytes = b"outbe/pledgenote/owner/v1".to_vec();
    bytes.extend(encode(&(chain, account, nonce, action))?);
    Ok(B256::from_slice(
        ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), &bytes).as_ref(),
    ))
}

pub fn use_mac(
    secret: B256,
    chain: B256,
    note_id: B256,
    owner_sa: Address,
) -> Result<B256, String> {
    let mut bytes = b"outbe/pledgenote/use/v1".to_vec();
    bytes.extend(encode(&(chain, note_id, owner_sa))?);
    Ok(B256::from_slice(
        ring::hmac::sign(
            &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_slice()),
            &bytes,
        )
        .as_ref(),
    ))
}

/// Fixed-size plaintext padding makes journal and owner messages independent of
/// the selected account's state size. The u32 length is checked before encoding.
pub fn pad(bytes: &[u8], size: usize) -> Result<Vec<u8>, String> {
    let len = u32::try_from(bytes.len()).map_err(|_| "pledgenote message too large")?;
    if bytes.len().checked_add(4).is_none_or(|n| n > size) {
        return Err("pledgenote message too large".into());
    }
    let mut padded = Vec::with_capacity(size);
    padded.extend_from_slice(&len.to_be_bytes());
    padded.extend_from_slice(bytes);
    padded.resize(size, 0);
    Ok(padded)
}

pub fn unpad(bytes: &[u8], size: usize) -> Result<&[u8], String> {
    if bytes.len() != size || size < 4 {
        return Err("invalid pledgenote padding".into());
    }
    let length = u32::from_be_bytes(bytes[..4].try_into().map_err(|_| "invalid length")?) as usize;
    let end = length
        .checked_add(4)
        .filter(|&n| n <= size)
        .ok_or("invalid pledgenote length")?;
    if bytes[end..].iter().any(|&b| b != 0) {
        return Err("noncanonical pledgenote padding".into());
    }
    Ok(&bytes[4..end])
}

/// Encrypt with the network's existing attested offer public key, using an
/// independent key-derivation domain. Randomness is client input, never consensus.
pub fn encrypt_request(public_key: [u8; 32], request: &PrivateRequest) -> Result<Vec<u8>, String> {
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut secret = zeroize::Zeroizing::new([0u8; 32]);
    let mut nonce = [0u8; 12];
    rng.fill(secret.as_mut())
        .map_err(|_| "pledgenote random key")?;
    rng.fill(&mut nonce)
        .map_err(|_| "pledgenote random nonce")?;
    let private = x25519_dalek::StaticSecret::from(*secret);
    let public = x25519_dalek::PublicKey::from(&private);
    let shared = private.diffie_hellman(&x25519_dalek::PublicKey::from(public_key));
    if !shared.was_contributory() {
        return Err("invalid pledgenote public key".into());
    }
    let key = zeroize::Zeroizing::new(crate::offer_encrypt::hkdf_sha256(
        ENVELOPE_DOMAIN,
        shared.as_bytes(),
        ENVELOPE_DOMAIN,
    )?);
    let plain = zeroize::Zeroizing::new(pad(&encode(request)?, ENVELOPE_PLAINTEXT_BYTES)?);
    let mut bytes = public.as_bytes().to_vec();
    bytes.extend_from_slice(&nonce);
    bytes.extend(crate::offer_encrypt::chacha20poly1305_encrypt(
        &key, &nonce, &plain,
    )?);
    Ok(bytes)
}

pub fn decrypt_receipt(view_key: &[u8; 32], bytes: &[u8]) -> Result<Receipt, String> {
    if bytes.len() != 12 + ENVELOPE_PLAINTEXT_BYTES + 16 {
        return Err("invalid receipt length".into());
    }
    let nonce: [u8; 12] = bytes[..12]
        .try_into()
        .map_err(|_| "invalid receipt nonce")?;
    let unbound = ring::aead::UnboundKey::new(&ring::aead::CHACHA20_POLY1305, view_key)
        .map_err(|_| "invalid receipt key")?;
    let key = ring::aead::LessSafeKey::new(unbound);
    let mut plain = zeroize::Zeroizing::new(bytes[12..].to_vec());
    let opened = key
        .open_in_place(
            ring::aead::Nonce::assume_unique_for_key(nonce),
            ring::aead::Aad::empty(),
            &mut plain,
        )
        .map_err(|_| "invalid receipt authentication")?;
    decode(unpad(opened, ENVELOPE_PLAINTEXT_BYTES)?)
}

pub fn verify_response(public_key: &[u8; 32], response: &Response) -> Result<(), String> {
    let key = ed25519_dalek::VerifyingKey::from_bytes(public_key)
        .map_err(|_| "invalid attestation key")?;
    let signature = ed25519_dalek::Signature::from_slice(&response.attestation)
        .map_err(|_| "invalid attestation length")?;
    key.verify_strict(
        &attestation_preimage(response.inputs_hash, &response.reply)?,
        &signature,
    )
    .map_err(|_| "invalid ledger attestation".into())
}
