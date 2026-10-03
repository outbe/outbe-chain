use super::super::recovery::DkgRetryStore;
use super::ceremony::{CeremonyChannels, CeremonyConfig, CeremonyState, MessageOutcome};
use super::{recv_chain_finalized_log, sleep_until_optional, DkgComplete, DkgProgress};
use alloy_primitives::Bytes;
use commonware_cryptography::bls12381;
use commonware_cryptography::bls12381::{
    dkg::feldman_desmedt::Output,
    primitives::{group::Share, variant::MinSig},
};
use commonware_p2p::{Receiver as P2pReceiver, Sender as P2pSender};
use commonware_runtime::Clock;
use commonware_utils::ordered::Set;
use eyre::Result;
use tokio::sync::mpsc;
use tracing::debug;

/// Run a DKG ceremony with durable local dealer and player recovery.
///
/// The dealer seed is persisted before any bundle is sent. Player inputs are
/// persisted before their ACK is emitted. A restarted process reconstructs both
/// roles and verifies byte-identical ACK replay before networking resumes.
#[allow(clippy::too_many_arguments)]
pub async fn run_initial_dkg_durable(
    clock: &impl Clock,
    signing_key: bls12381::PrivateKey,
    participants: Set<bls12381::PublicKey>,
    previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
    previous_share: Option<Share>,
    round: u64,
    progress_tx: Option<mpsc::UnboundedSender<DkgProgress>>,
    finalized_log_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
    retry_store: DkgRetryStore,
    sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<DkgComplete> {
    run(
        clock,
        CeremonyConfig {
            signing_key,
            participants,
            previous_output,
            previous_share,
            round,
            retry_store: Some(retry_store),
        },
        CeremonyChannels {
            sender,
            receiver,
            progress_tx,
            finalized_log_rx,
        },
    )
    .await
}

async fn run(
    clock: &impl Clock,
    config: CeremonyConfig,
    mut channels: CeremonyChannels<
        impl P2pSender<PublicKey = bls12381::PublicKey>,
        impl P2pReceiver<PublicKey = bls12381::PublicKey>,
    >,
) -> Result<DkgComplete> {
    let mut state = CeremonyState::start(
        clock,
        config,
        channels.finalized_log_rx.is_some(),
        &mut channels.sender,
    )
    .await?;
    loop {
        // Preserve the biased receive > chain > retry > grace > timeout order.
        commonware_macros::select! {
            msg_result = channels.receiver.recv() => {
                let (from, mut raw) = msg_result.map_err(|e| eyre::eyre!("DKG P2P receiver error: {e}"))?;
                let Some(msg) = state.read_message(&from, &mut raw) else { continue };
                if state.handle_message(from, msg, &mut channels.sender, &channels.progress_tx).await? == MessageOutcome::SkipIteration {
                    continue;
                }
            },
            chain_log = recv_chain_finalized_log(&mut channels.finalized_log_rx) => {
                match chain_log {
                    Some(bytes) => state.record_chain_log(bytes),
                    None => {
                        channels.finalized_log_rx = None;
                        debug!("chain-finalized DKG dealer log stream closed");
                    }
                }
            },
            _ = clock.sleep_until(state.next_retry_tick) => state.retry(&mut channels.sender).await,
            _ = sleep_until_optional(clock, state.ack_collection_deadline) => {},
            _ = clock.sleep_until(state.deadline) => return Err(state.timeout_error()),
        }
        if state
            .seal_dealer(clock, &mut channels.sender, &channels.progress_tx)
            .await?
            == MessageOutcome::SkipIteration
        {
            continue;
        }
        if state.bootstrap_complete(clock, &mut channels.sender).await || state.chain_complete() {
            break;
        }
    }
    state.finalize()
}
