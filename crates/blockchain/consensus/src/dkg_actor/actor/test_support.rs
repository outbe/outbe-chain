use super::super::recovery::DkgRetryStore;
use super::run_initial_dkg_durable;
use super::run_reshare_dealer_only_durable;
use super::DkgComplete;
use super::DkgDealerOnlyComplete;
use super::DkgProgress;
pub use super::{
    DkgDealerParameters as DealerOnlyDkgFixture, DkgParticipantParameters as InitialDkgFixture,
};
use alloy_primitives::Bytes;
use commonware_cryptography::bls12381;
use commonware_p2p::Receiver as P2pReceiver;
use commonware_p2p::Sender as P2pSender;
use commonware_runtime::Clock;
use commonware_utils::ordered::Set;
use eyre::Result;
use tokio::sync::mpsc;

impl InitialDkgFixture {
    pub fn bootstrap(
        signing_key: bls12381::PrivateKey,
        participants: Set<bls12381::PublicKey>,
    ) -> Self {
        Self {
            signing_key,
            participants,
            previous_output: None,
            previous_share: None,
            round: 0,
        }
    }
}

/// Run a DKG ceremony over P2P (initial or reshare).
///
/// Blocks until the ceremony completes or times out. The completion quorum
/// depends on the mode:
/// - **Chain-finalized reshare**: completes on `>= 2f+1` (N3f1) finalized dealer
///   logs. The chain carrier makes the selected subset canonical, so one or more
///   offline validators do NOT block the ceremony.
/// - **Initial interactive bootstrap (genesis)**: requires ALL `n` genesis dealer
///   logs. There is no canonical carrier yet, so every validator must agree on
///   the identical complete dealer-log set to derive the same public polynomial;
///   a `2f+1` subset would be non-deterministic and could fork the genesis
///   committee. A single offline founder therefore stalls genesis until the
///   timeout - by design (see the completion guard below and
///   `test_bootstrap_dkg_waits_for_all_genesis_nodes_*`).
///
/// # Arguments
/// * `fixture` - this validator's key, ordered participants and previous round material
/// * `progress_tx` - local and P2P dealer-log progress for this ceremony
/// * `finalized_log_rx` - finalized chain-carried dealer logs for this ceremony
/// * `network` - P2P sender and receiver for the DKG channel
#[cfg(test)]
pub async fn run_initial_dkg(
    clock: &impl Clock,
    fixture: InitialDkgFixture,
    progress_tx: Option<mpsc::UnboundedSender<DkgProgress>>,
    finalized_log_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
    network: (
        impl P2pSender<PublicKey = bls12381::PublicKey>,
        impl P2pReceiver<PublicKey = bls12381::PublicKey>,
    ),
) -> Result<DkgComplete> {
    let InitialDkgFixture {
        signing_key,
        participants,
        previous_output,
        previous_share,
        round,
    } = fixture;
    let (sender, receiver) = network;
    let recovery_dir = tempfile::tempdir()?;
    run_initial_dkg_durable(
        clock,
        super::DkgParticipantParameters {
            signing_key,
            participants,
            previous_output,
            previous_share,
            round,
        },
        super::DkgProgressChannels {
            progress_tx,
            finalized_log_rx,
        },
        DkgRetryStore::in_keys_dir(recovery_dir.path(), crate::bls::KeyBackend::Plaintext),
        super::DkgTransport { sender, receiver },
    )
    .await
}

/// Run the dealer-only side of a live reshare.
///
/// This is used by validators that are in the previous DKG output and hold a
/// previous share, but are excluded from the target participant set. They must
/// still deal to the new players so the reshare can complete, but they must not
/// create a `Player` or wait for a new share.
#[cfg(test)]
pub async fn run_reshare_dealer_only(
    clock: &impl Clock,
    fixture: DealerOnlyDkgFixture,
    progress_tx: mpsc::UnboundedSender<DkgProgress>,
    network: (
        impl P2pSender<PublicKey = bls12381::PublicKey>,
        impl P2pReceiver<PublicKey = bls12381::PublicKey>,
    ),
) -> Result<DkgDealerOnlyComplete> {
    let DealerOnlyDkgFixture {
        signing_key,
        participants,
        previous_output,
        previous_share,
        round,
    } = fixture;
    let (sender, receiver) = network;
    let recovery_dir = tempfile::tempdir()?;
    run_reshare_dealer_only_durable(
        clock,
        super::DkgDealerParameters {
            signing_key,
            participants,
            previous_output,
            previous_share,
            round,
        },
        progress_tx,
        DkgRetryStore::in_keys_dir(recovery_dir.path(), crate::bls::KeyBackend::Plaintext),
        super::DkgTransport { sender, receiver },
    )
    .await
}
