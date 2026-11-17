//! Resident DKG seams A–F: deal, ingest, acknowledge, finalize dealer/player,
//! then recover the fixed offer root. The actor relays only public commitments,
//! acknowledgements, logs and ciphertext. Shares, partial signatures and the
//! reconstructed root stay in zeroizing resident ownership, never in host output.
mod dealer;
mod error;
mod finalize;
mod player;
mod secret_partial;
mod share_cipher;
mod store;
pub use error::{DkgError, DkgOperation, DkgSeamOrderError};
pub use store::DkgSessionStore;

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use alloy_primitives::{keccak256, B256};
use commonware_codec::{Encode as _, Read as _, ReadExt as _};
use commonware_cryptography::bls12381::{
    self,
    dkg::feldman_desmedt::{
        Dealer, DealerLog, DealerPrivMsg, DealerPubMsg, Info, Logs, Output, Player, PlayerAck,
        SignedDealerLog,
    },
    primitives::{
        group::Share,
        ops::threshold,
        sharing::Mode,
        variant::{MinSig, PartialSignature},
    },
    Batch,
};
use commonware_cryptography::Signer as _;
use commonware_parallel::Sequential;
use commonware_utils::{ordered::Set, N3f1, TryCollect as _};
use zeroize::Zeroizing;

use self::share_cipher::{decrypt_share, encrypt_share, EncryptedShare};
use super::{InProcessError, Result};
use crate::resident::crypto_ops::x25519_public;
use InProcessError as TeeError;

type Variant = MinSig;
/// Public BLS identity of a ceremony participant.
pub type PubKey = bls12381::PublicKey;
/// Internal signing-key type used by the resident ceremony.
pub type PrivKey = bls12381::PrivateKey;

/// Public ceremony transcript domain and participant sets.
pub type CeremonyInfo = Info<Variant, PubKey>;
/// Public dealer commitment broadcast to every participant.
pub type CeremonyPubMsg = DealerPubMsg<Variant>;
/// Public acknowledgement of a verified encrypted dealing.
pub type CeremonyAck = PlayerAck<PubKey>;
/// Dealer-signed public dealing log.
pub type CeremonySignedLog = SignedDealerLog<Variant, PrivKey>;
/// Verified public dealer log used during player finalization.
pub type CeremonyLog = DealerLog<Variant, PubKey>;
/// Public group polynomial and participant binding.
pub type CeremonyOutput = Output<Variant, PubKey>;
/// Internal threshold share; its scalar zeroizes on drop.
pub type CeremonyShare = Share;

/// Opaque ceremony identifier supplied by the protocol actor.
pub type CeremonyKey = [u8; 32];

/// Encoded public commitment plus per-recipient encrypted shares.
pub type EncodedDealing = (Vec<u8>, Vec<(Vec<u8>, Vec<u8>)>);

/// Ceremony namespace, separated from consensus DKG.
pub const TEE_DKG_NAMESPACE: &[u8] = b"outbe-tee-dkg";

/// Threshold offer-signature namespace, separate from ceremony messages.
pub const TEE_OFFER_NAMESPACE: &[u8] = b"outbe-tee-offer";
/// Frozen offer-root signing message; never includes caller-selected data.
pub const TEE_OFFER_MESSAGE: &[u8] = b"outbe/tee/offer/v1";

fn dkg_err(context: DkgOperation, _error: impl Sized) -> TeeError {
    DkgError::Operation { operation: context }.into()
}

/// One ceremony's resident dealer/player state; secret shares never leave through its public seams.
pub struct DkgSession {
    info: CeremonyInfo,
    signing_key: PrivKey,
    enc_secret: Zeroizing<[u8; 32]>,
    recipient_enc_keys: BTreeMap<PubKey, [u8; 32]>,
    dealer: Option<Dealer<Variant, PrivKey>>,
    player: Option<Player<Variant, PrivKey>>,
    recovered_share: Option<CeremonyShare>,
    group_output: Option<CeremonyOutput>,
}

impl DkgSession {
    /// Open resident player state with crate-owned signing/encryption secrets.
    pub(crate) fn new(
        info: CeremonyInfo,
        signing_key: PrivKey,
        enc_secret: &[u8; 32],
        recipient_enc_keys: BTreeMap<PubKey, [u8; 32]>,
    ) -> Result<Self> {
        let player = Player::<Variant, PrivKey>::new(info.clone(), signing_key.clone())
            .map_err(|e| dkg_err(DkgOperation::PlayerNew, e))?;
        Ok(Self {
            info,
            signing_key,
            enc_secret: Zeroizing::new(*enc_secret),
            recipient_enc_keys,
            dealer: None,
            player: Some(player),
            recovered_share: None,
            group_output: None,
        })
    }

    /// Return this session's public BLS participant identity.
    pub fn participant_pubkey(&self) -> PubKey {
        self.signing_key.public_key()
    }

    /// Return this session's public share-encryption key.
    pub fn enc_public(&self) -> [u8; 32] {
        x25519_public(&self.enc_secret)
    }
}

/// Verify a public signed dealer log against the exact ceremony transcript.
pub fn verify_dealer_log(
    info: &CeremonyInfo,
    signed: CeremonySignedLog,
) -> Option<(PubKey, CeremonyLog)> {
    signed.check(info)
}

fn decode_pubkey(bytes: &[u8]) -> Result<PubKey> {
    let mut reader: &[u8] = bytes;
    PubKey::read(&mut reader).map_err(|e| dkg_err(DkgOperation::DecodeBlsPubkey, e))
}

/// Build one canonical founding transcript from the participant BLS public keys.
pub fn build_ceremony_info(
    round: u64,
    participant_bls: &[Vec<u8>],
) -> Result<(CeremonyInfo, Vec<PubKey>)> {
    let mut pubkeys: Vec<PubKey> = participant_bls
        .iter()
        .map(|b| decode_pubkey(b))
        .collect::<Result<_>>()?;
    pubkeys.sort_by_key(|k| k.encode());
    let participants: Set<PubKey> = pubkeys
        .iter()
        .cloned()
        .try_collect()
        .map_err(|e| dkg_err(DkgOperation::BuildParticipantSet, e))?;
    let info = Info::<Variant, PubKey>::new::<N3f1>(
        TEE_DKG_NAMESPACE,
        round,
        None,
        Mode::NonZeroCounter,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        participants.clone(),
        participants,
    )
    .map_err(|e| dkg_err(DkgOperation::BuildCeremonyInfo, e))?;
    Ok((info, pubkeys))
}

impl DkgSession {
    fn max_players(&self) -> Result<NonZeroU32> {
        let n = u32::try_from(self.recipient_enc_keys.len())
            .map_err(|_| DkgError::ParticipantCountOverflow)?;
        NonZeroU32::new(n).ok_or_else(|| DkgError::EmptyParticipantSet.into())
    }

    /// Seam A wire adapter: encode public commitment and sealed shares.
    pub fn start_dealer_encoded(&mut self) -> Result<EncodedDealing> {
        let (pub_msg, sealed) = self.start_dealer(None)?;
        let shares = sealed
            .into_iter()
            .map(|(pk, blob)| (pk.encode().to_vec(), blob.to_wire_bytes()))
            .collect();
        Ok((pub_msg.encode().to_vec(), shares))
    }

    /// Seam B wire adapter: open and verify a sealed share, returning an encoded public ack.
    pub fn player_ingest_encoded(
        &mut self,
        dealer_bls: &[u8],
        pub_msg: &[u8],
        sealed_share: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let dealer = decode_pubkey(dealer_bls)?;
        let max = self.max_players()?;
        let mut reader: &[u8] = pub_msg;
        let pub_msg = DealerPubMsg::<Variant>::read_cfg(&mut reader, &max)
            .map_err(|e| dkg_err(DkgOperation::DecodePubMsg, e))?;
        let blob = share_cipher::decode(sealed_share)?;
        let ack = self.player_ingest(dealer, pub_msg, &blob)?;
        Ok(ack.map(|a| a.encode().to_vec()))
    }

    /// Seam C wire adapter: decode and count a public acknowledgement.
    pub fn dealer_receive_ack_encoded(&mut self, player_bls: &[u8], ack: &[u8]) -> Result<()> {
        let player = decode_pubkey(player_bls)?;
        let mut reader: &[u8] = ack;
        let ack = PlayerAck::<PubKey>::read(&mut reader)
            .map_err(|e| dkg_err(DkgOperation::DecodeAck, e))?;
        self.dealer_receive_ack(player, ack)
    }

    /// Seam D wire adapter: encode the signed public dealer log.
    pub fn dealer_finalize_encoded(&mut self) -> Result<Vec<u8>> {
        Ok(self.dealer_finalize()?.encode().to_vec())
    }

    /// Seam E wire adapter: require all founding dealers and return only public output and share commitment.
    pub fn player_finalize_encoded(&mut self, signed_logs: &[Vec<u8>]) -> Result<(Vec<u8>, B256)> {
        let max = self.max_players()?;
        let mut logs = Vec::with_capacity(signed_logs.len());
        let mut distinct_dealers = std::collections::BTreeSet::new();
        for bytes in signed_logs {
            let mut reader: &[u8] = bytes;
            let signed = SignedDealerLog::<Variant, PrivKey>::read_cfg(&mut reader, &max)
                .map_err(|e| dkg_err(DkgOperation::DecodeSignedLog, e))?;
            let (pk, log) = verify_dealer_log(&self.info, signed)
                .ok_or(DkgError::DealerLogVerificationFailed)?;
            distinct_dealers.insert(commonware_codec::Encode::encode(&pk).to_vec());
            logs.push((pk, log));
        }
        // Initial DKG: the group key is the sum of EVERY dealer's contribution, so an
        // incomplete dealer set yields a DIFFERENT group key. Require the verified
        // distinct dealers to cover the full committee. Otherwise an untrusted host
        // that feeds different dealer subsets to different enclaves would diverge the
        // derived offer key across validators. (Each verified dealer is a committee
        // member, so a full distinct count equals full coverage. Scheduled rolling
        // reshare is a separate consensus ceremony. This founding path is all-n.)
        let expected = max.get() as usize;
        if distinct_dealers.len() != expected {
            return Err(DkgError::IncompleteDealerSet {
                actual: distinct_dealers.len(),
                expected,
            }
            .into());
        }
        let (output, share) = self.player_finalize(logs)?;
        Ok((
            output.encode().to_vec(),
            keccak256(encode_secret(&share).as_slice()),
        ))
    }
}

impl std::fmt::Debug for DkgSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DkgSession([REDACTED])")
    }
}

// Write directly into zeroizing ownership, without an intermediate immutable Bytes copy.
fn encode_secret(
    value: &(impl commonware_codec::Write + commonware_codec::EncodeSize),
) -> Zeroizing<Vec<u8>> {
    let mut encoded = Zeroizing::new(Vec::with_capacity(value.encode_size()));
    value.write(&mut *encoded);
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    struct SecretBearingFailure;
    impl std::fmt::Debug for SecretBearingFailure {
        fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            panic!("secret-bearing failures must never be formatted")
        }
    }
    #[test]
    fn in_process_dkg_errors_and_debug_do_not_format_secrets() {
        let error = dkg_err(DkgOperation::PlayerNew, SecretBearingFailure);
        assert!(matches!(
            error,
            InProcessError::Dkg(DkgError::Operation {
                operation: DkgOperation::PlayerNew
            })
        ));
        assert_eq!(error.to_string(), "DKG rejected operation");
        assert_eq!(
            format!("{:?}", DkgSessionStore::new()),
            "DkgSessionStore([REDACTED])"
        );
    }
}
