use super::*;

impl DkgSession {
    pub fn tribute_offer_partials_sealed(&self) -> Result<Vec<(PubKey, EncryptedShare)>> {
        let share = self
            .recovered_share
            .as_ref()
            .ok_or(DkgSeamOrderError::ShareUnavailable)?;
        let partial =
            threshold::sign_message::<Variant>(share, TEE_OFFER_NAMESPACE, TEE_OFFER_MESSAGE);
        // The plaintext partial exists only in this enclave. This code seals it to
        // each recipient (including self) so the host only ever relays ciphertext.
        let partial_bytes = encode_secret(&partial);
        self.recipient_enc_keys
            .iter()
            .map(|(recipient, public)| {
                Ok((
                    recipient.clone(),
                    encrypt_share(public, partial_bytes.as_ref())?,
                ))
            })
            .collect()
    }

    pub(crate) fn recover_tribute_offer_material(
        &self,
        sealed_for_me: &[Vec<u8>],
        chain_id: B256,
        tribute_offer_epoch: u64,
    ) -> Result<crate::resident::ResidentOfferMaterial> {
        let output = self
            .group_output
            .as_ref()
            .ok_or(DkgSeamOrderError::OutputUnavailable)?;
        let mut partials: Vec<PartialSignature<Variant>> = Vec::with_capacity(sealed_for_me.len());
        for blob_bytes in sealed_for_me {
            let blob = share_cipher::decode(blob_bytes)?;
            let plaintext = decrypt_share(&self.enc_secret, &blob)?;
            let mut reader: &[u8] = plaintext.as_ref();
            let partial = PartialSignature::<Variant>::read(&mut reader)
                .map_err(|e| dkg_err(DkgOperation::DecodeOfferPartial, e))?;
            if !reader.is_empty() {
                return Err(DkgError::TrailingBytes.into());
            }
            threshold::verify_message::<Variant>(
                output.public(),
                TEE_OFFER_NAMESPACE,
                TEE_OFFER_MESSAGE,
                &partial,
            )
            .map_err(|error| dkg_err(DkgOperation::VerifyOfferPartial, error))?;
            partials.push(partial);
        }
        let group_sig =
            threshold::recover::<Variant, _>(output.public(), partials.iter(), &Sequential)
                .map_err(|e| dkg_err(DkgOperation::RecoverOfferGroupSignature, e))?;
        // `sigma` (the encoded group signature) is retained resident by the caller
        // so the exact permanent key can be delivered through
        // the deterministic one-time registry onboarding artifact.
        let sigma = encode_secret(&group_sig);
        Ok(
            crate::resident::ResidentOfferMaterial::from_group_signature(
                sigma,
                chain_id,
                0,
                tribute_offer_epoch,
            )?,
        )
    }

    pub fn group_public_key_bytes(&self) -> Result<Vec<u8>> {
        let output = self
            .group_output
            .as_ref()
            .ok_or(DkgSeamOrderError::OutputUnavailable)?;
        Ok(output.public().public().encode().to_vec())
    }
}
