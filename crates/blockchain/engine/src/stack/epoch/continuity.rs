//! Finalized continuity anchors required before a Simplex engine starts.
use super::super::*;
use outbe_consensus::finalization::state::FinalizationViewHandle;

const EPOCH_RESTART_ANCHOR_TIMEOUT: Duration = Duration::from_secs(5);
const EPOCH_RESTART_ANCHOR_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(in crate::stack) async fn resolve_epoch_floor<E: Clock>(
    ctx: &E,
    finalization_view: &FinalizationViewHandle,
    epoch: Epoch,
    genesis_hash: B256,
) -> Result<Digest> {
    Ok(if epoch.get() == 0 {
        Digest(genesis_hash)
    } else {
        let deadline = ctx.current() + EPOCH_RESTART_ANCHOR_TIMEOUT;
        loop {
            let (height, hash, round_ready) = {
                let view = finalization_view.read();
                (
                    view.last_finalized_number,
                    view.forkchoice.finalized_block_hash,
                    view.last_finalized_round.is_some(),
                )
            };
            if height > 0 && hash != alloy_primitives::B256::ZERO && round_ready {
                break Digest(hash);
            }
            if ctx.current() >= deadline {
                return Err(eyre::eyre!(
                    "epoch={} restart without finalized anchor after {:?}; \
                         handle_genesis would return ZERO, or Phase 1 would lack \
                         the finalized-round proof key for parent_view=0",
                    epoch.get(),
                    EPOCH_RESTART_ANCHOR_TIMEOUT,
                ));
            }
            ctx.sleep(EPOCH_RESTART_ANCHOR_POLL_INTERVAL).await;
        }
    })
}

/// Distinguishes the two existing activation diagnostics. Both require the
/// exact activation height to be finalized before the next engine starts.
#[derive(Clone, Copy)]
pub(in crate::stack) enum AnchorTransition {
    Dkg,
    DealerDemotion,
}

pub(in crate::stack) async fn wait_for_activation_anchor<E: Clock>(
    ctx: &E,
    finalization_view: &FinalizationViewHandle,
    activation_height: u64,
    transition: AnchorTransition,
) -> Result<()> {
    let deadline = ctx.current() + EPOCH_RESTART_ANCHOR_TIMEOUT;
    loop {
        let (finalized, finalized_hash, round_ready) = {
            let view = finalization_view.read();
            (
                view.last_finalized_number,
                view.forkchoice.finalized_block_hash,
                view.last_finalized_round.is_some(),
            )
        };
        if finalized >= activation_height && finalized_hash != B256::ZERO && round_ready {
            return Ok(());
        }
        if ctx.current() >= deadline {
            return Err(match transition {
                AnchorTransition::Dkg => eyre::eyre!(
                    "DKG activation race after {:?}: finalized_anchor=(height={}, hash={}, round_ready={}) activation_height={}; FinalizationActor is lagging the DKG manager",
                    EPOCH_RESTART_ANCHOR_TIMEOUT, finalized, finalized_hash, round_ready, activation_height
                ),
                AnchorTransition::DealerDemotion => eyre::eyre!(
                    "exited-validator demotion activation race after {:?}: finalized=(height={}, hash={}, round_ready={}) activation_height={}",
                    EPOCH_RESTART_ANCHOR_TIMEOUT, finalized, finalized_hash, round_ready, activation_height
                ),
            });
        }
        ctx.sleep(EPOCH_RESTART_ANCHOR_POLL_INTERVAL).await;
    }
}
