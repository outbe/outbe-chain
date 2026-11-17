//! V2 mutable envelopes use the existing owner-local opening contract.
use super::{
    crypto_ops::{derive, mac, nonce, seal},
    Result, StateKeyHandle, TeeCryptoError,
};
use crate::tee_decrypt::{tee_decrypt_v2_envelope, V2EnvelopeDomain};
use alloy_primitives::Address;
use zeroize::Zeroizing;

impl StateKeyHandle {
    pub fn encrypt_v2_envelope(
        &self,
        account: Address,
        domain: V2EnvelopeDomain<'_>,
        binding_tag: &[u8],
        (previous_version, binding_payload): (u64, &[u8]),
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let version = previous_version
            .checked_add(1)
            .ok_or(TeeCryptoError::InvalidEnvelope)?;
        let mut context = Zeroizing::new(account.as_slice().to_vec());
        context.extend_from_slice(domain.scope);
        context.extend_from_slice(&version.to_be_bytes());
        context.extend_from_slice(binding_payload);
        let binding = mac(&self.0, binding_tag, &context);
        context.truncate(20 + domain.scope.len() + 8);
        context.extend_from_slice(&binding);
        let key = derive(self.0.as_ref(), &context, domain.key_info)?;
        let nonce = nonce(&key, &context, domain.nonce_info)?;
        let mut out = version.to_be_bytes().to_vec();
        out.extend_from_slice(domain.marker);
        out.extend_from_slice(&binding);
        out.extend_from_slice(&seal(&key, &nonce, plaintext)?);
        Ok(out)
    }

    pub fn decrypt_v2_envelope(
        &self,
        account: Address,
        domain: V2EnvelopeDomain<'_>,
        blob: &[u8],
    ) -> Result<(u64, Zeroizing<Vec<u8>>)> {
        if blob.is_empty() {
            return Ok((0, Zeroizing::new(Vec::new())));
        }
        let opened = tee_decrypt_v2_envelope(&self.0, account, domain, blob)?;
        let version = u64::from_be_bytes(
            blob[..8]
                .try_into()
                .map_err(|_| TeeCryptoError::InvalidEnvelope)?,
        );
        Ok((version, Zeroizing::new(opened.as_slice().to_vec())))
    }
}
