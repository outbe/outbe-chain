//! Block-boundary protocol module for `CredisFactory`.

use outbe_primitives::{
    block::{BlockRuntimeContext, ProtocolModule},
    error::Result,
};

/// Protocol module for the end-block Credis factory step.
pub struct CredisFactoryModule;

impl ProtocolModule for CredisFactoryModule {
    const NAME: &'static str = "credis_factory";

    fn end_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        crate::called::process_call_pass(ctx)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::{
        block::BlockHeightRegime, reshare_artifact::OutbeBlockArtifacts,
        system_tx_context::SystemTxBuildContext,
    };

    #[test]
    fn credis_factory_step_builds_an_empty_end_block_payload() {
        let artifacts = OutbeBlockArtifacts::default();
        let ctx = SystemTxBuildContext {
            regime: BlockHeightRegime::SteadyState,
            block_number: 2,
            parent_hash: alloy_primitives::B256::ZERO,
            artifacts: &artifacts,
            parent_consensus_metadata: None,
            pending_tee_bootstrap: None,
            expected_tee_policy: None,
        };
        assert!(CredisFactoryModule::build_payload(&ctx).unwrap().is_empty());
    }
}
