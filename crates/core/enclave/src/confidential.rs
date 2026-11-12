//! Deterministic confidential ledger keys, slot ciphertexts, and write authorizations.
use crate::crypto::{chacha20poly1305_decrypt, chacha20poly1305_encrypt, hkdf_sha256};
use crate::errors::{Result, TeeError};
use alloy_primitives::{Address, B256, U256};
use outbe_tee::protocol::{GratisOp, Ledger, PromisOp};
use ring::hmac;
use zeroize::Zeroizing;

/// Every ledger uses field zero for its primary balance.
pub const FIELD_BALANCE: u8 = 0;

/// Immutable labels separate each ledger from the other confidential ledgers.
pub struct Domain {
    state_info: &'static [u8],
    pub account_keys: AccountKeyDerivation,
    pub authorization: ModifyDomain,
}

pub struct AccountKeyDerivation {
    view_info: &'static [u8],
    modify_info: &'static [u8],
}
pub struct SlotCipherDomain {
    pub nonce_info: &'static [u8],
}
pub struct ModifyDomain {
    modify_tag: &'static [u8],
}

/// The fields that a modify authorization authenticates, in canonical order.
pub struct ModifyAuthorization {
    pub account: Address,
    pub op_tag: u8,
    pub amount: U256,
    pub op_nonce: u64,
    pub chain_id: B256,
}

/// A ledger operation whose discriminant a modify authorization binds.
pub trait OperationTag: Copy {
    fn tag(self) -> u8;
}

impl OperationTag for GratisOp {
    fn tag(self) -> u8 {
        self as u8
    }
}

impl OperationTag for PromisOp {
    fn tag(self) -> u8 {
        self as u8
    }
}

/// The account operation covered by a modify authorization.
pub struct ModifyOperation<Op> {
    pub account: Address,
    pub op: Op,
    pub amount: U256,
    pub op_nonce: u64,
    pub chain_id: B256,
}

impl<Op: OperationTag> ModifyOperation<Op> {
    pub(crate) fn authorization(&self) -> ModifyAuthorization {
        ModifyAuthorization {
            account: self.account,
            op_tag: self.op.tag(),
            amount: self.amount,
            op_nonce: self.op_nonce,
            chain_id: self.chain_id,
        }
    }
}

/// A borrowed slot context keeps the key outside persistent enclave state.
pub struct SlotCipher<'a> {
    domain: &'a SlotCipherDomain,
    view_key: &'a [u8; 32],
    account: Address,
    field: u8,
}

pub const GRATIS: Domain = Domain {
    state_info: b"outbe/gratis/state-key/v1/",
    account_keys: AccountKeyDerivation {
        view_info: b"outbe/gratis/view-key/v1",
        modify_info: b"outbe/gratis/modify-key/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/gratis/modify/v1",
    },
};

pub const PROMIS: Domain = Domain {
    state_info: b"outbe/promis/state-key/v1/",
    account_keys: AccountKeyDerivation {
        view_info: b"outbe/promis/view-key/v1",
        modify_info: b"outbe/promis/modify-key/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/promis/modify/v1",
    },
};

pub const FIDELITY: Domain = Domain {
    state_info: b"outbe/fidelity/state-key/v1/",
    account_keys: AccountKeyDerivation {
        view_info: b"outbe/fidelity/view-key/v1",
        modify_info: b"outbe/fidelity/modify-key/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/fidelity/modify/v1",
    },
};

pub const TRIBUTE: Domain = Domain {
    state_info: b"outbe/tribute/state-key/v1/",
    account_keys: AccountKeyDerivation {
        view_info: b"outbe/tribute/view-key/v1",
        modify_info: b"outbe/tribute/modify-key/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/tribute/modify/v1",
    },
};

pub const NOD: Domain = Domain {
    state_info: b"outbe/nod/state-key/v1/",
    account_keys: AccountKeyDerivation {
        view_info: b"outbe/nod/view-key/v1",
        modify_info: b"outbe/nod/modify-key/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/nod/modify/v1",
    },
};

pub fn domain_for(ledger: Ledger) -> &'static Domain {
    match ledger {
        Ledger::Gratis => &GRATIS,
        Ledger::Promis => &PROMIS,
        Ledger::Fidelity => &FIDELITY,
        Ledger::Tribute => &TRIBUTE,
        Ledger::Nod => &NOD,
    }
}

impl Domain {
    pub fn derive_state_key(
        &self,
        group_sig: &[u8],
        chain_id: B256,
        epoch: u64,
    ) -> Result<Zeroizing<[u8; 32]>> {
        let mut info = self.state_info.to_vec();
        info.extend_from_slice(epoch.to_string().as_bytes());
        Ok(Zeroizing::new(hkdf_sha256(
            chain_id.as_slice(),
            group_sig,
            &info,
        )?))
    }
}

impl AccountKeyDerivation {
    pub fn derive_view_key(
        &self,
        state_key: &[u8; 32],
        account: Address,
    ) -> Result<Zeroizing<[u8; 32]>> {
        Ok(Zeroizing::new(hkdf_sha256(
            state_key,
            account.as_slice(),
            self.view_info,
        )?))
    }

    pub fn derive_modify_key(
        &self,
        state_key: &[u8; 32],
        account: Address,
    ) -> Result<Zeroizing<[u8; 32]>> {
        Ok(Zeroizing::new(hkdf_sha256(
            state_key,
            account.as_slice(),
            self.modify_info,
        )?))
    }
}

impl SlotCipherDomain {
    pub fn slot<'a>(
        &'a self,
        view_key: &'a [u8; 32],
        account: Address,
        field: u8,
    ) -> SlotCipher<'a> {
        SlotCipher {
            domain: self,
            view_key,
            account,
            field,
        }
    }
    pub fn slot_nonce(&self, key: &[u8; 32], ikm: &[u8], version: u64) -> Result<[u8; 12]> {
        let mut buf = ikm.to_vec();
        buf.extend_from_slice(&version.to_be_bytes());
        let okm = hkdf_sha256(key, &buf, self.nonce_info)?;
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&okm[..12]);
        Ok(nonce)
    }
}

impl SlotCipher<'_> {
    pub fn read_blob(&self, blob: &[u8]) -> Result<(u64, Vec<u8>)> {
        let view_key = self.view_key;
        let account = self.account;
        let field = self.field;
        if blob.is_empty() {
            return Ok((0, Vec::new()));
        }
        if blob.len() < 8 {
            return Err(TeeError::DecryptFailed);
        }
        let mut vbytes = [0u8; 8];
        vbytes.copy_from_slice(&blob[..8]);
        let version = u64::from_be_bytes(vbytes);
        let mut ikm = account.as_slice().to_vec();
        ikm.push(field);
        let nonce = self.domain.slot_nonce(view_key, &ikm, version)?;
        let pt = chacha20poly1305_decrypt(view_key, &nonce, &blob[8..])?;
        Ok((version, pt))
    }

    pub fn write_blob(&self, prev_version: u64, plaintext: &[u8]) -> Result<Vec<u8>> {
        let view_key = self.view_key;
        let account = self.account;
        let field = self.field;
        let version = prev_version.saturating_add(1);
        let mut ikm = account.as_slice().to_vec();
        ikm.push(field);
        let nonce = self.domain.slot_nonce(view_key, &ikm, version)?;
        let ct = chacha20poly1305_encrypt(view_key, &nonce, plaintext)?;
        let mut blob = version.to_be_bytes().to_vec();
        blob.extend_from_slice(&ct);
        Ok(blob)
    }

    pub fn read_u256(&self, version: Option<u64>, blob: &[u8]) -> Result<U256> {
        if version.is_none() {
            return Err(TeeError::DecryptFailed);
        }
        let (_, bytes) = self.read_blob(blob)?;
        let bytes = Zeroizing::new(bytes);
        let amount: &[u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| TeeError::DecryptFailed)?;
        Ok(U256::from_be_bytes(*amount))
    }

    pub fn write_u256(&self, prev_version: u64, amount: U256) -> Result<Vec<u8>> {
        let plaintext = Zeroizing::new(amount.to_be_bytes::<32>());
        self.write_blob(prev_version, plaintext.as_ref())
    }
}

impl ModifyDomain {
    fn modify_preimage(&self, request: &ModifyAuthorization) -> Vec<u8> {
        let mut b = self.modify_tag.to_vec();
        b.extend_from_slice(request.account.as_slice());
        b.push(request.op_tag);
        b.extend_from_slice(&request.amount.to_be_bytes::<32>());
        b.extend_from_slice(&request.op_nonce.to_be_bytes());
        b.extend_from_slice(request.chain_id.as_slice());
        b
    }

    pub fn modify_mac(&self, modify_key: &[u8; 32], request: &ModifyAuthorization) -> [u8; 32] {
        let key = hmac::Key::new(hmac::HMAC_SHA256, modify_key);
        let tag = hmac::sign(&key, &self.modify_preimage(request));
        let mut out = [0u8; 32];
        out.copy_from_slice(tag.as_ref());
        out
    }

    pub fn verify_modify_auth(
        &self,
        modify_key: &[u8; 32],
        request: &ModifyAuthorization,
        mac: &[u8; 32],
    ) -> bool {
        let key = hmac::Key::new(hmac::HMAC_SHA256, modify_key);
        hmac::verify(&key, &self.modify_preimage(request), mac).is_ok()
    }
}

const ENVELOPE_HEADER_LEN: usize = 8 + 4 + 32;
const ENVELOPE_TAG_LEN: usize = 16;
const BALANCE_BLOB_LEN: usize = ENVELOPE_HEADER_LEN + 32 + ENVELOPE_TAG_LEN;

/// Domain constants for a rollback-safe V2 bound envelope (`GRA2`, `PRO2`, `FID2`).
pub struct EnvelopeCipherSpec {
    pub marker: &'static [u8; 4],
    pub binding_tag: &'static [u8],
    pub key_info: &'static [u8],
    pub nonce_info: &'static [u8],
}

/// Inputs for a 92-byte V2 balance/event transition (`GRA2` / `PRO2`).
pub struct BalanceTransition {
    pub account: Address,
    pub field: u8,
    pub previous_version: u64,
    pub amount: U256,
    pub input_hash: B256,
}

/// Domain-separated `input_hash` for sealing event-only `GRA2`/`PRO2` amount
/// envelopes (`previous_version: 0`), ensuring the event envelope never shares
/// a binding, key, or nonce with the version-1 balance transition on first mint.
pub fn event_amount_input_hash(canonical_hash: B256) -> B256 {
    let mut preimage = b"outbe/tee/event-amount/v1".to_vec();
    preimage.extend_from_slice(canonical_hash.as_slice());
    alloy_primitives::keccak256(preimage)
}

impl EnvelopeCipherSpec {
    pub(crate) fn read_amount(
        &self,
        key: &[u8; 32],
        account: Address,
        field: u8,
        blob: &[u8],
    ) -> Result<(u64, U256)> {
        if blob.is_empty() {
            return Ok((0, U256::ZERO));
        }
        if blob.len() != BALANCE_BLOB_LEN {
            return Err(TeeError::DecryptFailed);
        }
        let (version, bytes) = self.read_envelope(key, account, &[field], blob)?;
        let bytes = Zeroizing::new(bytes);
        let amount: &[u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| TeeError::DecryptFailed)?;
        Ok((version, U256::from_be_bytes(*amount)))
    }

    pub(crate) fn write_amount(
        &self,
        key: &[u8; 32],
        transition: BalanceTransition,
    ) -> Result<Vec<u8>> {
        let BalanceTransition {
            account,
            field,
            previous_version,
            amount,
            input_hash,
        } = transition;
        let plaintext = Zeroizing::new(amount.to_be_bytes::<32>());
        let mut binding_payload = Zeroizing::new([0u8; 64]);
        binding_payload[..32].copy_from_slice(input_hash.as_slice());
        binding_payload[32..].copy_from_slice(plaintext.as_ref());
        self.write_envelope(
            key,
            (account, &[field]),
            (previous_version, binding_payload.as_ref()),
            plaintext.as_ref(),
        )
    }

    pub(crate) fn read_envelope(
        &self,
        key: &[u8; 32],
        account: Address,
        scope: &[u8],
        blob: &[u8],
    ) -> Result<(u64, Vec<u8>)> {
        if blob.is_empty() {
            return Ok((0, Vec::new()));
        }
        if blob.len() < ENVELOPE_HEADER_LEN + ENVELOPE_TAG_LEN || &blob[8..12] != self.marker {
            return Err(TeeError::DecryptFailed);
        }
        let version =
            u64::from_be_bytes(blob[..8].try_into().map_err(|_| TeeError::DecryptFailed)?);
        if version == 0 {
            return Err(TeeError::DecryptFailed);
        }
        let (opening, nonce) = self.opening_material(
            key,
            account,
            scope,
            (version, &blob[12..ENVELOPE_HEADER_LEN]),
        )?;
        let plaintext = chacha20poly1305_decrypt(&opening, &nonce, &blob[ENVELOPE_HEADER_LEN..])?;
        Ok((version, plaintext))
    }

    pub(crate) fn write_envelope(
        &self,
        key: &[u8; 32],
        (account, scope): (Address, &[u8]),
        (previous_version, binding_payload): (u64, &[u8]),
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let version = previous_version
            .checked_add(1)
            .ok_or(TeeError::EncryptFailed)?;
        let mut context = self.binding_tag.to_vec();
        context.extend_from_slice(account.as_slice());
        context.extend_from_slice(scope);
        context.extend_from_slice(&version.to_be_bytes());
        context.extend_from_slice(binding_payload);
        let context = Zeroizing::new(context);
        let binding = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), &context);
        let (sealing, nonce) =
            self.opening_material(key, account, scope, (version, binding.as_ref()))?;
        let ciphertext = chacha20poly1305_encrypt(&sealing, &nonce, plaintext)?;
        let mut blob = version.to_be_bytes().to_vec();
        blob.extend_from_slice(self.marker);
        blob.extend_from_slice(binding.as_ref());
        blob.extend_from_slice(&ciphertext);
        Ok(blob)
    }

    pub(crate) fn opening_material(
        &self,
        key: &[u8; 32],
        account: Address,
        scope: &[u8],
        (version, binding): (u64, &[u8]),
    ) -> Result<(Zeroizing<[u8; 32]>, [u8; 12])> {
        let mut context = account.as_slice().to_vec();
        context.extend_from_slice(scope);
        context.extend_from_slice(&version.to_be_bytes());
        context.extend_from_slice(binding);
        let derived = Zeroizing::new(hkdf_sha256(key, &context, self.key_info)?);
        let nonce_bytes = hkdf_sha256(&*derived, &context, self.nonce_info)?;
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&nonce_bytes[..12]);
        Ok((derived, nonce))
    }
}
