//! Node-local durable recovery for DKG dealer and player state.
//!
//! The wire protocol is unchanged. This module persists enough local input to
//! reconstruct the exact in-memory `Dealer` and `Player` state after restart.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use alloy_primitives::{keccak256, B256};
use commonware_codec::{Encode, Read as _, ReadExt as _};
use commonware_cryptography::bls12381::{
    self,
    dkg::feldman_desmedt::{DealerPrivMsg, DealerPubMsg, Info, Player, PlayerAck},
    primitives::variant::MinSig,
};
use commonware_utils::N3f1;
use eyre::Result;

use super::wire::DkgCeremonyId;
use crate::errors::DkgError;

pub(crate) const DKG_DEALER_RETRY_FILE: &str = "dkg_dealer_retry.hex";
pub(crate) const DKG_PLAYER_RETRY_FILE: &str = "dkg_player_retry.hex";
const DKG_DEALER_RETRY_MAGIC: &[u8; 8] = b"ODKGDR01";
const DKG_PLAYER_RETRY_MAGIC: &[u8; 8] = b"ODKGPR01";
const DKG_DEALER_RETRY_HEADER_LEN: usize = 8 + 8 + 32 + 32 + 4;
const DKG_PLAYER_RETRY_HEADER_LEN: usize = 8 + 8 + 32 + 4;
const MAX_DKG_PLAYER_RETRY_BYTES: usize = 64 * 1024 * 1024;
const MAX_DKG_PLAYER_RETRY_STORED_BYTES: u64 = (MAX_DKG_PLAYER_RETRY_BYTES as u64) * 2 + 4096;

#[derive(Clone)]
pub(super) struct DkgDealerRetrySnapshot {
    pub(super) ceremony_id: DkgCeremonyId,
    pub(super) seed: zeroize::Zeroizing<[u8; 32]>,
    pub(super) accepted_acks: BTreeMap<bls12381::PublicKey, PlayerAck<bls12381::PublicKey>>,
}

impl std::fmt::Debug for DkgDealerRetrySnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DkgDealerRetrySnapshot")
            .field("ceremony_id", &self.ceremony_id)
            .field("seed", &"<redacted>")
            .field("accepted_acks", &self.accepted_acks)
            .finish()
    }
}

/// Load the ceremony-scoped dealer seed or save a new one before networking.
pub(super) fn load_or_create_dealer_snapshot(
    retry_store: Option<&DkgRetryStore>,
    ceremony_id: DkgCeremonyId,
    round: u64,
    restored_message: &'static str,
) -> Result<DkgDealerRetrySnapshot> {
    if let Some(store) = retry_store {
        if let Some(snapshot) = store.load_dealer(ceremony_id)? {
            tracing::info!(round, "{restored_message}");
            return Ok(snapshot);
        }
    }
    use rand_commonware::Rng as _;
    let mut seed = zeroize::Zeroizing::new([0u8; 32]);
    rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng).fill_bytes(seed.as_mut());
    let snapshot = DkgDealerRetrySnapshot {
        ceremony_id,
        seed,
        accepted_acks: BTreeMap::new(),
    };
    if let Some(store) = retry_store {
        store.save_dealer(&snapshot)?;
    }
    Ok(snapshot)
}

/// Record a newly accepted ACK before the actor advances delivery or sealing.
pub(super) fn persist_dealer_ack(
    snapshot: Option<&mut DkgDealerRetrySnapshot>,
    retry_store: Option<&DkgRetryStore>,
    player: bls12381::PublicKey,
    ack: PlayerAck<bls12381::PublicKey>,
) -> Result<()> {
    let Some(snapshot) = snapshot else {
        return Ok(());
    };
    snapshot.accepted_acks.insert(player, ack);
    if let Some(store) = retry_store {
        store.save_dealer(snapshot)?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct AcceptedPlayerDealing {
    pub_msg: DealerPubMsg<MinSig>,
    priv_msg: DealerPrivMsg,
    ack: PlayerAck<bls12381::PublicKey>,
}

#[derive(Clone, Debug)]
pub(super) struct DkgPlayerRetrySnapshot {
    ceremony_id: DkgCeremonyId,
    accepted_dealings: BTreeMap<bls12381::PublicKey, AcceptedPlayerDealing>,
}

impl DkgPlayerRetrySnapshot {
    pub(super) fn empty(ceremony_id: DkgCeremonyId) -> Self {
        Self {
            ceremony_id,
            accepted_dealings: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum PlayerBundleAction {
    SendAck(PlayerAck<bls12381::PublicKey>),
    DuplicateAck(PlayerAck<bls12381::PublicKey>),
    Invalid,
    Equivocation { previous: B256, received: B256 },
}

/// Durable DKG transcript storage for both local protocol roles.
///
/// Dealer and player files remain separate so the existing dealer format and
/// its operational recovery behavior stay byte-for-byte compatible.
#[derive(Clone, Debug)]
pub struct DkgRetryStore {
    dealer_path: PathBuf,
    player_path: PathBuf,
    backend: crate::bls::KeyBackend,
}

impl DkgRetryStore {
    #[must_use]
    pub fn in_keys_dir(keys_dir: &Path, backend: crate::bls::KeyBackend) -> Self {
        Self {
            dealer_path: keys_dir.join(DKG_DEALER_RETRY_FILE),
            player_path: keys_dir.join(DKG_PLAYER_RETRY_FILE),
            backend,
        }
    }

    pub(super) fn load_dealer(
        &self,
        ceremony_id: DkgCeremonyId,
    ) -> Result<Option<DkgDealerRetrySnapshot>> {
        if !self.dealer_path.exists() {
            return Ok(None);
        }
        let bytes = zeroize::Zeroizing::new(
            crate::bls::load_raw(&self.dealer_path, &self.backend)
                .map_err(DkgError::load_dealer_retry_snapshot)?,
        );
        let snapshot = decode_dealer_retry_snapshot(&bytes)?;
        Ok((snapshot.ceremony_id == ceremony_id).then_some(snapshot))
    }

    pub(super) fn save_dealer(&self, snapshot: &DkgDealerRetrySnapshot) -> Result<()> {
        let encoded = encode_dealer_retry_snapshot(snapshot)?;
        save_snapshot(&self.dealer_path, &encoded, &self.backend, "dealer")
    }

    fn load_player(
        &self,
        ceremony_id: DkgCeremonyId,
        max_players: NonZeroU32,
    ) -> Result<Option<DkgPlayerRetrySnapshot>> {
        if !self.player_path.exists() {
            return Ok(None);
        }
        let stored_len = std::fs::metadata(&self.player_path)
            .map_err(DkgError::stat_player_retry_snapshot)?
            .len();
        if stored_len > MAX_DKG_PLAYER_RETRY_STORED_BYTES {
            return Err(DkgError::StoredPlayerRetrySnapshotTooLarge {
                max_bytes: MAX_DKG_PLAYER_RETRY_STORED_BYTES,
            }
            .into());
        }
        let bytes = crate::bls::load_raw(&self.player_path, &self.backend)
            .map_err(DkgError::load_player_retry_snapshot)?;
        let snapshot = decode_player_retry_snapshot(&bytes, max_players)?;
        Ok((snapshot.ceremony_id == ceremony_id).then_some(snapshot))
    }

    fn save_player(&self, snapshot: &DkgPlayerRetrySnapshot) -> Result<()> {
        save_snapshot(
            &self.player_path,
            &encode_player_retry_snapshot(snapshot)?,
            &self.backend,
            "player",
        )
    }

    /// Remove both transcripts only after the matching DKG output has been
    /// durably promoted at its canonical activation boundary.
    pub fn clear(&self) -> Result<()> {
        crate::bls::remove_raw(&self.dealer_path, &self.backend)
            .map_err(DkgError::clear_dealer_retry_snapshot)?;
        crate::bls::remove_raw(&self.player_path, &self.backend)
            .map_err(DkgError::clear_player_retry_snapshot)
    }
}

pub(super) fn restore_player(
    info: Info<MinSig, bls12381::PublicKey>,
    signing_key: bls12381::PrivateKey,
    ceremony_id: DkgCeremonyId,
    max_players: NonZeroU32,
    retry_store: Option<&DkgRetryStore>,
) -> Result<(Player<MinSig, bls12381::PrivateKey>, DkgPlayerRetrySnapshot)> {
    let mut player = Player::<MinSig, bls12381::PrivateKey>::new(info, signing_key)
        .map_err(DkgError::create_player)?;
    let snapshot = match retry_store {
        Some(store) => store
            .load_player(ceremony_id, max_players)?
            .unwrap_or_else(|| DkgPlayerRetrySnapshot::empty(ceremony_id)),
        None => DkgPlayerRetrySnapshot::empty(ceremony_id),
    };

    for (dealer, accepted) in &snapshot.accepted_dealings {
        let replayed_ack = player
            .dealer_message::<N3f1>(
                dealer.clone(),
                accepted.pub_msg.clone(),
                accepted.priv_msg.clone(),
            )
            .map_err(DkgError::invalid_persisted_player_dealing)?
            .ok_or(DkgError::PersistedPlayerDealingProducedNoAck)?;
        if replayed_ack.encode() != accepted.ack.encode() {
            return Err(DkgError::PersistedPlayerDealingDifferentAck.into());
        }
    }

    Ok((player, snapshot))
}

/// Owned dealer identity and transcript received by a recovering player.
pub(super) struct PlayerDealerBundle {
    pub(super) dealer: bls12381::PublicKey,
    pub(super) pub_msg: DealerPubMsg<MinSig>,
    pub(super) priv_msg: DealerPrivMsg,
}

pub(super) fn handle_player_bundle(
    player: &mut Player<MinSig, bls12381::PrivateKey>,
    snapshot: &mut DkgPlayerRetrySnapshot,
    retry_store: Option<&DkgRetryStore>,
    bundle: PlayerDealerBundle,
) -> Result<PlayerBundleAction> {
    let PlayerDealerBundle {
        dealer,
        pub_msg,
        priv_msg,
    } = bundle;
    let received = dealer_bundle_hash(&pub_msg, &priv_msg);
    if let Some(accepted) = snapshot.accepted_dealings.get(&dealer) {
        let previous = dealer_bundle_hash(&accepted.pub_msg, &accepted.priv_msg);
        if previous == received
            && accepted.pub_msg.encode() == pub_msg.encode()
            && accepted.priv_msg.encode() == priv_msg.encode()
        {
            return Ok(PlayerBundleAction::DuplicateAck(accepted.ack.clone()));
        }
        return Ok(PlayerBundleAction::Equivocation { previous, received });
    }

    let Ok(Some(ack)) =
        player.dealer_message::<N3f1>(dealer.clone(), pub_msg.clone(), priv_msg.clone())
    else {
        return Ok(PlayerBundleAction::Invalid);
    };
    snapshot.accepted_dealings.insert(
        dealer,
        AcceptedPlayerDealing {
            pub_msg,
            priv_msg,
            ack: ack.clone(),
        },
    );
    if let Some(store) = retry_store {
        store.save_player(snapshot)?;
    }
    Ok(PlayerBundleAction::SendAck(ack))
}

fn dealer_bundle_hash(pub_msg: &DealerPubMsg<MinSig>, priv_msg: &DealerPrivMsg) -> B256 {
    let pub_bytes = pub_msg.encode();
    let priv_bytes = priv_msg.encode();
    let mut bytes = Vec::with_capacity(8 + pub_bytes.len() + priv_bytes.len());
    bytes.extend_from_slice(&(pub_bytes.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&pub_bytes);
    bytes.extend_from_slice(&(priv_bytes.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&priv_bytes);
    keccak256(bytes)
}

fn save_snapshot(
    path: &Path,
    bytes: &[u8],
    backend: &crate::bls::KeyBackend,
    role: &str,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| DkgError::create_retry_dir(role, parent, error))?;
    }
    crate::bls::save_raw(path, bytes, backend)
        .map_err(|error| DkgError::persist_retry_snapshot(role, error))
}

pub(super) fn encode_dealer_retry_snapshot(
    snapshot: &DkgDealerRetrySnapshot,
) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    let ack_count: u32 = snapshot
        .accepted_acks
        .len()
        .try_into()
        .map_err(|_| DkgError::TooManyDealerRetryAcks)?;
    // ACK bytes contain no seed. Size the secret buffer once so the seed is
    // never copied into a discarded reallocation.
    let mut ack_bytes = Vec::new();
    for (player, ack) in &snapshot.accepted_acks {
        push_retry_snapshot_field(&mut ack_bytes, &player.encode())?;
        push_retry_snapshot_field(&mut ack_bytes, &ack.encode())?;
    }
    let total_len = DKG_DEALER_RETRY_HEADER_LEN + ack_bytes.len();
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(total_len));
    bytes.extend_from_slice(DKG_DEALER_RETRY_MAGIC);
    bytes.extend_from_slice(&snapshot.ceremony_id.round.to_be_bytes());
    bytes.extend_from_slice(snapshot.ceremony_id.info_hash.as_slice());
    bytes.extend_from_slice(snapshot.seed.as_ref());
    bytes.extend_from_slice(&ack_count.to_be_bytes());
    bytes.extend_from_slice(&ack_bytes);
    Ok(bytes)
}

pub(super) fn decode_dealer_retry_snapshot(bytes: &[u8]) -> Result<DkgDealerRetrySnapshot> {
    if bytes.len() < DKG_DEALER_RETRY_HEADER_LEN {
        return Err(DkgError::InvalidDealerRetrySnapshotLength {
            expected_min: DKG_DEALER_RETRY_HEADER_LEN,
            actual: bytes.len(),
        }
        .into());
    }
    if &bytes[..DKG_DEALER_RETRY_MAGIC.len()] != DKG_DEALER_RETRY_MAGIC {
        return Err(DkgError::InvalidDealerRetrySnapshotMagic.into());
    }
    let round = u64::from_be_bytes(bytes[8..16].try_into()?);
    let info_hash = B256::from(<[u8; 32]>::try_from(&bytes[16..48])?);
    let mut seed = zeroize::Zeroizing::new([0u8; 32]);
    seed.copy_from_slice(&bytes[48..80]);
    let ack_count = u32::from_be_bytes(bytes[80..84].try_into()?);
    ensure_entry_count(ack_count, "dealer ACK")?;
    let mut cursor = 84usize;
    let mut accepted_acks = BTreeMap::new();
    for _ in 0..ack_count {
        let player = decode_exact(
            take_retry_snapshot_field(bytes, &mut cursor, "player")?,
            |reader| bls12381::PublicKey::read(reader),
            "player key",
        )?;
        let ack = decode_exact(
            take_retry_snapshot_field(bytes, &mut cursor, "ACK")?,
            |reader| PlayerAck::<bls12381::PublicKey>::read(reader),
            "ACK",
        )?;
        if accepted_acks.insert(player, ack).is_some() {
            return Err(DkgError::DuplicatePlayerInRetrySnapshot.into());
        }
    }
    ensure_exact_end(bytes, cursor, "dealer")?;
    Ok(DkgDealerRetrySnapshot {
        ceremony_id: DkgCeremonyId { round, info_hash },
        seed,
        accepted_acks,
    })
}

fn encode_player_retry_snapshot(snapshot: &DkgPlayerRetrySnapshot) -> Result<Vec<u8>> {
    let dealing_count: u32 = snapshot
        .accepted_dealings
        .len()
        .try_into()
        .map_err(|_| DkgError::TooManyPlayerRetryDealings)?;
    let mut bytes = Vec::with_capacity(DKG_PLAYER_RETRY_HEADER_LEN);
    bytes.extend_from_slice(DKG_PLAYER_RETRY_MAGIC);
    bytes.extend_from_slice(&snapshot.ceremony_id.round.to_be_bytes());
    bytes.extend_from_slice(snapshot.ceremony_id.info_hash.as_slice());
    bytes.extend_from_slice(&dealing_count.to_be_bytes());
    for (dealer, accepted) in &snapshot.accepted_dealings {
        push_retry_snapshot_field(&mut bytes, &dealer.encode())?;
        push_retry_snapshot_field(&mut bytes, &accepted.pub_msg.encode())?;
        push_retry_snapshot_field(&mut bytes, &accepted.priv_msg.encode())?;
        push_retry_snapshot_field(&mut bytes, &accepted.ack.encode())?;
    }
    if bytes.len() > MAX_DKG_PLAYER_RETRY_BYTES {
        return Err(DkgError::PlayerRetrySnapshotTooLarge {
            max_bytes: MAX_DKG_PLAYER_RETRY_BYTES,
        }
        .into());
    }
    Ok(bytes)
}

fn decode_player_retry_snapshot(
    bytes: &[u8],
    max_players: NonZeroU32,
) -> Result<DkgPlayerRetrySnapshot> {
    if bytes.len() < DKG_PLAYER_RETRY_HEADER_LEN || bytes.len() > MAX_DKG_PLAYER_RETRY_BYTES {
        return Err(DkgError::InvalidPlayerRetrySnapshotLength {
            actual: bytes.len(),
        }
        .into());
    }
    if &bytes[..DKG_PLAYER_RETRY_MAGIC.len()] != DKG_PLAYER_RETRY_MAGIC {
        return Err(DkgError::InvalidPlayerRetrySnapshotMagic.into());
    }
    let round = u64::from_be_bytes(bytes[8..16].try_into()?);
    let info_hash = B256::from(<[u8; 32]>::try_from(&bytes[16..48])?);
    let dealing_count = u32::from_be_bytes(bytes[48..52].try_into()?);
    ensure_entry_count(dealing_count, "player dealing")?;
    let mut cursor = 52usize;
    let mut accepted_dealings = BTreeMap::new();
    for _ in 0..dealing_count {
        let dealer = decode_exact(
            take_retry_snapshot_field(bytes, &mut cursor, "dealer")?,
            |reader| bls12381::PublicKey::read(reader),
            "dealer key",
        )?;
        let pub_msg = decode_exact(
            take_retry_snapshot_field(bytes, &mut cursor, "public message")?,
            |reader| DealerPubMsg::<MinSig>::read_cfg(reader, &max_players),
            "public message",
        )?;
        let priv_msg = decode_exact(
            take_retry_snapshot_field(bytes, &mut cursor, "private message")?,
            |reader| DealerPrivMsg::read(reader),
            "private message",
        )?;
        let ack = decode_exact(
            take_retry_snapshot_field(bytes, &mut cursor, "ACK")?,
            |reader| PlayerAck::<bls12381::PublicKey>::read(reader),
            "ACK",
        )?;
        if accepted_dealings
            .insert(
                dealer,
                AcceptedPlayerDealing {
                    pub_msg,
                    priv_msg,
                    ack,
                },
            )
            .is_some()
        {
            return Err(DkgError::DuplicateDealerInPlayerRetrySnapshot.into());
        }
    }
    ensure_exact_end(bytes, cursor, "player")?;
    Ok(DkgPlayerRetrySnapshot {
        ceremony_id: DkgCeremonyId { round, info_hash },
        accepted_dealings,
    })
}

fn push_retry_snapshot_field(bytes: &mut Vec<u8>, field: &[u8]) -> Result<()> {
    let len: u32 = field
        .len()
        .try_into()
        .map_err(|_| DkgError::RetryFieldExceedsU32)?;
    bytes.extend_from_slice(&len.to_be_bytes());
    bytes.extend_from_slice(field);
    Ok(())
}

fn take_retry_snapshot_field<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    field: &str,
) -> Result<&'a [u8]> {
    let len_end = cursor
        .checked_add(4)
        .ok_or_else(|| DkgError::RetryFieldLengthOverflow {
            field: field.to_owned(),
        })?;
    let len_bytes =
        bytes
            .get(*cursor..len_end)
            .ok_or_else(|| DkgError::TruncatedRetryFieldLength {
                field: field.to_owned(),
            })?;
    let len = u32::from_be_bytes(len_bytes.try_into()?) as usize;
    let value_end = len_end
        .checked_add(len)
        .ok_or_else(|| DkgError::RetryFieldValueOverflow {
            field: field.to_owned(),
        })?;
    let value =
        bytes
            .get(len_end..value_end)
            .ok_or_else(|| DkgError::TruncatedRetryFieldValue {
                field: field.to_owned(),
            })?;
    *cursor = value_end;
    Ok(value)
}

fn decode_exact<T, E>(
    bytes: &[u8],
    decode: impl FnOnce(&mut &[u8]) -> std::result::Result<T, E>,
    field: &str,
) -> Result<T>
where
    E: std::fmt::Display,
{
    let mut reader = bytes;
    let value = decode(&mut reader).map_err(|error| DkgError::invalid_retry_field(field, error))?;
    if !reader.is_empty() {
        return Err(DkgError::TrailingRetryFieldBytes {
            field: field.to_owned(),
        }
        .into());
    }
    Ok(value)
}

fn ensure_entry_count(count: u32, kind: &str) -> Result<()> {
    if count > crate::bls::MAX_VALIDATORS {
        return Err(DkgError::RetryEntryCountExceedsValidatorLimit {
            kind: kind.to_owned(),
            count,
        }
        .into());
    }
    Ok(())
}

fn ensure_exact_end(bytes: &[u8], cursor: usize, role: &str) -> Result<()> {
    if cursor != bytes.len() {
        return Err(DkgError::TrailingRetrySnapshotBytes {
            role: role.to_owned(),
        }
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
