use super::super::recovery::DkgRetryStore;
use super::run_initial_dkg_durable;
use super::run_reshare_dealer_only_durable;
use super::DkgComplete;
use super::DkgDealerOnlyComplete;
use super::DkgProgress;
use alloy_primitives::Bytes;
use commonware_cryptography::bls12381;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Output;
use commonware_cryptography::bls12381::primitives::group::Share;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_p2p::Receiver as P2pReceiver;
use commonware_p2p::Sender as P2pSender;
use commonware_runtime::Clock;
use commonware_utils::ordered::Set;
use eyre::Result;
use tokio::sync::mpsc;

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
/// * `signing_key` - this validator's BLS individual private key (MinPk)
/// * `participants` - ordered set of all validator BLS public keys
/// * `previous_output` - `None` for initial DKG, `Some(output)` for reshare
/// * `previous_share` - `None` for initial DKG, `Some(share)` for reshare
/// * `round` - DKG round number (0 for initial, incremented for reshares)
/// * `finalized_log_rx` - finalized chain-carried dealer logs for this ceremony
/// * `sender` - P2P sender for the DKG channel
/// * `receiver` - P2P receiver for the DKG channel
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub async fn run_initial_dkg(
    clock: &impl Clock,
    signing_key: bls12381::PrivateKey,
    participants: Set<bls12381::PublicKey>,
    previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
    previous_share: Option<Share>,
    round: u64,
    progress_tx: Option<mpsc::UnboundedSender<DkgProgress>>,
    finalized_log_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
    sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<DkgComplete> {
    let recovery_dir = tempfile::tempdir()?;
    run_initial_dkg_durable(
        clock,
        signing_key,
        participants,
        previous_output,
        previous_share,
        round,
        progress_tx,
        finalized_log_rx,
        DkgRetryStore::in_keys_dir(recovery_dir.path(), crate::bls::KeyBackend::Plaintext),
        sender,
        receiver,
    )
    .await
}

/// Run the dealer-only side of a live reshare.
///
/// This is used by validators that are in the previous DKG output and hold a
/// previous share, but are excluded from the target participant set. They must
/// still deal to the new players so the reshare can complete, but they must not
/// create a `Player` or wait for a new share.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub async fn run_reshare_dealer_only(
    clock: &impl Clock,
    signing_key: bls12381::PrivateKey,
    participants: Set<bls12381::PublicKey>,
    previous_output: Output<MinSig, bls12381::PublicKey>,
    previous_share: Share,
    round: u64,
    progress_tx: mpsc::UnboundedSender<DkgProgress>,
    sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<DkgDealerOnlyComplete> {
    let recovery_dir = tempfile::tempdir()?;
    run_reshare_dealer_only_durable(
        clock,
        signing_key,
        participants,
        previous_output,
        previous_share,
        round,
        progress_tx,
        DkgRetryStore::in_keys_dir(recovery_dir.path(), crate::bls::KeyBackend::Plaintext),
        sender,
        receiver,
    )
    .await
}
