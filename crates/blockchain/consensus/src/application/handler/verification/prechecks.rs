//! Structural checks run before the projection gate or Engine API work.
use super::{
    validate_header_consensus_artifacts_for_activation, ApplicationShared, HeaderArtifactRequest,
    HeaderArtifactValidationDeps, ResolvedVerifyBlocks, ValidatorRole, VerifyRequest,
};
use crate::{
    application::{
        handler::finalized_parent_attestation_from_phase1_system_tx,
        validation::validate_context_parent_binding,
    },
    config::VERIFY_RESOLUTION_TIMEOUT,
};
use tracing::{debug, warn};

impl ApplicationShared {
    pub(super) async fn validate_verify_blocks(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        request: &VerifyRequest,
        resolved: &ResolvedVerifyBlocks,
    ) -> eyre::Result<bool> {
        if !self.validate_verify_parent_and_height(request, resolved) {
            return Ok(false);
        }
        let round = request.context.round;
        let payload_digest = request.payload_digest;
        let block = &resolved.block;
        let parent_block = &resolved.parent_block;

        let ancestry = crate::application::ancestry::marshal_ancestry_reader(
            self.marshal_mailbox.clone(),
            self.block_cache.clone(),
            self.ancestry_readiness.clone(),
            crate::application::ancestry::AncestryLookupPolicy {
                round: Some(round),
                timeout: VERIFY_RESOLUTION_TIMEOUT,
            },
            clock.child("ancestry"),
        );
        if let Err(error) = validate_header_consensus_artifacts_for_activation(
            HeaderArtifactRequest {
                block,
                parent_block: parent_block.as_ref(),
                round,
                proposer: &request.context.leader,
                role: ValidatorRole::from_proposer_evm_address(self.proposer_evm_address),
            },
            HeaderArtifactValidationDeps {
                chain_id: self.chain_id,
                ocomp_lifecycle_activation: self.ocomp_lifecycle_activation,
                certificate_scheme_provider: &self.certificate_scheme_provider,
                committee_provider: &self.committee_provider,
                dkg_manager: &self.dkg_manager,
                ancestry: &ancestry,
            },
        )
        .await
        {
            if error.is_unavailable() {
                crate::metrics::record_dkg_boundary_unavailable(
                    crate::metrics::DkgBoundaryUnavailableReason::AncestryUnavailable,
                );
                return Err(eyre::eyre!("DKG boundary requirement unavailable: {error}"));
            }
            warn!(
                digest = %payload_digest.0,
                round = %round,
                %error,
                "proposed block carries invalid header consensus artifact"
            );
            return Ok(false);
        }

        // This Phase 1 check only decodes the system tx structure. It does not
        // decode or check the carried BLS certificate, and it does not check
        // accounting. The header artifact check above is not only structural:
        // it reads the epoch committee snapshot and certificate scheme to bind
        // the leader, and it runs DKG boundary admission against ancestry.
        // After these prechecks, `handle_verify` runs full execution
        // verification through the executor. There, the EVM-side V2 verifier
        // (`outbe_consensus::proof::verify_v2_proof`) does the full V2
        // cryptographic verification.
        if let Err(error) = finalized_parent_attestation_from_phase1_system_tx(block) {
            warn!(
                digest = %payload_digest.0,
                round = %round,
                %error,
                "failed to decode Phase 1 finalized-parent metadata structure during verify"
            );
            return Ok(false);
        }

        Ok(true)
    }
    fn validate_verify_parent_and_height(
        &self,
        request: &VerifyRequest,
        resolved: &ResolvedVerifyBlocks,
    ) -> bool {
        let round = request.context.round;
        let payload_digest = request.payload_digest;
        let parent_digest = request.parent_digest();
        let block = &resolved.block;
        let parent_block = &resolved.parent_block;
        if let Err(error) = validate_context_parent_binding(
            block,
            parent_block.as_ref(),
            parent_digest,
            self.genesis_hash,
        ) {
            warn!(
                digest = %payload_digest.0,
                round = %round,
                block_number = block.number(),
                parent = %parent_digest.0,
                %error,
                "proposed block does not extend Simplex context parent"
            );
            return false;
        }

        if let Err(rejection) = self.epoch_fence.check(round, block.number()) {
            debug!(
                %round,
                digest = %payload_digest.0,
                block_number = block.number(),
                ?rejection,
                "dropping stale verify before Engine API work"
            );
            return false;
        }

        if let Err(error) = self.vrf_safety.ensure_block_allowed(block.number()) {
            warn!(
                digest = %payload_digest.0,
                round = %round,
                block_number = block.number(),
                %error,
                "proposed block is above VRF expiry"
            );
            return false;
        }

        true
    }
}
