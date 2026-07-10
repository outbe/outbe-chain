//! Enclave-side Gratis confidential state engine (secret-bearing).
//!
//! Derives per-account view/modify keys and the resident `gratis_state_key` from
//! the DKG group signature, and applies Gratis write ops over deterministically
//! encrypted balances. Every function here is a pure transform of its inputs +
//! the resident state key, so every validator's enclave produces byte-identical
//! ciphertext (consensus determinism) — the same property `encrypt_share_deterministic`
//! provides for the on-chain offer-key seal.
//!
//! Blob format for every stored ciphertext: `version(8, big-endian) || AEAD-ct`.
//! The monotonic `version` is folded into the AEAD nonce so overwriting a slot
//! never reuses a `(key, nonce)` pair; it is also what lets the client derive the
//! nonce to decrypt (balance/pledged use the account's view key as the AEAD key).

use alloy_primitives::{Address, B256, U256};
use ring::hmac;

use outbe_tee::protocol::{GratisOp, GratisOpRequest, GratisOpResult, GratisOpStatus};
use outbe_tee::{
    GRATIS_MODIFY_KEY_INFO, GRATIS_NONCE_INFO, GRATIS_PLEDGE_HANDLE_INFO, GRATIS_STATE_HKDF_INFO,
    GRATIS_VIEW_KEY_INFO,
};

use crate::crypto::{chacha20poly1305_decrypt, chacha20poly1305_encrypt, hkdf_sha256};
use crate::errors::{Result, TeeError};

/// AEAD-slot field tags folded into the nonce derivation so a ciphertext cannot be
/// lifted between an account's balance and pledged slots.
const FIELD_BALANCE: u8 = 0;
const FIELD_PLEDGED: u8 = 1;

/// Pledge-record plaintext: `amount(32) ‖ eoa(20) ‖ bundle(20) ‖ total(4) ‖
/// remaining(4) ‖ spent(1)`.
const RECORD_PLAINTEXT_LEN: usize = 32 + 20 + 20 + 4 + 4 + 1;

const MODIFY_PREIMAGE_TAG: &[u8] = b"outbe/gratis/modify/v1";
const SPEND_BIND_TAG: &[u8] = b"outbe/gratis/credis-bind/v1";

// --- Key derivation -------------------------------------------------------------

/// Derive the resident Gratis state key from the DKG group signature — identical
/// across every committee enclave (so encrypted state is byte-identical), bound to
/// chain + epoch. Mirrors [`crate::crypto::derive_tribute_offer_secret_from_group_sig`].
pub fn derive_gratis_state_key(group_sig: &[u8], chain_id: B256, epoch: u64) -> Result<[u8; 32]> {
    let mut info = GRATIS_STATE_HKDF_INFO.to_vec();
    info.extend_from_slice(epoch.to_string().as_bytes());
    hkdf_sha256(chain_id.as_slice(), group_sig, &info)
}

/// Per-account view key: read capability AND the AEAD key for the account's
/// balance/pledged blobs, so a holder can decrypt its own state client-side.
pub fn derive_view_key(state_key: &[u8; 32], account: Address) -> Result<[u8; 32]> {
    hkdf_sha256(state_key, account.as_slice(), GRATIS_VIEW_KEY_INFO)
}

/// Per-account modify key: authorizes writes (via HMAC); never decrypts state.
pub fn derive_modify_key(state_key: &[u8; 32], account: Address) -> Result<[u8; 32]> {
    hkdf_sha256(state_key, account.as_slice(), GRATIS_MODIFY_KEY_INFO)
}

/// Deterministic pledge handle (public record id) that replaces the old ZK
/// commitment. Unique per `(account, amount, op_nonce)`.
pub fn derive_pledge_handle(
    state_key: &[u8; 32],
    account: Address,
    amount: U256,
    op_nonce: u64,
) -> Result<B256> {
    let mut ikm = account.as_slice().to_vec();
    ikm.extend_from_slice(&amount.to_be_bytes::<32>());
    ikm.extend_from_slice(&op_nonce.to_be_bytes());
    Ok(B256::from(hkdf_sha256(
        state_key,
        &ikm,
        GRATIS_PLEDGE_HANDLE_INFO,
    )?))
}

// --- Authorization MACs (also used by clients/tests to produce the auth) --------

fn modify_preimage(
    account: Address,
    op: GratisOp,
    amount: U256,
    op_nonce: u64,
    chain_id: B256,
) -> Vec<u8> {
    let mut b = MODIFY_PREIMAGE_TAG.to_vec();
    b.extend_from_slice(account.as_slice());
    b.push(op as u8);
    b.extend_from_slice(&amount.to_be_bytes::<32>());
    b.extend_from_slice(&op_nonce.to_be_bytes());
    b.extend_from_slice(chain_id.as_slice());
    b
}

/// `HMAC-SHA256(modify_key, preimage)` — the write authorization the client sends
/// and the enclave re-checks.
pub fn modify_mac(
    modify_key: &[u8; 32],
    account: Address,
    op: GratisOp,
    amount: U256,
    op_nonce: u64,
    chain_id: B256,
) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, modify_key);
    let tag = hmac::sign(
        &key,
        &modify_preimage(account, op, amount, op_nonce, chain_id),
    );
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

fn verify_modify_auth(
    modify_key: &[u8; 32],
    account: Address,
    op: GratisOp,
    amount: U256,
    op_nonce: u64,
    chain_id: B256,
    mac: &[u8; 32],
) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, modify_key);
    hmac::verify(
        &key,
        &modify_preimage(account, op, amount, op_nonce, chain_id),
        mac,
    )
    .is_ok()
}

/// Per-pledge spend secret the EOA derives locally from its modify key + the
/// public handle, then hands to the CCA off-chain. `HMAC(modify_key, handle)`.
pub fn pledge_secret(modify_key: &[u8; 32], handle: B256) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, modify_key);
    let tag = hmac::sign(&key, handle.as_slice());
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

/// Spend authorization binding a pledge to a destination bundle account, so a
/// mempool observer of `requestCredis(handle, spend_auth)` cannot redirect it.
/// `HMAC(pledge_secret, "credis-bind" ‖ bundle)`.
pub fn spend_auth_mac(pledge_secret: &[u8; 32], bundle: Address) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, pledge_secret);
    let mut msg = SPEND_BIND_TAG.to_vec();
    msg.extend_from_slice(bundle.as_slice());
    let tag = hmac::sign(&key, &msg);
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

// --- Versioned deterministic AEAD over per-account amounts ----------------------

fn slot_nonce(key: &[u8; 32], ikm: &[u8], version: u64) -> Result<[u8; 12]> {
    let mut buf = ikm.to_vec();
    buf.extend_from_slice(&version.to_be_bytes());
    let okm = hkdf_sha256(key, &buf, GRATIS_NONCE_INFO)?;
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&okm[..12]);
    Ok(nonce)
}

/// Decrypt a `version ‖ ct` amount blob; an empty blob is a fresh slot (`0`).
fn read_amount(
    view_key: &[u8; 32],
    account: Address,
    field: u8,
    blob: &[u8],
) -> Result<(u64, U256)> {
    if blob.is_empty() {
        return Ok((0, U256::ZERO));
    }
    if blob.len() < 8 {
        return Err(TeeError::DecryptFailed);
    }
    let mut vbytes = [0u8; 8];
    vbytes.copy_from_slice(&blob[..8]);
    let version = u64::from_be_bytes(vbytes);
    let mut ikm = account.as_slice().to_vec();
    ikm.push(field);
    let nonce = slot_nonce(view_key, &ikm, version)?;
    let pt = chacha20poly1305_decrypt(view_key, &nonce, &blob[8..])?;
    if pt.len() != 32 {
        return Err(TeeError::DecryptFailed);
    }
    Ok((version, U256::from_be_slice(&pt)))
}

/// Encrypt `amount` into a fresh `version+1 ‖ ct` blob.
fn write_amount(
    view_key: &[u8; 32],
    account: Address,
    field: u8,
    prev_version: u64,
    amount: U256,
) -> Result<Vec<u8>> {
    let version = prev_version.saturating_add(1);
    let mut ikm = account.as_slice().to_vec();
    ikm.push(field);
    let nonce = slot_nonce(view_key, &ikm, version)?;
    let ct = chacha20poly1305_encrypt(view_key, &nonce, &amount.to_be_bytes::<32>())?;
    let mut blob = version.to_be_bytes().to_vec();
    blob.extend_from_slice(&ct);
    Ok(blob)
}

/// Client-side helper: decrypt an account's balance blob with its view key (the
/// key delivered by `DeriveAccountKeys`). Same primitive the enclave uses, so a
/// client reproduces the plaintext without ever touching the state key.
pub fn decrypt_balance(view_key: &[u8; 32], account: Address, blob: &[u8]) -> Result<U256> {
    read_amount(view_key, account, FIELD_BALANCE, blob).map(|(_, v)| v)
}

/// Client-side helper: decrypt an account's pledged-ledger blob with its view key.
pub fn decrypt_pledged(view_key: &[u8; 32], account: Address, blob: &[u8]) -> Result<U256> {
    read_amount(view_key, account, FIELD_PLEDGED, blob).map(|(_, v)| v)
}

// --- Pledge record --------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct PledgeRecord {
    amount: U256,
    eoa: Address,
    bundle: Address,
    total: u32,
    remaining: u32,
    spent: bool,
}

impl PledgeRecord {
    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(RECORD_PLAINTEXT_LEN);
        b.extend_from_slice(&self.amount.to_be_bytes::<32>());
        b.extend_from_slice(self.eoa.as_slice());
        b.extend_from_slice(self.bundle.as_slice());
        b.extend_from_slice(&self.total.to_be_bytes());
        b.extend_from_slice(&self.remaining.to_be_bytes());
        b.push(self.spent as u8);
        b
    }

    fn decode(b: &[u8]) -> Result<Self> {
        if b.len() != RECORD_PLAINTEXT_LEN {
            return Err(TeeError::DecryptFailed);
        }
        let to4 =
            |s: &[u8]| -> Result<[u8; 4]> { s.try_into().map_err(|_| TeeError::DecryptFailed) };
        Ok(Self {
            amount: U256::from_be_slice(&b[0..32]),
            eoa: Address::from_slice(&b[32..52]),
            bundle: Address::from_slice(&b[52..72]),
            total: u32::from_be_bytes(to4(&b[72..76])?),
            remaining: u32::from_be_bytes(to4(&b[76..80])?),
            spent: b[80] != 0,
        })
    }
}

fn read_record(state_key: &[u8; 32], handle: B256, blob: &[u8]) -> Result<(u64, PledgeRecord)> {
    if blob.len() < 8 {
        return Err(TeeError::DecryptFailed);
    }
    let mut vbytes = [0u8; 8];
    vbytes.copy_from_slice(&blob[..8]);
    let version = u64::from_be_bytes(vbytes);
    let nonce = slot_nonce(state_key, handle.as_slice(), version)?;
    let pt = chacha20poly1305_decrypt(state_key, &nonce, &blob[8..])?;
    Ok((version, PledgeRecord::decode(&pt)?))
}

fn write_record(
    state_key: &[u8; 32],
    handle: B256,
    prev_version: u64,
    rec: &PledgeRecord,
) -> Result<Vec<u8>> {
    let version = prev_version.saturating_add(1);
    let nonce = slot_nonce(state_key, handle.as_slice(), version)?;
    let ct = chacha20poly1305_encrypt(state_key, &nonce, &rec.encode())?;
    let mut blob = version.to_be_bytes().to_vec();
    blob.extend_from_slice(&ct);
    Ok(blob)
}

// --- The op engine --------------------------------------------------------------

fn base_result() -> GratisOpResult {
    GratisOpResult {
        status: GratisOpStatus::Applied,
        new_balance: Vec::new(),
        new_pledged: Vec::new(),
        new_pledge_record: Vec::new(),
        pledge_handle: B256::ZERO,
        gratis_amount: U256::ZERO,
        pledger_eoa: Address::ZERO,
        event_amount: U256::ZERO,
        next_op_nonce: 0,
        inputs_canonical_hash: B256::ZERO,
        attestation_tag: Vec::new(),
    }
}

fn reject(reason: impl Into<String>) -> GratisOpResult {
    let mut r = base_result();
    r.status = GratisOpStatus::Rejected {
        reason: reason.into(),
    };
    r
}

/// Apply a Gratis op over encrypted state. Pure and deterministic given
/// `state_key` + `req`. Sets `inputs_canonical_hash`; the caller (dispatch) signs
/// and fills `attestation_tag`. Business rejections come back as
/// `GratisOpStatus::Rejected` (→ precompile revert), never a panic.
pub fn apply_op(state_key: &[u8; 32], req: &GratisOpRequest) -> GratisOpResult {
    let inputs_canonical_hash = outbe_tee::protocol::gratis_op_canonical_hash(req);
    let mut result = match apply_op_inner(state_key, req) {
        Ok(r) => r,
        Err(e) => reject(e.to_string()),
    };
    result.inputs_canonical_hash = inputs_canonical_hash;
    result
}

fn apply_op_inner(state_key: &[u8; 32], req: &GratisOpRequest) -> Result<GratisOpResult> {
    match req.op {
        GratisOp::Mine | GratisOp::Burn | GratisOp::Pledge | GratisOp::Unpledge => {
            apply_owner_op(state_key, req)
        }
        GratisOp::PledgeToBundle => apply_pledge_to_bundle(state_key, req),
        GratisOp::UnlockToEoa => apply_unlock_to_eoa(state_key, req),
    }
}

/// Mine/Burn/Pledge/Unpledge — all modify-key gated and keyed by `req.account`.
fn apply_owner_op(state_key: &[u8; 32], req: &GratisOpRequest) -> Result<GratisOpResult> {
    if req.amount.is_zero() {
        return Ok(reject("amount must be positive"));
    }
    if req.account.is_zero() {
        return Ok(reject("invalid address"));
    }
    let modify_key = derive_modify_key(state_key, req.account)?;
    if !verify_modify_auth(
        &modify_key,
        req.account,
        req.op,
        req.amount,
        req.modify_auth.op_nonce,
        req.chain_id,
        &req.modify_auth.mac,
    ) {
        return Ok(reject("invalid modify authorization"));
    }

    let view_key = derive_view_key(state_key, req.account)?;
    let (bver, balance) = read_amount(&view_key, req.account, FIELD_BALANCE, &req.current_balance)?;
    let (pver, pledged) = read_amount(&view_key, req.account, FIELD_PLEDGED, &req.current_pledged)?;

    let mut r = base_result();
    r.event_amount = req.amount;
    r.next_op_nonce = req.modify_auth.op_nonce.saturating_add(1);

    match req.op {
        GratisOp::Mine => {
            let new_balance = match balance.checked_add(req.amount) {
                Some(v) => v,
                None => return Ok(reject("gratis balance overflow")),
            };
            r.new_balance = write_amount(&view_key, req.account, FIELD_BALANCE, bver, new_balance)?;
        }
        GratisOp::Burn => {
            if balance < req.amount {
                return Ok(reject("insufficient balance"));
            }
            r.new_balance = write_amount(
                &view_key,
                req.account,
                FIELD_BALANCE,
                bver,
                balance - req.amount,
            )?;
        }
        GratisOp::Pledge => {
            if balance < req.amount {
                return Ok(reject("insufficient balance"));
            }
            let new_pledged = match pledged.checked_add(req.amount) {
                Some(v) => v,
                None => return Ok(reject("gratis pledged overflow")),
            };
            let handle =
                derive_pledge_handle(state_key, req.account, req.amount, req.modify_auth.op_nonce)?;
            let record = PledgeRecord {
                amount: req.amount,
                eoa: req.account,
                bundle: Address::ZERO,
                total: req.installments.max(1),
                remaining: req.installments.max(1),
                spent: false,
            };
            r.new_balance = write_amount(
                &view_key,
                req.account,
                FIELD_BALANCE,
                bver,
                balance - req.amount,
            )?;
            r.new_pledged = write_amount(&view_key, req.account, FIELD_PLEDGED, pver, new_pledged)?;
            r.new_pledge_record = write_record(state_key, handle, 0, &record)?;
            r.pledge_handle = handle;
        }
        GratisOp::Unpledge => {
            // Direct unpledge (e.g. credis rejected) is record-based: it fully
            // closes an UNSPENT pledge so the record can never later be consumed
            // for credis (double-spend) or drained via UnlockToEoa.
            let Some(handle) = req.pledge_handle else {
                return Ok(reject("unpledge requires a pledge handle"));
            };
            let (rver, mut record) = read_record(state_key, handle, &req.current_pledge_record)?;
            if record.spent {
                return Ok(reject("pledge already consumed"));
            }
            if record.eoa != req.account {
                return Ok(reject("unpledge account does not match pledge record"));
            }
            if record.amount != req.amount {
                return Ok(reject("unpledge amount does not match pledge record"));
            }
            if pledged < record.amount {
                return Ok(reject("insufficient pledged balance"));
            }
            let new_balance = match balance.checked_add(record.amount) {
                Some(v) => v,
                None => return Ok(reject("gratis balance overflow")),
            };
            record.spent = true;
            record.remaining = 0;
            r.new_balance = write_amount(&view_key, req.account, FIELD_BALANCE, bver, new_balance)?;
            r.new_pledged = write_amount(
                &view_key,
                req.account,
                FIELD_PLEDGED,
                pver,
                pledged - record.amount,
            )?;
            r.new_pledge_record = write_record(state_key, handle, rver, &record)?;
        }
        _ => unreachable!("apply_owner_op only handles owner ops"),
    }
    Ok(r)
}

/// requestCredis: consume a pledge record, verify the spend binding to the bundle
/// account, mark it spent, and surface the pledged amount. No balance move.
fn apply_pledge_to_bundle(state_key: &[u8; 32], req: &GratisOpRequest) -> Result<GratisOpResult> {
    let (Some(handle), Some(bundle), Some(spend_auth)) =
        (req.pledge_handle, req.bundle_account, req.spend_auth)
    else {
        return Ok(reject(
            "pledge_to_bundle requires handle, bundle, and spend_auth",
        ));
    };
    let (rver, mut record) = read_record(state_key, handle, &req.current_pledge_record)?;
    if record.spent {
        return Ok(reject("pledge already spent"));
    }
    let modify_key = derive_modify_key(state_key, record.eoa)?;
    let secret = pledge_secret(&modify_key, handle);
    let expected = spend_auth_mac(&secret, bundle);
    if !constant_time_eq(&expected, &spend_auth) {
        return Ok(reject("invalid spend authorization"));
    }
    record.spent = true;
    record.bundle = bundle;

    let mut r = base_result();
    r.new_pledge_record = write_record(state_key, handle, rver, &record)?;
    r.gratis_amount = record.amount;
    // Reveal the pledger EOA so the credis position can store it for the later
    // per-installment unlock (accepted linkage-visibility tradeoff).
    r.pledger_eoa = record.eoa;
    r.event_amount = record.amount;
    Ok(r)
}

/// payAnadosis: release one installment of pledged collateral back to the original
/// EOA's balance. `req.account` is the EOA (supplied by the host from the credis
/// position); the enclave checks the record binds to it.
///
// TODO(privacy): the host must know the EOA to pick its balance slot, so the
// credis position stores the pledger EOA in plaintext — validators can see the
// EOA↔bundle linkage (amounts stay encrypted). To restore ZK-style unlinkability,
// keep a per-position→EOA mapping resident in the enclave and credit the EOA via
// an enclave-curated balance path so the host never learns the pledger.
fn apply_unlock_to_eoa(state_key: &[u8; 32], req: &GratisOpRequest) -> Result<GratisOpResult> {
    let Some(handle) = req.pledge_handle else {
        return Ok(reject("unlock requires a pledge handle"));
    };
    let (rver, mut record) = read_record(state_key, handle, &req.current_pledge_record)?;
    if !record.spent {
        return Ok(reject("pledge not requested for credis"));
    }
    if record.eoa != req.account {
        return Ok(reject("unlock account does not match pledge record"));
    }
    if record.remaining == 0 {
        return Ok(reject("pledge fully released"));
    }
    let total = record.total.max(1);
    let per = record.amount / U256::from(total);
    // The final installment releases the remainder so the sum is exactly `amount`.
    let release = if record.remaining == 1 {
        record.amount - per * U256::from(total - 1)
    } else {
        per
    };

    let view_key = derive_view_key(state_key, req.account)?;
    let (bver, balance) = read_amount(&view_key, req.account, FIELD_BALANCE, &req.current_balance)?;
    let (pver, pledged) = read_amount(&view_key, req.account, FIELD_PLEDGED, &req.current_pledged)?;
    let new_balance = match balance.checked_add(release) {
        Some(v) => v,
        None => return Ok(reject("gratis balance overflow")),
    };
    if pledged < release {
        return Ok(reject("pledged ledger underflow"));
    }
    record.remaining -= 1;

    let mut r = base_result();
    r.new_balance = write_amount(&view_key, req.account, FIELD_BALANCE, bver, new_balance)?;
    r.new_pledged = write_amount(
        &view_key,
        req.account,
        FIELD_PLEDGED,
        pver,
        pledged - release,
    )?;
    r.new_pledge_record = write_record(state_key, handle, rver, &record)?;
    r.gratis_amount = release;
    r.event_amount = release;
    Ok(r)
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_tee::protocol::ModifyAuth;

    const CHAIN: B256 = B256::repeat_byte(0xC1);
    fn state_key() -> [u8; 32] {
        derive_gratis_state_key(b"a-group-threshold-signature-~48-bytes-long!!", CHAIN, 0).unwrap()
    }
    fn alice() -> Address {
        Address::repeat_byte(0x11)
    }
    fn bundle() -> Address {
        Address::repeat_byte(0xBB)
    }

    fn auth(sk: &[u8; 32], acct: Address, op: GratisOp, amount: U256, nonce: u64) -> ModifyAuth {
        let mk = derive_modify_key(sk, acct).unwrap();
        ModifyAuth {
            mac: modify_mac(&mk, acct, op, amount, nonce, CHAIN),
            op_nonce: nonce,
        }
    }

    fn req(op: GratisOp, acct: Address, amount: U256, nonce: u64) -> GratisOpRequest {
        GratisOpRequest {
            op,
            chain_id: CHAIN,
            account: acct,
            amount,
            current_balance: Vec::new(),
            current_pledged: Vec::new(),
            current_pledge_record: Vec::new(),
            modify_auth: ModifyAuth {
                mac: [0u8; 32],
                op_nonce: nonce,
            },
            installments: 10,
            pledge_handle: None,
            bundle_account: None,
            spend_auth: None,
        }
    }

    #[test]
    fn mine_is_deterministic_across_calls() {
        let sk = state_key();
        let mut r = req(GratisOp::Mine, alice(), U256::from(1000u64), 0);
        r.modify_auth = auth(&sk, alice(), GratisOp::Mine, r.amount, 0);
        let a = apply_op(&sk, &r);
        let b = apply_op(&sk, &r);
        assert_eq!(a, b, "same state key + request → byte-identical result");
        assert!(matches!(a.status, GratisOpStatus::Applied));
        assert!(!a.new_balance.is_empty());
    }

    #[test]
    fn view_key_decrypts_minted_balance() {
        let sk = state_key();
        let mut r = req(GratisOp::Mine, alice(), U256::from(4242u64), 0);
        r.modify_auth = auth(&sk, alice(), GratisOp::Mine, r.amount, 0);
        let res = apply_op(&sk, &r);
        // A holder with only the view key can decrypt the balance blob.
        let vk = derive_view_key(&sk, alice()).unwrap();
        let (_v, bal) = read_amount(&vk, alice(), FIELD_BALANCE, &res.new_balance).unwrap();
        assert_eq!(bal, U256::from(4242u64));
        assert_eq!(res.next_op_nonce, 1);
    }

    #[test]
    fn mine_rejects_forged_modify_auth() {
        let sk = state_key();
        let mut r = req(GratisOp::Mine, alice(), U256::from(1u64), 0);
        r.modify_auth = auth(&sk, alice(), GratisOp::Mine, r.amount, 0);
        r.modify_auth.mac[0] ^= 0xff;
        assert!(matches!(
            apply_op(&sk, &r).status,
            GratisOpStatus::Rejected { .. }
        ));
    }

    #[test]
    fn burn_requires_sufficient_balance() {
        let sk = state_key();
        // mint 100
        let mut m = req(GratisOp::Mine, alice(), U256::from(100u64), 0);
        m.modify_auth = auth(&sk, alice(), GratisOp::Mine, m.amount, 0);
        let minted = apply_op(&sk, &m);
        // burn 200 → reject
        let mut b = req(GratisOp::Burn, alice(), U256::from(200u64), 1);
        b.current_balance = minted.new_balance.clone();
        b.modify_auth = auth(&sk, alice(), GratisOp::Burn, b.amount, 1);
        assert!(matches!(
            apply_op(&sk, &b).status,
            GratisOpStatus::Rejected { .. }
        ));
    }

    #[test]
    fn pledge_request_credis_and_unlock_flow() {
        let sk = state_key();
        // mine 1000 to alice
        let mut m = req(GratisOp::Mine, alice(), U256::from(1000u64), 0);
        m.modify_auth = auth(&sk, alice(), GratisOp::Mine, m.amount, 0);
        let minted = apply_op(&sk, &m);

        // pledge 1000 (10 installments)
        let mut p = req(GratisOp::Pledge, alice(), U256::from(1000u64), 1);
        p.current_balance = minted.new_balance.clone();
        p.installments = 10;
        p.modify_auth = auth(&sk, alice(), GratisOp::Pledge, p.amount, 1);
        let pledged = apply_op(&sk, &p);
        assert!(matches!(pledged.status, GratisOpStatus::Applied));
        let handle = pledged.pledge_handle;
        assert_ne!(handle, B256::ZERO);
        // balance drained to 0
        let vk = derive_view_key(&sk, alice()).unwrap();
        let (_v, bal) = read_amount(&vk, alice(), FIELD_BALANCE, &pledged.new_balance).unwrap();
        assert_eq!(bal, U256::ZERO);

        // requestCredis: alice derives the pledge secret and binds to the bundle.
        let mk = derive_modify_key(&sk, alice()).unwrap();
        let secret = pledge_secret(&mk, handle);
        let spend = spend_auth_mac(&secret, bundle());
        let mut rc = req(GratisOp::PledgeToBundle, alice(), U256::ZERO, 0);
        rc.current_pledge_record = pledged.new_pledge_record.clone();
        rc.pledge_handle = Some(handle);
        rc.bundle_account = Some(bundle());
        rc.spend_auth = Some(spend);
        let credis = apply_op(&sk, &rc);
        assert!(matches!(credis.status, GratisOpStatus::Applied));
        assert_eq!(credis.gratis_amount, U256::from(1000u64));

        // A wrong bundle binding is rejected (front-running defense).
        let mut bad = rc.clone();
        bad.bundle_account = Some(Address::repeat_byte(0xEE));
        bad.current_pledge_record = pledged.new_pledge_record.clone();
        assert!(matches!(
            apply_op(&sk, &bad).status,
            GratisOpStatus::Rejected { .. }
        ));

        // Pay 10 installments → unlocks 100 each back to alice, exactly draining.
        let mut record_blob = credis.new_pledge_record.clone();
        let mut bal_blob = pledged.new_balance.clone();
        let mut pledged_blob = pledged.new_pledged.clone();
        let mut total_released = U256::ZERO;
        for _ in 0..10 {
            let mut u = req(GratisOp::UnlockToEoa, alice(), U256::ZERO, 0);
            u.pledge_handle = Some(handle);
            u.current_pledge_record = record_blob.clone();
            u.current_balance = bal_blob.clone();
            u.current_pledged = pledged_blob.clone();
            let un = apply_op(&sk, &u);
            assert!(
                matches!(un.status, GratisOpStatus::Applied),
                "{:?}",
                un.status
            );
            total_released += un.gratis_amount;
            record_blob = un.new_pledge_record.clone();
            bal_blob = un.new_balance.clone();
            pledged_blob = un.new_pledged.clone();
        }
        assert_eq!(total_released, U256::from(1000u64));
        let (_v, final_bal) = read_amount(&vk, alice(), FIELD_BALANCE, &bal_blob).unwrap();
        assert_eq!(final_bal, U256::from(1000u64));
        // 11th unlock rejected — fully released.
        let mut u = req(GratisOp::UnlockToEoa, alice(), U256::ZERO, 0);
        u.pledge_handle = Some(handle);
        u.current_pledge_record = record_blob;
        u.current_balance = bal_blob;
        u.current_pledged = pledged_blob;
        assert!(matches!(
            apply_op(&sk, &u).status,
            GratisOpStatus::Rejected { .. }
        ));
    }

    /// Helper: mine `amount` then pledge it, returning `(handle, pledge_result)`.
    fn mine_and_pledge(sk: &[u8; 32], amount: U256) -> (B256, GratisOpResult) {
        let mut m = req(GratisOp::Mine, alice(), amount, 0);
        m.modify_auth = auth(sk, alice(), GratisOp::Mine, amount, 0);
        let minted = apply_op(sk, &m);
        let mut p = req(GratisOp::Pledge, alice(), amount, 1);
        p.current_balance = minted.new_balance.clone();
        p.installments = 10;
        p.modify_auth = auth(sk, alice(), GratisOp::Pledge, amount, 1);
        let pledged = apply_op(sk, &p);
        (pledged.pledge_handle, pledged)
    }

    /// Bug 1 regression: a direct unpledge closes the record so it can no longer
    /// be consumed for credis (no double-spend).
    #[test]
    fn unpledge_closes_record_and_blocks_credis() {
        let sk = state_key();
        let amount = U256::from(1000u64);
        let (handle, pledged) = mine_and_pledge(&sk, amount);

        let mut up = req(GratisOp::Unpledge, alice(), amount, 2);
        up.pledge_handle = Some(handle);
        up.current_pledge_record = pledged.new_pledge_record.clone();
        up.current_balance = pledged.new_balance.clone();
        up.current_pledged = pledged.new_pledged.clone();
        up.modify_auth = auth(&sk, alice(), GratisOp::Unpledge, amount, 2);
        let un = apply_op(&sk, &up);
        assert!(
            matches!(un.status, GratisOpStatus::Applied),
            "{:?}",
            un.status
        );
        let vk = derive_view_key(&sk, alice()).unwrap();
        let (_v, bal) = read_amount(&vk, alice(), FIELD_BALANCE, &un.new_balance).unwrap();
        assert_eq!(bal, amount, "collateral returned to alice");

        let mk = derive_modify_key(&sk, alice()).unwrap();
        let spend = spend_auth_mac(&pledge_secret(&mk, handle), bundle());
        let mut rc = req(GratisOp::PledgeToBundle, alice(), U256::ZERO, 0);
        rc.current_pledge_record = un.new_pledge_record.clone();
        rc.pledge_handle = Some(handle);
        rc.bundle_account = Some(bundle());
        rc.spend_auth = Some(spend);
        assert!(
            matches!(apply_op(&sk, &rc).status, GratisOpStatus::Rejected { .. }),
            "closed pledge must not be spendable for credis"
        );
    }

    /// Bug 2 regression: collateral cannot be unlocked before credis was requested
    /// (the record must be `spent`).
    #[test]
    fn unlock_rejected_before_credis_requested() {
        let sk = state_key();
        let amount = U256::from(1000u64);
        let (handle, pledged) = mine_and_pledge(&sk, amount);

        let mut u = req(GratisOp::UnlockToEoa, alice(), U256::ZERO, 0);
        u.pledge_handle = Some(handle);
        u.current_pledge_record = pledged.new_pledge_record.clone();
        u.current_balance = pledged.new_balance.clone();
        u.current_pledged = pledged.new_pledged.clone();
        assert!(
            matches!(apply_op(&sk, &u).status, GratisOpStatus::Rejected { .. }),
            "unlock must require the pledge to be spent for credis first"
        );
    }
}
