//! Synchronous confidential state. Chain storage is authoritative; enclave caches
//! are disposable. Journal records contain completed transitions, never work to run.
use crate::protocol::*;
use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::{
    addresses::{FIDELITY_ADDRESS, GRATIS_ADDRESS},
    error::{PrecompileError, Result},
    storage::{
        types::{Mapping, Slot, StorageBytes},
        StorageHandle,
    },
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Domain {
    Gratis,
    Fidelity,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub count: u64,
    pub hash: B256,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollateralAction {
    Return,
    Burn,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollateralAuthorization {
    pub credis_id: U256,
    pub collateral_id: B256,
    pub action: CollateralAction,
    pub amount: U256,
    pub expected_remaining: U256,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Call {
    Gratis(Box<GratisOpRequest>),
    Activate {
        credis_id: U256,
        smart_account: Address,
        credential: Vec<u8>,
        timestamp: u64,
    },
    Collateral {
        authorization: CollateralAuthorization,
        timestamp: u64,
        fidelity_anchor: u64,
    },
    GratisView {
        account: Address,
        field: u8,
    },
    Fidelity(Box<FidelityCohortRequest>),
    FidelitySnapshot(Box<FidelitySnapshotRequest>),
    FidelityQuery(Box<FidelityQueryRequest>),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub chain_id: B256,
    pub gratis: Head,
    pub fidelity: Head,
    pub call: Call,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub domain: Domain,
    pub before: Head,
    pub record: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Value {
    Gratis(Box<GratisOpResult>),
    Activated {
        terms: PledgeTerms,
        collateral_id: B256,
    },
    Collateral {
        amount: U256,
    },
    View {
        blob: Vec<u8>,
        nonce: u64,
    },
    Fidelity(FidelityOpOutcome),
    Snapshot(Vec<FidelityLeagueEntry>),
    Query(FidelityQueryResult),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub request_hash: B256,
    pub value: Value,
    pub updates: Vec<Update>,
    pub attestation: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Applied(Box<Applied>),
    Missing { domain: Domain, after: Head },
    Rejected { reason: String },
    Loaded,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub chain_id: B256,
    pub domain: Domain,
    pub after: Head,
    pub records: Vec<Vec<u8>>,
}

pub const RECORD_BYTES: usize = 4096;
pub const PAGE_RECORDS: u64 = 16;
pub fn advance(head: Head, record: &[u8]) -> Option<Head> {
    let mut bytes = head.hash.as_slice().to_vec();
    bytes.extend_from_slice(record);
    Some(Head {
        count: head.count.checked_add(1)?,
        hash: keccak256(bytes),
    })
}
pub fn hash(req: &Request) -> std::result::Result<B256, serde_json::Error> {
    serde_json::to_vec(req).map(keccak256)
}
pub fn attestation_preimage(result: &Applied) -> std::result::Result<Vec<u8>, serde_json::Error> {
    let mut copy = result.clone();
    copy.attestation.clear();
    let mut bytes = b"outbe/confidential/result/v1".to_vec();
    bytes.extend(serde_json::to_vec(&copy)?);
    Ok(bytes)
}
fn fault(e: impl std::fmt::Display) -> PrecompileError {
    PrecompileError::Fatal(format!("confidential state: {e}"))
}
fn layout(domain: Domain) -> (Address, u64) {
    match domain {
        Domain::Gratis => (GRATIS_ADDRESS, 2),
        Domain::Fidelity => (FIDELITY_ADDRESS, 2),
    }
}
pub fn head(storage: &StorageHandle<'_>, domain: Domain) -> Result<Head> {
    let (address, base) = layout(domain);
    Ok(Head {
        count: Slot::new(U256::from(base), address, storage.clone()).read()?,
        hash: Slot::new(U256::from(base + 1), address, storage.clone()).read()?,
    })
}
pub fn persist(storage: &StorageHandle<'_>, update: &Update) -> Result<()> {
    if head(storage, update.domain)? != update.before {
        return Err(PrecompileError::Revert("stale confidential state".into()));
    }
    if update.record.len() != RECORD_BYTES {
        return Err(fault("invalid journal record size"));
    }
    let next = advance(update.before, &update.record).ok_or_else(|| fault("journal exhausted"))?;
    let (address, base) = layout(update.domain);
    let records: Mapping<u64, StorageBytes<'_>> =
        Mapping::new(U256::from(base + 2), address, storage.clone());
    records
        .get_bytes(&update.before.count)
        .write(&update.record)?;
    let roots: Mapping<u64, B256> = Mapping::new(U256::from(base + 3), address, storage.clone());
    roots.write(&next.count, next.hash)?;
    Slot::new(U256::from(base), address, storage.clone()).write(next.count)?;
    Slot::new(U256::from(base + 1), address, storage.clone()).write(next.hash)
}
/// Test transports run the same engine; production verifies the pinned enclave key.
pub fn execute(
    storage: &StorageHandle<'_>,
    call: Call,
    mut test_transport: impl FnMut(&EnclaveRequest) -> Option<EnclaveResponse>,
) -> Result<Applied> {
    let request = Request {
        chain_id: B256::from(U256::from(storage.chain_id()?)),
        gratis: head(storage, Domain::Gratis)?,
        fidelity: head(storage, Domain::Fidelity)?,
        call,
    };
    let expected_hash = hash(&request).map_err(fault)?;
    let mut last_progress = None;
    loop {
        let wire = EnclaveRequest::Confidential {
            request: Box::new(request.clone()),
        };
        let (response, pinned) = exchange(&wire, &mut test_transport)?;
        match response {
            Response::Applied(result) => {
                if result.request_hash != expected_hash {
                    return Err(fault("request hash mismatch"));
                }
                if let Some(key) = pinned {
                    use ed25519_dalek::{Signature, VerifyingKey};
                    let key = VerifyingKey::from_bytes(&key).map_err(fault)?;
                    let sig = Signature::from_slice(&result.attestation).map_err(fault)?;
                    key.verify_strict(&attestation_preimage(&result).map_err(fault)?, &sig)
                        .map_err(fault)?;
                }
                if head(storage, Domain::Gratis)? != request.gratis
                    || head(storage, Domain::Fidelity)? != request.fidelity
                {
                    return Err(PrecompileError::Revert(
                        "stale confidential response".into(),
                    ));
                }
                return Ok(*result);
            }
            Response::Rejected { reason } => return Err(PrecompileError::Revert(reason)),
            Response::Missing { domain, mut after } => {
                let target = match domain {
                    Domain::Gratis => request.gratis,
                    Domain::Fidelity => request.fidelity,
                };
                if after.count >= target.count || last_progress == Some((domain, after)) {
                    return Err(fault("invalid cache recovery cursor"));
                }
                last_progress = Some((domain, after));
                let (address, base) = layout(domain);
                let roots: Mapping<u64, B256> =
                    Mapping::new(U256::from(base + 3), address, storage.clone());
                if roots.read(&after.count)? != after.hash {
                    after = Head::default();
                }
                let journal: Mapping<u64, StorageBytes<'_>> =
                    Mapping::new(U256::from(base + 2), address, storage.clone());
                let end = after.count.saturating_add(PAGE_RECORDS).min(target.count);
                let mut records = Vec::new();
                for index in after.count..end {
                    records.push(journal.get_bytes(&index).read()?);
                }
                let page = Page {
                    chain_id: request.chain_id,
                    domain,
                    after,
                    records,
                };
                let (loaded, _) = exchange(
                    &EnclaveRequest::LoadConfidential { page },
                    &mut test_transport,
                )?;
                if !matches!(loaded, Response::Loaded | Response::Missing { .. }) {
                    return Err(fault("cache recovery rejected"));
                }
            }
            Response::Loaded => return Err(fault("unexpected cache response")),
        }
    }
}
fn exchange(
    request: &EnclaveRequest,
    test: &mut impl FnMut(&EnclaveRequest) -> Option<EnclaveResponse>,
) -> Result<(Response, Option<[u8; 32]>)> {
    let (response, key) = if let Some(response) = test(request) {
        (response, None)
    } else {
        let (key, response) =
            crate::try_with_enclave(|c| (c.attestation_pub(), c.request(request)))
                .ok_or_else(|| fault("tee_sidecar_unavailable"))?;
        (response.map_err(fault)?, Some(key))
    };
    match response {
        EnclaveResponse::Confidential { response } => Ok((response, key)),
        EnclaveResponse::Error { message } => Err(fault(message)),
        _ => Err(fault("unexpected response")),
    }
}

/// Deterministic encryption for forkable state. A keyed synthetic IV covers the
/// entire plaintext and context; divergent writes after rollback use distinct
/// derived AEAD keys. The unkeyed plaintext digest is never exposed.
pub fn seal(
    key: &[u8; 32],
    context: &[u8],
    plain: &[u8],
    padded_len: usize,
) -> std::result::Result<Vec<u8>, String> {
    use ring::{aead, hmac};
    if plain.len() > padded_len {
        return Err("confidential record exceeds bound".into());
    }
    let mut padded = plain.to_vec();
    padded.resize(padded_len, 0);
    let mut preimage = iv_context(context)?;
    preimage.extend_from_slice(&padded);
    let mac_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let iv = hmac::sign(&mac_key, &preimage);
    let mut derivation = b"outbe/confidential/aead/v1".to_vec();
    derivation.extend_from_slice(iv.as_ref());
    let subkey = hmac::sign(&mac_key, &derivation);
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, subkey.as_ref())
            .map_err(|_| "invalid key")?,
    );
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key([0; 12]),
        aead::Aad::from(context),
        &mut padded,
    )
    .map_err(|_| "encryption failed")?;
    let mut out = iv.as_ref().to_vec();
    out.extend(padded);
    Ok(out)
}
pub fn open(key: &[u8; 32], context: &[u8], blob: &[u8]) -> std::result::Result<Vec<u8>, String> {
    use ring::{aead, hmac};
    if blob.len() < 48 {
        return Err("invalid ciphertext".into());
    }
    let mac_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut derivation = b"outbe/confidential/aead/v1".to_vec();
    derivation.extend_from_slice(&blob[..32]);
    let subkey = hmac::sign(&mac_key, &derivation);
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, subkey.as_ref())
            .map_err(|_| "invalid key")?,
    );
    let mut ct = blob[32..].to_vec();
    let plain = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key([0; 12]),
            aead::Aad::from(context),
            &mut ct,
        )
        .map_err(|_| "invalid ciphertext")?;
    let mut preimage = iv_context(context)?;
    preimage.extend_from_slice(plain);
    hmac::verify(&mac_key, &preimage, &blob[..32]).map_err(|_| "invalid synthetic IV")?;
    Ok(plain.to_vec())
}

/// Decode an owner-only pledge reply. The note itself never appears in a
/// successful issuance's public calldata or storage access.
pub fn decrypt_pledge_reply(
    view_key: &[u8; 32],
    reply: &[u8],
) -> std::result::Result<B256, String> {
    let plain = open(view_key, b"outbe/pledge-reply/v1", reply)?;
    if plain.len() != 32 {
        return Err("invalid pledge reply".into());
    }
    Ok(B256::from_slice(&plain))
}
/// Decode a root-bound balance/pledged view (112 bytes, including zero accounts).
pub fn decrypt_view(
    view_key: &[u8; 32],
    account: Address,
    field: u8,
    blob: &[u8],
) -> std::result::Result<U256, String> {
    if blob.len() != 112 {
        return Err("invalid confidential view".into());
    }
    let mut context = blob[..32].to_vec();
    context.extend_from_slice(account.as_slice());
    context.push(field);
    let plain = open(view_key, &context, &blob[32..])?;
    Ok(U256::from_be_slice(&plain))
}
/// Client-side fresh X25519 sealed credential, using the existing sealed-share
/// wire format. Its fixed plaintext binds network and destination explicitly.
pub fn encrypt_pledge_credential(
    offer_public: &[u8; 32],
    chain_id: B256,
    note: B256,
    smart_account: Address,
    spend_auth: [u8; 32],
) -> std::result::Result<Vec<u8>, String> {
    use rand::RngCore;
    use ring::{aead, hkdf};
    use x25519_dalek::{PublicKey, StaticSecret};
    let mut secret = [0; 32];
    let mut nonce = [0; 12];
    rand::thread_rng().fill_bytes(&mut secret);
    rand::thread_rng().fill_bytes(&mut nonce);
    let secret = StaticSecret::from(secret);
    let public = PublicKey::from(&secret);
    let shared = secret.diffie_hellman(&PublicKey::from(*offer_public));
    if !shared.was_contributory() {
        return Err("invalid enclave public key".into());
    }
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, offer_public);
    let prk = salt.extract(shared.as_bytes());
    let info = [b"outbe/tee/dkg-share/v1".as_slice()];
    let mut material = [0; 32];
    prk.expand(&info, hkdf::HKDF_SHA256)
        .map_err(|_| "key derivation failed")?
        .fill(&mut material)
        .map_err(|_| "key derivation failed")?;
    let key = aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &material).map_err(|_| "invalid key")?,
    );
    let mut plain = b"outbe/credis/credential/v1\0\0\0\0\0\0".to_vec();
    plain.extend_from_slice(chain_id.as_slice());
    plain.extend_from_slice(note.as_slice());
    plain.extend_from_slice(smart_account.as_slice());
    plain.extend_from_slice(&spend_auth);
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::empty(),
        &mut plain,
    )
    .map_err(|_| "credential encryption failed")?;
    let mut out = public.as_bytes().to_vec();
    out.extend_from_slice(&nonce);
    out.extend(plain);
    Ok(out)
}

fn iv_context(context: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let len = u64::try_from(context.len()).map_err(|_| "context too long")?;
    let mut bytes = b"outbe/confidential/iv/v1".to_vec();
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.extend_from_slice(context);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deterministic_seals_bind_context_and_survive_divergent_reexecution() {
        let key = [1; 32];
        let a = seal(&key, b"context", &[2; 32], 32).unwrap();
        let b = seal(&key, b"context", &[3; 32], 32).unwrap();
        assert_ne!(&a[..32], &b[..32]);
        assert_eq!(a, seal(&key, b"context", &[2; 32], 32).unwrap());
        assert_eq!(open(&key, b"context", &a).unwrap(), vec![2; 32]);
        assert!(open(&key, b"other context", &a).is_err());
        assert!(open(&[2; 32], b"context", &a).is_err());
        let mut corrupt = a.clone();
        corrupt[33] ^= 1;
        assert!(open(&key, b"context", &corrupt).is_err());
        assert_eq!(alloy_primitives::hex::encode(a), "5e3c11467f6eafe4ccbd8126639d6b1c9c0be3a30038174bb9efa60eafcaf95a41848a5f763152bdcee283ff6027108d20a89ee3cf34a3f299fc964407c580a7c133c7200baac17df27364205c670b5f");
    }
}
