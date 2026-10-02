use super::super::recovery::handle_player_bundle;
use super::super::recovery::DkgDealerRetrySnapshot;
use super::super::recovery::DkgRetryStore;
use super::super::recovery::PlayerBundleAction;
use super::super::recovery::{
    load_or_create_dealer_snapshot, persist_dealer_ack, restore_player, DkgPlayerRetrySnapshot,
};
use super::super::wire::DkgCeremonyId;
use super::super::wire::DkgMessage;
use super::super::wire::DkgMessageReadError;
use super::super::wire::DkgWireConfig;
use super::acknowledge_restart_replay;
use super::chain_finalized_reconstructable;
use super::decode_signed_dealer_log;
use super::gossip_finalized_logs;
use super::record_and_store_signed_dealer_log;
use super::record_signed_dealer_log;
use super::send_ack;
use super::send_finalized_log;
use super::send_shares;
use super::should_retry_finalized_log_gossip;
use super::should_retry_share_distribution;
use super::take_restart_replay_shares;
use super::DkgComplete;
use super::DkgProgress;
use super::ACK_COLLECTION_GRACE;
use super::BOOTSTRAP_FINALIZED_LOG_GOSSIP_GRACE;
use super::DKG_TIMEOUT;
use super::RETRY_INTERVAL;
use alloy_primitives::Bytes;
use commonware_codec::Encode;
use commonware_cryptography::bls12381;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Dealer;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerLog;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Info;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Logs;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Output;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::SignedDealerLog;
use commonware_cryptography::bls12381::primitives::group::Share;
use commonware_cryptography::bls12381::primitives::sharing::Mode;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_p2p::Sender as P2pSender;
use commonware_parallel::Sequential;
use commonware_runtime::Clock;
use commonware_utils::ordered::Quorum;
use commonware_utils::ordered::Set;
use commonware_utils::N3f1;
use eyre::Result;
use rand_commonware::SeedableRng;
use std::collections::BTreeMap;
use std::num::NonZeroU32;

// Intentionally `tokio::sync::mpsc`: `progress_tx` / `finalized_log_rx` are created
// cross-crate by `outbe-engine` (`crates/blockchain/engine/src/stack.rs`) and have no
// timer/spawn dependency, so they are runtime-agnostic and do not require the tokio
// reactor. The type is kept to preserve the cross-crate engine API.
use tokio::sync::mpsc;
use tracing::debug;
use tracing::info;
use tracing::warn;

use commonware_cryptography::bls12381::dkg::feldman_desmedt::{
    DealerPrivMsg, DealerPubMsg, Player, PlayerAck,
};
use std::collections::BTreeSet;
use std::time::SystemTime;

pub(super) struct WireDiagnostics {
    wrong_ceremony: &'static str,
    invalid_message: &'static str,
}

const PARTICIPANT_DIAGNOSTICS: WireDiagnostics = WireDiagnostics {
    wrong_ceremony: "received DKG message for a different ceremony, ignoring",
    invalid_message: "failed to decode DKG message, ignoring",
};

pub(super) const DEALER_ONLY_DIAGNOSTICS: WireDiagnostics = WireDiagnostics {
    wrong_ceremony: "received dealer-only DKG message for a different ceremony, ignoring",
    invalid_message: "failed to decode dealer-only DKG message, ignoring",
};

pub(super) fn read_ceremony_message(
    from: &bls12381::PublicKey,
    buf: &mut impl bytes::Buf,
    wire_cfg: &DkgWireConfig,
    diagnostics: &WireDiagnostics,
) -> Option<DkgMessage> {
    match DkgMessage::read_for_ceremony(buf, wire_cfg) {
        Ok(message) => Some(message),
        Err(DkgMessageReadError::WrongCeremonyId { expected, received }) => {
            warn!(
                ?from,
                expected_round = expected.round,
                received_round = received.round,
                expected_info_hash = %expected.info_hash,
                received_info_hash = %received.info_hash,
                "{}", diagnostics.wrong_ceremony
            );
            None
        }
        Err(DkgMessageReadError::Codec(e)) => {
            warn!(?e, ?from, "{}", diagnostics.invalid_message);
            None
        }
    }
}

/// Inputs whose identity is fixed for one durable participant ceremony.
pub(super) struct CeremonyConfig {
    pub(super) signing_key: bls12381::PrivateKey,
    pub(super) participants: Set<bls12381::PublicKey>,
    pub(super) previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
    pub(super) previous_share: Option<Share>,
    pub(super) round: u64,
    pub(super) retry_store: Option<DkgRetryStore>,
}

/// Runtime handles stay separate from the cryptographic ceremony inputs.
pub(super) struct CeremonyChannels<S, R> {
    pub(super) sender: S,
    pub(super) receiver: R,
    pub(super) progress_tx: Option<mpsc::UnboundedSender<DkgProgress>>,
    pub(super) finalized_log_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
}

/// A skipped message must also skip the post-select sealing/completion gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageOutcome {
    Advance,
    SkipIteration,
}

struct RecoveredRoles {
    dealer: Option<Dealer<MinSig, bls12381::PrivateKey>>,
    my_pub_msg: Option<DealerPubMsg<MinSig>>,
    dealer_retry_snapshot: Option<DkgDealerRetrySnapshot>,
    player: Player<MinSig, bls12381::PrivateKey>,
    player_retry_snapshot: DkgPlayerRetrySnapshot,
    unsent_shares: BTreeMap<bls12381::PublicKey, DealerPrivMsg>,
    acked_players: BTreeSet<bls12381::PublicKey>,
    restart_replay_shares: BTreeMap<bls12381::PublicKey, DealerPrivMsg>,
}

/// The single owner of a participant attempt's mutable protocol state.
pub(super) struct CeremonyState {
    info: Info<MinSig, bls12381::PublicKey>,
    ceremony_id: DkgCeremonyId,
    participants: Set<bls12381::PublicKey>,
    n: u32,
    player_threshold: u32,
    log_threshold: u32,
    wire_cfg: DkgWireConfig,
    retry_store: Option<DkgRetryStore>,
    roles: RecoveredRoles,
    finalized_logs: BTreeMap<bls12381::PublicKey, DealerLog<MinSig, bls12381::PublicKey>>,
    signed_finalized_logs:
        BTreeMap<bls12381::PublicKey, SignedDealerLog<MinSig, bls12381::PrivateKey>>,
    invalid_dealers: BTreeSet<bls12381::PublicKey>,
    chain_finalized_mode: bool,
    pub(super) deadline: SystemTime,
    pub(super) next_retry_tick: SystemTime,
    pub(super) ack_collection_deadline: Option<SystemTime>,
    last_reconstruct_probe_len: usize,
    bootstrap_threshold_logged: bool,
    bootstrap_all_logs_collected_at: Option<SystemTime>,
}

mod completion;
mod messages;
mod recovery;

#[cfg(test)]
mod tests;
