use super::super::wire::DkgCeremonyId;
use super::super::wire::DkgMessage;
use alloy_primitives::Bytes;
use commonware_codec::Encode;
use commonware_codec::Read as _;
use commonware_cryptography::bls12381;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::observe;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerLog;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerPubMsg;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Info;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Logs;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::PlayerAck;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::SignedDealerLog;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_p2p::Recipients;
use commonware_p2p::Sender as P2pSender;
use commonware_parallel::Sequential;
use commonware_runtime::Clock;
use commonware_utils::N3f1;
use eyre::Result;
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::warn;

/// Encode and send one DKG message over the P2P sender. The single owner of the
/// `encode -> send -> "empty accepted-set is benign backpressure"` recipe, so every
/// DKG send interprets the result identically.
///
/// Returns `true` if at least one recipient accepted this attempt. An empty
/// accepted-set (commonware 2026.5.0 sync `Sender::send` returns the accepting
/// peers) means none accepted *this attempt* - benign rate-limit/backpressure,
/// recovered by the ceremony retry tick and peer pull - never a hard failure.
/// `#[must_use]`: callers must observe acceptance (typically to log backpressure).
#[must_use]
fn send_dkg_message(
    sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    recipients: Recipients<bls12381::PublicKey>,
    message: DkgMessage,
) -> bool {
    !sender.send(recipients, message.encode(), true).is_empty()
}

/// Broadcast one finalized dealer log to all peers. The single site that encodes
/// `DkgMessage::FinalizedLog` to `Recipients::All`, shared by the gossip loop and
/// the two one-shot post-finalize broadcasts.
#[must_use]
pub(super) fn send_finalized_log(
    sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ceremony_id: DkgCeremonyId,
    signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey>,
) -> bool {
    send_dkg_message(
        sender,
        Recipients::All,
        DkgMessage::FinalizedLog {
            ceremony_id,
            signed_log,
        },
    )
}

pub(super) async fn send_ack(
    sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ceremony_id: DkgCeremonyId,
    dealer: &bls12381::PublicKey,
    ack: PlayerAck<bls12381::PublicKey>,
    success_message: &'static str,
) {
    if send_dkg_message(
        sender,
        Recipients::One(dealer.clone()),
        DkgMessage::Ack { ceremony_id, ack },
    ) {
        debug!(?dealer, message = success_message, "sent DKG ack");
    } else {
        debug!(?dealer, "ack had no accepting recipient this attempt (rate-limited/backpressure); retried by the ceremony loop");
    }
}

pub(super) fn should_retry_share_distribution(
    dealer_active: bool,
    unsent: &BTreeMap<
        bls12381::PublicKey,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerPrivMsg,
    >,
) -> bool {
    dealer_active && !unsent.is_empty()
}

pub(super) fn should_retry_finalized_log_gossip(
    chain_finalized_mode: bool,
    signed_logs: &BTreeMap<bls12381::PublicKey, SignedDealerLog<MinSig, bls12381::PrivateKey>>,
) -> bool {
    !chain_finalized_mode && !signed_logs.is_empty()
}

pub(super) async fn recv_chain_finalized_log(
    rx: &mut Option<mpsc::UnboundedReceiver<Bytes>>,
) -> Option<Bytes> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

pub(super) async fn sleep_until_optional(
    clock: &impl Clock,
    deadline: Option<std::time::SystemTime>,
) {
    match deadline {
        Some(deadline) => clock.sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

pub(super) fn decode_signed_dealer_log(
    bytes: &Bytes,
    max_players: &NonZeroU32,
) -> Result<SignedDealerLog<MinSig, bls12381::PrivateKey>> {
    let mut reader = bytes.as_ref();
    let signed_log =
        SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(&mut reader, max_players)
            .map_err(|e| eyre::eyre!("invalid signed dealer log encoding: {e:?}"))?;
    if !reader.is_empty() {
        return Err(eyre::eyre!("trailing bytes after signed dealer log"));
    }
    Ok(signed_log)
}

pub(super) fn record_and_store_signed_dealer_log(
    signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey>,
    info: &Info<MinSig, bls12381::PublicKey>,
    finalized_logs: &mut BTreeMap<bls12381::PublicKey, DealerLog<MinSig, bls12381::PublicKey>>,
    signed_finalized_logs: &mut BTreeMap<
        bls12381::PublicKey,
        SignedDealerLog<MinSig, bls12381::PrivateKey>,
    >,
    source: &'static str,
) -> Option<bls12381::PublicKey> {
    let dealer_pk = record_signed_dealer_log(signed_log.clone(), info, finalized_logs, source)?;
    signed_finalized_logs
        .entry(dealer_pk.clone())
        .or_insert(signed_log);
    Some(dealer_pk)
}

pub(super) fn record_signed_dealer_log(
    signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey>,
    info: &Info<MinSig, bls12381::PublicKey>,
    finalized_logs: &mut BTreeMap<bls12381::PublicKey, DealerLog<MinSig, bls12381::PublicKey>>,
    source: &'static str,
) -> Option<bls12381::PublicKey> {
    let Some((dealer_pk, log)) = signed_log.check(info) else {
        warn!(source, "finalized DKG dealer log failed verification");
        return None;
    };
    let replaced = finalized_logs.insert(dealer_pk.clone(), log).is_some();
    debug!(
        ?dealer_pk,
        source,
        replaced,
        logs = finalized_logs.len(),
        "recorded finalized DKG dealer log"
    );
    Some(dealer_pk)
}

/// Returns `true` when the canonical group output is reconstructable from the
/// currently-collected finalized dealer logs - i.e. `observe` (public, share-free)
/// succeeds, which means >= 2f+1 CONTENT-VALID logs are present. Mirrors
/// `DkgManager`'s `ceremony::try_reconstruct`, so the actor completes at the SAME
/// log-set prefix the manager freezes `canonical_output` at (-> matching output).
/// Used as the chain-finalized completion gate: a signed-but-garbage dealer log
/// inflates the raw log count but is dropped by `observe`'s content check, so
/// gating on this (not the raw count) prevents finalizing over < 2f+1 valid logs.
pub(super) fn chain_finalized_reconstructable(
    info: &Info<MinSig, bls12381::PublicKey>,
    finalized_logs: &BTreeMap<bls12381::PublicKey, DealerLog<MinSig, bls12381::PublicKey>>,
) -> bool {
    let mut logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info.clone());
    for (dealer_pk, log) in finalized_logs {
        logs.record(dealer_pk.clone(), log.clone());
    }
    observe::<MinSig, bls12381::PublicKey, N3f1, commonware_cryptography::bls12381::Batch>(
        &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
        logs,
        &Sequential,
    )
    .is_ok()
}

pub(super) async fn gossip_finalized_logs(
    sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ceremony_id: DkgCeremonyId,
    signed_logs: &BTreeMap<bls12381::PublicKey, SignedDealerLog<MinSig, bls12381::PrivateKey>>,
) {
    for (dealer, signed_log) in signed_logs {
        if !send_finalized_log(sender, ceremony_id, signed_log.clone()) {
            debug!(
                ?dealer,
                "finalized-log gossip had no accepting recipients this attempt \
                 (rate-limited/backpressure); re-gossiped by the retry tick"
            );
        }
    }
}

/// Send DealerBundle messages to all players in the unsent map.
pub(super) async fn send_shares(
    sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ceremony_id: DkgCeremonyId,
    pub_msg: &DealerPubMsg<MinSig>,
    unsent: &BTreeMap<
        bls12381::PublicKey,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerPrivMsg,
    >,
) {
    for (player_pk, priv_msg) in unsent {
        let message = DkgMessage::DealerBundle {
            ceremony_id,
            pub_msg: pub_msg.clone(),
            priv_msg: priv_msg.clone(),
        };
        if send_dkg_message(sender, Recipients::One(player_pk.clone()), message) {
            debug!(?player_pk, "sent share to player");
        } else {
            debug!(
                ?player_pk,
                "share send had no accepting recipient this attempt \
                 (rate-limited/backpressure); retried by send_shares"
            );
        }
    }
}

/// Remove previously acknowledged remote dealings from the ordinary retry set
/// and return them for restart replay. They remain in that replay set until the
/// remote process regenerates the exact durable ACK, proving that its new
/// process-local Player state ingested the byte-identical dealing.
pub(super) fn take_restart_replay_shares(
    retry_shares: &mut BTreeMap<
        bls12381::PublicKey,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerPrivMsg,
    >,
    accepted_acks: &BTreeMap<bls12381::PublicKey, PlayerAck<bls12381::PublicKey>>,
) -> BTreeMap<
    bls12381::PublicKey,
    commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerPrivMsg,
> {
    accepted_acks
        .keys()
        .filter_map(|player| retry_shares.remove_entry(player))
        .collect()
}

pub(super) fn acknowledge_restart_replay(
    restart_replay_shares: &mut BTreeMap<
        bls12381::PublicKey,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerPrivMsg,
    >,
    accepted_acks: Option<&BTreeMap<bls12381::PublicKey, PlayerAck<bls12381::PublicKey>>>,
    player: &bls12381::PublicKey,
    received_ack: &PlayerAck<bls12381::PublicKey>,
) -> bool {
    let Some(expected_ack) = accepted_acks.and_then(|acks| acks.get(player)) else {
        return false;
    };
    if expected_ack.encode() != received_ack.encode() {
        return false;
    }
    restart_replay_shares.remove(player).is_some()
}
