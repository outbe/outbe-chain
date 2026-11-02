//! Entry points for the begin-block system phases. Both are best effort. A
//! bridge failure must never block a boundary or the slash-window phase. So
//! the entry points log errors, and the next block retries.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;

use crate::abi::ICrosschainGov;
use crate::liveness::LIVENESS_WINDOW_BLOCKS;
use crate::schema::HyperlaneControllerContract;

/// Mirrors the active validator set into the Hyperlane ISMs by sub-calling
/// `sync()` on the CrosschainGov precompile. The sub-call uses a child frame,
/// so the precompile, not the system tx, is the router's `msg.sender`. The
/// precompile's Interchain Account derives from that sender.
pub fn sync_validators(ctx: &BlockRuntimeContext) {
    let call = ICrosschainGov::syncCall {}.abi_encode().into();
    if let Err(error) = ctx.storage.call(CROSSCHAIN_GOV_ADDRESS, U256::ZERO, call) {
        tracing::warn!(
            target: "outbe::hyperlane",
            block = ctx.block.block_number,
            %error,
            "hyperlane validator sync skipped"
        );
    }
}

/// Every [`LIVENESS_WINDOW_BLOCKS`]: jails validators whose Hyperlane agent
/// stopped submitting checkpoints, then re-syncs the ISMs.
pub fn run_liveness_window(ctx: &BlockRuntimeContext) {
    if !ctx
        .block
        .block_number
        .is_multiple_of(LIVENESS_WINDOW_BLOCKS)
    {
        return;
    }
    let mut controller = HyperlaneControllerContract::new(ctx.storage.clone());
    match ctx.storage.with_checkpoint(|| controller.check_liveness()) {
        Ok(jailed) if !jailed.is_empty() => {
            tracing::info!(
                target: "outbe::hyperlane",
                block = ctx.block.block_number,
                ?jailed,
                "validators jailed for hyperlane liveness"
            );
            sync_validators(ctx);
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(
            target: "outbe::hyperlane",
            block = ctx.block.block_number,
            %error,
            "hyperlane liveness window skipped"
        ),
    }
}
