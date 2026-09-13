//! DKG initial ceremony actor.
//!
//! Runs the interactive DKG protocol over a dedicated P2P channel.
//! Initial bootstrap makes all genesis validators Dealer and Player. Live
//! reshare makes previous-output players Dealer and the target set Player, so
//! newly added validators can join as player-only until they receive a share.
//! Reshare ceremonies complete from >= 2f+1 chain-finalized dealer logs.
//! Interactive bootstrap without a chain carrier waits for every genesis
//! participant's dealer log, because threshold P2P subsets are not canonical and
//! may otherwise produce different public polynomials on different validators.
//!
//! Protocol:
//! 1. Each validator calls `Dealer::start()` -> gets `DealerPubMsg` + per-player `DealerPrivMsg`
//! 2. Each dealer sends `DealerBundle(pub_msg, priv_msg_i)` to player i via P2P
//! 3. Each player validates via `Player::dealer_message()`, sends `Ack` back to dealer
//! 4. Each node handles its own dealing locally (no network round-trip for self)
//! 5. Each dealer calls `Dealer::finalize()` -> `SignedDealerLog`
//! 6. Each dealer broadcasts `FinalizedLog(log)` to ALL via P2P
//! 7. Each player collects dealer logs, calls `Player::finalize()` -> `(Output, Share)`
//!
//! Local threshold finalization is not the activation source of truth. The
//! commonware `select()` function deterministically picks the first
//! `required_commitments` valid logs from the logs it is given, but different
//! nodes can receive different P2P subsets. Live reshare activation therefore
//! uses the canonical output reconstructed from finalized chain-carried dealer
//! logs; initial bootstrap waits for all genesis logs before block production.

use alloy_primitives::Bytes;
use commonware_cryptography::bls12381;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Output;
use commonware_cryptography::bls12381::primitives::group::Share;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_utils::ordered::Set;
use std::time::Duration;

/// Result of a successful DKG ceremony.
#[derive(Clone, Debug)]
pub struct DkgComplete {
    /// Threshold public polynomial (shared by all participants).
    pub output: Output<MinSig, bls12381::PublicKey>,
    /// This validator's private threshold share.
    pub share: Share,
    /// The participant set used in this DKG round.
    pub participants: Set<bls12381::PublicKey>,
}

/// Result of a dealer-only reshare participant.
///
/// A removed validator that still owns a previous threshold share remains a
/// dealer for the reshare, but is not a player in the target participant set
/// and therefore does not receive a fresh share.
#[derive(Clone, Debug)]
pub struct DkgDealerOnlyComplete {
    /// The participant set that receives fresh shares in this DKG round.
    pub participants: Set<bls12381::PublicKey>,
}

/// Progress emitted while a DKG ceremony is running in parallel with consensus.
#[derive(Debug, Clone)]
pub enum DkgProgress {
    /// The local dealer has finalized its signed dealer log and it may now be
    /// carried in a proposal `header.extra_data`.
    LocalDealerLog(Bytes),
    /// A valid P2P finalized dealer log was observed while chain-finalized mode
    /// is active. It may become a proposal candidate, but it is not canonical
    /// until it appears in finalized `header.extra_data`.
    P2pDealerLog(Bytes),
}

/// Timeout for the entire DKG ceremony.
const DKG_TIMEOUT: Duration = Duration::from_secs(120);

/// Interval between retry attempts for unsent shares.
#[cfg(not(test))]
const RETRY_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(test)]
const RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// Once a dealer has the Byzantine-liveness quorum, keep accepting ACKs through
/// a bounded node+enclave restart window before sealing its log.
/// Feldman-Desmedt deliberately publishes the evaluations of non-acking players;
/// ending grace before a healthy SGX node can restart can therefore turn a
/// recoverable crash into permanent share disclosure. Healthy ceremonies still
/// finalize immediately once every player ACKs; this longer bound affects only
/// missing-player recovery and remains below the configured prepare window.
#[cfg(not(test))]
const ACK_COLLECTION_GRACE: Duration = Duration::from_secs(30);
#[cfg(test)]
const ACK_COLLECTION_GRACE: Duration = Duration::from_millis(450);

/// Initial bootstrap has no chain carrier yet, so nodes that already collected
/// all genesis logs keep gossiping them briefly before returning threshold
/// material. This prevents fast nodes from leaving the DKG channel while slower
/// nodes are still missing a final log.
#[cfg(not(test))]
const BOOTSTRAP_FINALIZED_LOG_GOSSIP_GRACE: Duration = Duration::from_secs(10);
#[cfg(test)]
const BOOTSTRAP_FINALIZED_LOG_GOSSIP_GRACE: Duration = Duration::from_millis(250);

#[cfg(test)]
mod tests;

#[cfg(test)]
mod test_support;
#[cfg(test)]
pub use test_support::{run_initial_dkg, run_reshare_dealer_only};

mod messaging;
use messaging::{
    acknowledge_restart_replay, chain_finalized_reconstructable, decode_signed_dealer_log,
    gossip_finalized_logs, record_and_store_signed_dealer_log, record_signed_dealer_log,
    recv_chain_finalized_log, send_ack, send_finalized_log, send_shares,
    should_retry_finalized_log_gossip, should_retry_share_distribution, sleep_until_optional,
    take_restart_replay_shares,
};

mod dealer_only;
pub use dealer_only::run_reshare_dealer_only_durable;

mod round;
pub use round::run_initial_dkg_durable;
