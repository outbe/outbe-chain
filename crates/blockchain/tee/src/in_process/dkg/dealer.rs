use super::*;

impl DkgSession {
    pub fn start_dealer(
        &mut self,
        previous_share: Option<CeremonyShare>,
    ) -> Result<(CeremonyPubMsg, Vec<(PubKey, EncryptedShare)>)> {
        if self.dealer.is_some() {
            return Err(DkgSeamOrderError::StartDealerCalledTwice.into());
        }
        let (dealer, pub_msg, priv_msgs) = Dealer::<Variant, PrivKey>::start::<N3f1>(
            rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            self.info.clone(),
            self.signing_key.clone(),
            previous_share,
        )
        .map_err(|e| dkg_err(DkgOperation::DealerStart, e))?;
        self.dealer = Some(dealer);

        let mut sealed = Vec::with_capacity(priv_msgs.len());
        for (player_pk, priv_msg) in priv_msgs {
            let enc_pub = self
                .recipient_enc_keys
                .get(&player_pk)
                .ok_or(DkgError::MissingRecipientKey)?;
            // Serialize the protocol-secret share and seal it to the recipient.
            // The plaintext exists only in this enclave process. The host gets
            // ciphertext.
            let plaintext = encode_secret(&priv_msg);
            let blob = encrypt_share(enc_pub, plaintext.as_ref())?;
            sealed.push((player_pk, blob));
        }
        Ok((pub_msg, sealed))
    }
}
