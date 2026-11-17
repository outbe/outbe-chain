use super::*;

impl DkgSession {
    pub fn player_ingest(
        &mut self,
        dealer: PubKey,
        pub_msg: CeremonyPubMsg,
        encrypted_share: &EncryptedShare,
    ) -> Result<Option<CeremonyAck>> {
        let priv_bytes = decrypt_share(&self.enc_secret, encrypted_share)?;
        let mut reader: &[u8] = priv_bytes.as_ref();
        let priv_msg = DealerPrivMsg::read(&mut reader)
            .map_err(|e| dkg_err(DkgOperation::DecodeSealedShare, e))?;
        let player = self
            .player
            .as_mut()
            .ok_or(DkgSeamOrderError::PlayerAlreadyFinalized)?;
        Ok(player
            .dealer_message::<N3f1>(dealer, pub_msg, priv_msg)
            .ok()
            .flatten())
    }

    pub fn dealer_receive_ack(&mut self, player: PubKey, ack: CeremonyAck) -> Result<()> {
        let dealer = self
            .dealer
            .as_mut()
            .ok_or(DkgSeamOrderError::DealerUnavailable)?;
        dealer
            .receive_player_ack(player, ack)
            .map_err(|e| dkg_err(DkgOperation::DealerReceiveAck, e))
    }

    pub fn dealer_finalize(&mut self) -> Result<CeremonySignedLog> {
        let dealer = self
            .dealer
            .take()
            .ok_or(DkgSeamOrderError::DealerUnavailable)?;
        Ok(dealer.finalize::<N3f1>())
    }

    pub(crate) fn player_finalize(
        &mut self,
        logs: Vec<(PubKey, CeremonyLog)>,
    ) -> Result<(CeremonyOutput, CeremonyShare)> {
        let player = self
            .player
            .take()
            .ok_or(DkgSeamOrderError::PlayerAlreadyFinalized)?;
        let mut finalize_logs = Logs::<Variant, PubKey, N3f1>::new(self.info.clone());
        for (dealer_pk, log) in logs {
            finalize_logs.record(dealer_pk, log);
        }
        let (output, share) = player
            .finalize::<N3f1, Batch>(
                &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
                finalize_logs,
                &Sequential,
            )
            .map_err(|e| dkg_err(DkgOperation::PlayerFinalize, e))?;
        // Retain the group output + this party's share resident so Seam F
        // (offer-key partial-sign + group-sig recovery) can run on a later
        // request without re-finalizing. Both zeroize when the session drops.
        self.group_output = Some(output.clone());
        self.recovered_share = Some(share.clone());
        Ok((output, share))
    }
}
