//! Deterministic confidential ledger keys, slot ciphertexts, and write authorizations.
use crate::crypto::{chacha20poly1305_decrypt, chacha20poly1305_encrypt, hkdf_sha256};
use crate::errors::{Result, TeeError};
use alloy_primitives::{Address, B256, U256};
use outbe_tee::protocol::Ledger;
use ring::hmac;

/// Every ledger uses field zero for its primary balance.
pub const FIELD_BALANCE: u8 = 0;
/// Eight version bytes, 32 amount bytes, and a 16-byte authentication tag.
pub const AMOUNT_BLOB_LEN: usize = 8 + 32 + 16;

/// Immutable labels separate each ledger from the other confidential ledgers.
pub struct Domain {
    state_info: &'static [u8],
    pub account_keys: AccountKeyDerivation,
    pub cipher: SlotCipherDomain,
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
    cipher: SlotCipherDomain {
        nonce_info: b"outbe/gratis/nonce/v1",
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
    cipher: SlotCipherDomain {
        nonce_info: b"outbe/promis/nonce/v1",
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
    cipher: SlotCipherDomain {
        nonce_info: b"outbe/fidelity/nonce/v1",
    },
    authorization: ModifyDomain {
        modify_tag: b"outbe/fidelity/modify/v1",
    },
};

pub fn domain_for(ledger: Ledger) -> &'static Domain {
    match ledger {
        Ledger::Gratis => &GRATIS,
        Ledger::Promis => &PROMIS,
        Ledger::Fidelity => &FIDELITY,
    }
}

impl Domain {
    pub fn derive_state_key(
        &self,
        group_sig: &[u8],
        chain_id: B256,
        epoch: u64,
    ) -> Result<[u8; 32]> {
        let mut info = self.state_info.to_vec();
        info.extend_from_slice(epoch.to_string().as_bytes());
        hkdf_sha256(chain_id.as_slice(), group_sig, &info)
    }
}

impl AccountKeyDerivation {
    pub fn derive_view_key(&self, state_key: &[u8; 32], account: Address) -> Result<[u8; 32]> {
        hkdf_sha256(state_key, account.as_slice(), self.view_info)
    }

    pub fn derive_modify_key(&self, state_key: &[u8; 32], account: Address) -> Result<[u8; 32]> {
        hkdf_sha256(state_key, account.as_slice(), self.modify_info)
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
    pub fn read_amount(&self, blob: &[u8]) -> Result<(u64, U256)> {
        let view_key = self.view_key;
        let account = self.account;
        let field = self.field;
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
        let nonce = self.domain.slot_nonce(view_key, &ikm, version)?;
        let pt = chacha20poly1305_decrypt(view_key, &nonce, &blob[8..])?;
        if pt.len() != 32 {
            return Err(TeeError::DecryptFailed);
        }
        Ok((version, U256::from_be_slice(&pt)))
    }

    pub fn write_amount(&self, prev_version: u64, amount: U256) -> Result<Vec<u8>> {
        let view_key = self.view_key;
        let account = self.account;
        let field = self.field;
        let version = prev_version.saturating_add(1);
        let mut ikm = account.as_slice().to_vec();
        ikm.push(field);
        let nonce = self.domain.slot_nonce(view_key, &ikm, version)?;
        let ct = chacha20poly1305_encrypt(view_key, &nonce, &amount.to_be_bytes::<32>())?;
        let mut blob = version.to_be_bytes().to_vec();
        blob.extend_from_slice(&ct);
        debug_assert_eq!(
            blob.len(),
            AMOUNT_BLOB_LEN,
            "amount blob must be a fixed {AMOUNT_BLOB_LEN} bytes"
        );
        Ok(blob)
    }

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
