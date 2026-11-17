//! Opaque keys: derived material never crosses the crate boundary as raw bytes.
use super::{
    crypto_ops::{derive, mac, nonce, open, seal, shared_legacy, verify_mac},
    Result, TeeCryptoError,
};
use alloy_primitives::Address;
use zeroize::Zeroizing;

pub struct StateKeyHandle(pub(crate) Zeroizing<[u8; 32]>);
pub struct OfferKeyHandle(pub(crate) StateKeyHandle);
pub struct AccountKeysHandle {
    pub(crate) view: StateKeyHandle,
    pub(crate) modify: Zeroizing<[u8; 32]>,
    account: Address,
}
impl std::fmt::Debug for StateKeyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StateKeyHandle([REDACTED])")
    }
}
impl std::fmt::Debug for OfferKeyHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OfferKeyHandle([REDACTED])")
    }
}
impl std::fmt::Debug for AccountKeysHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountKeysHandle([REDACTED])")
    }
}

impl StateKeyHandle {
    /// HKDF with this key as salt, for hierarchical ledger derivation.
    pub fn derive_subkey(&self, context: &[u8], info: &[u8]) -> Result<Self> {
        Ok(Self(derive(self.0.as_ref(), context, info)?))
    }
    /// HKDF with this key as IKM, for digest-bound NOD/Tribute record keys.
    pub fn derive_with_salt(&self, salt: &[u8], info: &[u8]) -> Result<Self> {
        Ok(Self(derive(salt, self.0.as_ref(), info)?))
    }
    pub fn compute_mac(&self, label: &[u8], message: &[u8]) -> [u8; 32] {
        mac(&self.0, label, message)
    }
    pub fn verify_mac(&self, label: &[u8], message: &[u8], tag: &[u8; 32]) -> bool {
        verify_mac(&self.0, label, message, tag)
    }
    /// AEAD with a caller-provided public nonce; caller owns nonce uniqueness.
    pub fn aead_encrypt(&self, nonce: &[u8; 12], plaintext: &[u8]) -> Result<Vec<u8>> {
        seal(&self.0, nonce, plaintext)
    }
    pub fn aead_decrypt(&self, nonce: &[u8; 12], ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        open(&self.0, nonce, ciphertext)
    }
    pub fn slot_encrypt(
        &self,
        nonce_info: &[u8],
        account: Address,
        field: u8,
        previous_version: u64,
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let version = previous_version.saturating_add(1);
        let context =
            crate::tee_decrypt::counter_slot_nonce_context(account.as_slice(), field, version);
        let nonce = nonce(&self.0, &context, nonce_info)?;
        let mut out = version.to_be_bytes().to_vec();
        out.extend_from_slice(&seal(&self.0, &nonce, plaintext)?);
        Ok(out)
    }
    pub fn slot_decrypt(
        &self,
        nonce_info: &[u8],
        account: Address,
        field: u8,
        blob: &[u8],
    ) -> Result<(u64, Zeroizing<Vec<u8>>)> {
        if blob.is_empty() {
            return Ok((0, Zeroizing::new(Vec::new())));
        }
        let version = blob
            .get(..8)
            .and_then(|v| v.try_into().ok())
            .map(u64::from_be_bytes)
            .ok_or(TeeCryptoError::InvalidEnvelope)?;
        let context =
            crate::tee_decrypt::counter_slot_nonce_context(account.as_slice(), field, version);
        let nonce = nonce(&self.0, &context, nonce_info)?;
        Ok((version, open(&self.0, &nonce, &blob[8..])?))
    }
    #[cfg(any(test, feature = "test-utils"))]
    pub fn for_test(key: [u8; 32]) -> Self {
        Self(Zeroizing::new(key))
    }
}

impl AccountKeysHandle {
    #[cfg(any(test, feature = "test-utils"))]
    pub fn for_test(account: Address, view: [u8; 32], modify: [u8; 32]) -> Self {
        Self {
            account,
            view: StateKeyHandle::for_test(view),
            modify: Zeroizing::new(modify),
        }
    }

    pub fn account(&self) -> Address {
        self.account
    }
    pub fn view_key_handle(&self) -> &StateKeyHandle {
        &self.view
    }
}

pub fn derive_account_keys_handle(
    state: &StateKeyHandle,
    account: Address,
    view_label: &[u8],
    modify_label: &[u8],
) -> Result<AccountKeysHandle> {
    Ok(AccountKeysHandle {
        view: state.derive_subkey(account.as_slice(), view_label)?,
        modify: derive(state.0.as_ref(), account.as_slice(), modify_label)?,
        account,
    })
}
pub fn slot_encrypt(
    handle: &AccountKeysHandle,
    nonce_info: &[u8],
    (account, field): (Address, u8),
    previous_version: u64,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    if account != handle.account {
        return Err(TeeCryptoError::WrongOwner);
    }
    handle
        .view
        .slot_encrypt(nonce_info, account, field, previous_version, plaintext)
}
pub fn slot_decrypt(
    handle: &AccountKeysHandle,
    nonce_info: &[u8],
    account: Address,
    field: u8,
    blob: &[u8],
) -> Result<(u64, Zeroizing<Vec<u8>>)> {
    if account != handle.account {
        return Err(TeeCryptoError::WrongOwner);
    }
    handle.view.slot_decrypt(nonce_info, account, field, blob)
}
pub fn compute_modify_mac(
    handle: &AccountKeysHandle,
    modify_tag: &[u8],
    message: &[u8],
) -> [u8; 32] {
    mac(&handle.modify, modify_tag, message)
}
pub fn verify_modify_mac(
    handle: &AccountKeysHandle,
    modify_tag: &[u8],
    message: &[u8],
    tag: &[u8; 32],
) -> bool {
    verify_mac(&handle.modify, modify_tag, message, tag)
}

impl OfferKeyHandle {
    /// Import legacy sidecar material without permitting raw-key export; removed with the sidecar.
    pub fn from_legacy_secret(secret: Zeroizing<[u8; 32]>) -> Self {
        Self(StateKeyHandle(secret))
    }

    pub fn public(&self) -> [u8; 32] {
        super::crypto_ops::x25519_public(&self.0 .0)
    }
    pub fn key_handle(&self) -> &StateKeyHandle {
        &self.0
    }
    pub fn ecdhe_decrypt(
        &self,
        salt: &[u8],
        info: &[u8],
        peer: &[u8; 32],
        nonce: &[u8; 12],
        ciphertext: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>> {
        let shared = shared_legacy(&self.0 .0, peer);
        let key = derive(salt, shared.as_ref(), info)?;
        open(&key, nonce, ciphertext)
    }
    #[cfg(any(test, feature = "test-utils"))]
    pub fn for_test(secret: [u8; 32]) -> Self {
        Self(StateKeyHandle::for_test(secret))
    }
}
