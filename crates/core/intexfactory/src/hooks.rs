//! Entry points of the Intex passes.

use outbe_primitives::{
    block::{BlockRuntimeContext, ProtocolModule},
    error::Result,
};

/// Deletes the spent series of the expired groups.
pub use crate::cleanup::cleanup;

/// Protocol module for the end-block Intex factory step.
pub struct IntexFactoryModule;

impl ProtocolModule for IntexFactoryModule {
    const NAME: &'static str = "intex_factory";

    fn end_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        crate::called::process_call_pass(ctx)?;
        crate::notify::send_called_notices(ctx)?;
        crate::cleanup::cleanup(ctx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::{
        block::BlockHeightRegime,
        reshare_artifact::OutbeBlockArtifacts,
        system_tx_context::{SystemTxPayloadValidationError, SystemTxValidationContext},
    };

    #[test]
    fn intex_factory_step_rejects_unexpected_payload() {
        let artifacts = OutbeBlockArtifacts::default();
        let ctx = SystemTxValidationContext {
            regime: BlockHeightRegime::SteadyState,
            block_number: 2,
            parent_hash: alloy_primitives::B256::ZERO,
            artifacts: &artifacts,
        };
        assert!(matches!(
            IntexFactoryModule::validate_payload(&[1], &ctx),
            Err(SystemTxPayloadValidationError::UnexpectedNonEmptyPayload {
                module: "intex_factory",
                len: 1,
            })
        ));
    }
}
