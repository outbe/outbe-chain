use super::*;
impl ApplicationShared {
    pub(super) async fn resolve_proposal_parent(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        context: super::super::ingress::SimplexContext,
    ) -> eyre::Result<Option<ProposalParent>> {
        let (parent_view, parent) = context.parent;
        let parent_digest = Digest(parent.0);
        let round = context.round;
        debug!(%round, %parent_view, parent = %parent_digest.0, "propose requested");

        // epoch continuity: special-case the first proposal of a
        // new Simplex epoch (`epoch > 0`, `parent_view = 0`) before the chain
        // genesis path. `Ok(None)` means "not an epoch boundary"; caller falls
        // through to the chain genesis / cache / marshal-by-digest branches.
        let maybe_epoch_anchor = match epoch_boundary::resolve_epoch_boundary_parent(
            &self.finalization_view,
            &self.marshal_mailbox,
            clock,
            epoch_boundary::EpochBoundaryParentRequest {
                round,
                parent_view,
                parent_digest,
            },
        )
        .await
        {
            Ok(opt) => opt,
            Err(error) => {
                warn!(
                    %round,
                    parent = %parent_digest.0,
                    %error,
                    "propose: epoch boundary parent resolution failed; forfeiting slot"
                );
                return Ok(None);
            }
        };
        debug_assert!(
            !(round.epoch().get() > 0
                && parent_view == View::new(0)
                && maybe_epoch_anchor.is_none()),
            "resolve_epoch_boundary_parent invariant: epoch>0 && parent_view=0 must \
             resolve to Some(EpochBoundaryParent) or return an explicit error"
        );

        if let Some(anchor) = maybe_epoch_anchor {
            return Ok(Some(ProposalParent {
                height: anchor.height,
                digest: parent_digest,
                block: Some(anchor.block),
                proof_key: Some(anchor.proof_key),
            }));
        }
        if parent_digest.0 == self.genesis_hash {
            return Ok(Some(ProposalParent {
                height: Height::zero(),
                digest: parent_digest,
                block: None,
                proof_key: None,
            }));
        }
        self.resolve_cached_proposal_parent(clock, context)
            .await
            .map(Some)
    }
    async fn resolve_cached_proposal_parent(
        &self,
        clock: &(impl commonware_runtime::Clock + commonware_runtime::Supervisor),
        context: super::super::ingress::SimplexContext,
    ) -> eyre::Result<ProposalParent> {
        let (parent_view, parent) = context.parent;
        let parent_digest = Digest(parent.0);
        let round = context.round;
        let cached_parent = self.block_cache.get_and_remove(&parent_digest);
        let parent_block = if let Some(block) = cached_parent {
            block
        } else {
            // Parent from another proposer - resolve via marshal.
            let marshal = self.marshal_mailbox.clone();
            let block_future = marshal.subscribe_by_digest(
                parent_digest,
                commonware_consensus::marshal::core::DigestFallback::FetchByRound {
                    round: parent_round(round, parent_view),
                },
            );
            match clock
                .timeout(PROPOSE_RESOLUTION_TIMEOUT, block_future)
                .await
            {
                Ok(Ok(block)) => (*block).clone(),
                Ok(Err(_)) => {
                    return Err(eyre::eyre!(
                        "failed to resolve parent block {} for proposal",
                        parent_digest.0
                    ));
                }
                Err(_) => {
                    return Err(eyre::eyre!(
                        "timed out resolving parent block {} for proposal",
                        parent_digest.0
                    ));
                }
            }
        };

        let parent_height = Height::new(parent_block.number());
        Ok(ProposalParent {
            height: parent_height,
            digest: parent_digest,
            block: Some(parent_block),
            proof_key: Some(CertifiedParentProofKey::new(
                parent_round(round, parent_view).epoch().get(),
                parent_view.get(),
                parent_digest.0,
            )),
        })
    }
    pub(super) async fn proposal_parent_gate(
        &self,
        round: Round,
        parent: &ProposalParent,
    ) -> eyre::Result<Option<ProposeOutcome>> {
        let parent_height = parent.height;
        let parent_digest = parent.digest;
        let next_block_number = parent_height.get().saturating_add(1);
        if let Err(rejection) = self.epoch_fence.check(round, next_block_number) {
            debug!(
                %round,
                parent = %parent_digest.0,
                next_block_number,
                ?rejection,
                "dropping stale proposal before Engine API work"
            );
            return Ok(Some(ProposeOutcome::EpochStale));
        }

        let required_parent = ProjectionCheckpoint {
            block_number: parent_height.get(),
            block_hash: parent_digest.0,
        };
        if wait_for_projected_parent(
            self.projection_readiness.clone(),
            required_parent,
            std::future::pending(),
        )
        .await?
            == ParentProjectionGate::Withhold
        {
            return Ok(Some(ProposeOutcome::ProjectionUnavailable));
        }

        Ok(None)
    }
    pub(super) async fn import_proposal_parent(
        &self,
        parent: &ProposalParent,
        execution_read_budget: ExecutionReadBudget,
    ) -> eyre::Result<()> {
        let parent_height = parent.height;
        let parent_digest = parent.digest;
        if let Some(parent_block) = parent.block.as_ref() {
            // Step 2: Send parent to execution layer via new_payload.
            let execution_data =
                OutbeExecutionData::new(std::sync::Arc::new(parent_block.clone().into_inner()))
                    .with_execution_read_budget(execution_read_budget);

            if crate::test_faults::should_drop_new_payload_for_test(parent_height) {
                warn!(
                    height = %parent_height,
                    parent = %parent_digest.0,
                    "test-marshal-drop: skipping propose parent new_payload"
                );
            } else {
                match self.engine.new_payload(execution_data).await {
                    Ok(status) if status.is_valid() || status.is_syncing() => {
                        debug!(parent = %parent_digest.0, ?status, "parent verified by execution layer");
                    }
                    Ok(status) => {
                        return Err(eyre::eyre!(
                            "parent {} rejected by execution layer: {status:?}",
                            parent_digest.0
                        ));
                    }
                    Err(e) => {
                        return Err(eyre::eyre!(
                            "new_payload failed for parent {}: {e}",
                            parent_digest.0
                        ));
                    }
                }
            }

            self.finalization_view
                .advance_timestamp_floor(parent_block.timestamp_millis());
        }

        Ok(())
    }
}
