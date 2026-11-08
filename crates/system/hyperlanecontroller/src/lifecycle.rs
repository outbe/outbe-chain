//! Entry points for the begin-block system phases. Non-fatal bridge failures
//! are best effort (logged so the next block retries), whereas node-local
//! storage or fatal invariant failures propagate and abort the block.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};

use crate::liveness::LIVENESS_WINDOW_BLOCKS;
use crate::precompile::IHyperlaneController;
use crate::schema::HyperlaneControllerContract;

fn should_propagate_hyperlane_error(error: &PrecompileError) -> bool {
    error.is_node_local()
        || matches!(
            error,
            PrecompileError::Fatal(_) | PrecompileError::FatalWithCause(_)
        )
}

/// Mirrors the active validator set into the Hyperlane ISMs by sub-calling
/// the controller's `sync()`. The sub-call uses a child frame, so the
/// controller, not the system tx, is the `msg.sender` of its `callLocal` and
/// `callRemote` calls to the owner (`CROSSCHAIN_BRIDGE_ADDRESS`).
pub fn sync_validators(ctx: &BlockRuntimeContext) -> Result<()> {
    let call = IHyperlaneController::syncCall {}.abi_encode().into();
    if let Err(error) = ctx
        .storage
        .call(HYPERLANE_CONTROLLER_ADDRESS, U256::ZERO, call)
    {
        if should_propagate_hyperlane_error(&error) {
            return Err(error);
        }
        tracing::warn!(
            target: "outbe::hyperlane",
            block = ctx.block.block_number,
            %error,
            "hyperlane validator sync skipped"
        );
    }
    Ok(())
}

/// Every [`LIVENESS_WINDOW_BLOCKS`]: jails validators whose Hyperlane agent
/// stopped submitting checkpoints, then re-syncs the ISMs.
pub fn run_liveness_window(ctx: &BlockRuntimeContext) -> Result<()> {
    if !ctx
        .block
        .block_number
        .is_multiple_of(LIVENESS_WINDOW_BLOCKS)
    {
        return Ok(());
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
            sync_validators(ctx)?;
        }
        Ok(_) => {}
        Err(error) if should_propagate_hyperlane_error(&error) => return Err(error),
        Err(error) => tracing::warn!(
            target: "outbe::hyperlane",
            block = ctx.block.block_number,
            %error,
            "hyperlane liveness window skipped"
        ),
    }
    Ok(())
}

/// Begin-block liveness for the hyperlane slash window.
pub struct HyperlaneModule;

impl outbe_primitives::block::ProtocolModule for HyperlaneModule {
    const NAME: &'static str = "hyperlane_slash";

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        run_liveness_window(ctx)
    }
}
