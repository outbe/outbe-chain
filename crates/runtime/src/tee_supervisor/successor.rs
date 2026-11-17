use super::*;
use outbe_tee::{lifecycle::successor::*, registry_view::NodeBindingSelectorV1};

pub(super) struct ResidentSuccessor {
    candidate: Arc<InProcessTeeContext>,
    attempt: SuccessorAttemptV1,
    pub(super) finalized: bool,
}

impl TeeLifecycleSupervisor {
    pub(super) async fn advance_successor(&mut self) -> Result<(), SuccessorRuntimeError> {
        let at = self.port.finalized_checkpoint().await?;
        let view = self
            .port
            .registry_state(
                at,
                &NodeBindingSelectorV1::Validator(self.signer.operator.address()),
            )
            .await?;
        if self.successor.is_none() {
            return self.begin_successor(&view).await;
        }
        let pending = self
            .successor
            .as_mut()
            .ok_or(SuccessorRuntimeError::MissingCandidate)?;
        if view.binding.as_ref().is_some_and(|b| {
            b.enclave_id == pending.attempt.prepare_intent.enclave_id
                && b.binding_id == pending.attempt.prepare_intent.binding_id
        }) {
            reconcile_attempt(
                &self.port,
                &self.signer,
                (&self.live, &pending.candidate),
                &mut self.journal,
            )
            .await?;
            pending.finalized = true;
            self.phase = RenewalUpgradePhaseV1::Finalized;
            return Ok(());
        }
        let status = reconcile_attempt(
            &self.port,
            &self.signer,
            (&self.live, &pending.candidate),
            &mut self.journal,
        )
        .await?;
        if matches!(
            status,
            SuccessorStep::AwaitingFinality | SuccessorStep::Submitted { .. }
        ) {
            return Ok(());
        }
        if let SuccessorStep::CancelRequired { context_hash } =
            reconcile_candidate_v1(&view, Some(&pending.attempt), Some(&pending.candidate))
        {
            cancel_successor_v1(
                &self.port,
                &self.signer,
                &self.live,
                &mut self.journal,
                context_hash,
            )
            .await?;
            self.successor = None;
            self.phase = RenewalUpgradePhaseV1::Idle;
            return Ok(());
        }
        if matches!(status, SuccessorStep::RebuildRequired { .. })
            && self
                .journal
                .load()?
                .is_some_and(|r| r.kind == ServiceTxKindV1::UpgradePrepare)
        {
            let lease = SuccessorLeaseV1 {
                binding_id: pending.attempt.prepare_intent.binding_id,
                valid_until: pending.attempt.prepare_intent.requested_valid_until,
            };
            let (attempt, _) = prepare_candidate(
                &self.port,
                &self.signer,
                (&self.live, &pending.candidate),
                &mut self.journal,
                lease,
            )
            .await?;
            pending.attempt = attempt;
            return Ok(());
        }
        if view.candidate.is_some() {
            if !pending.candidate.offer_key_ready()? {
                let proof = self
                    .port
                    .upgrade_proof(at, &pending.attempt.context)
                    .await?;
                provision_successor_v1(
                    &self.port,
                    &self.signer,
                    &pending.candidate,
                    &pending.attempt,
                    &proof,
                )
                .await?;
                self.phase = RenewalUpgradePhaseV1::CandidateKeyReady;
            }
            transition_successor_v1(
                &self.port,
                &self.signer,
                &pending.candidate,
                &mut self.journal,
                &pending.attempt,
            )
            .await?;
            self.phase = RenewalUpgradePhaseV1::Submitted;
        }
        Ok(())
    }

    async fn begin_successor(
        &mut self,
        view: &LifecycleRegistryStateV1,
    ) -> Result<(), SuccessorRuntimeError> {
        let at = view.checkpoint;
        if let SuccessorStep::CancelRequired { context_hash } =
            reconcile_candidate_v1(view, None, None)
        {
            cancel_successor_v1(
                &self.port,
                &self.signer,
                &self.live,
                &mut self.journal,
                context_hash,
            )
            .await?;
            return Ok(());
        }
        let cold = !self.live.offer_key_ready()?;
        let candidate = if cold {
            self.live.clone()
        } else {
            fresh_candidate(&self.live, self.descriptor.clone())?
        };
        let policy = view
            .successor_policy
            .as_ref()
            .map(|s| &s.policy)
            .unwrap_or(&view.policy);
        let lease = SuccessorLeaseV1 {
            binding_id: engine::binding_id(&candidate, view)?,
            valid_until: at
                .timestamp
                .checked_add(policy.maximum_lease)
                .ok_or(SuccessorRuntimeError::LeaseOverflow)?,
        };
        let (attempt, _) = prepare_candidate(
            &self.port,
            &self.signer,
            (&self.live, &candidate),
            &mut self.journal,
            lease,
        )
        .await?;
        self.successor = Some(ResidentSuccessor {
            candidate,
            attempt,
            finalized: false,
        });
        self.phase = RenewalUpgradePhaseV1::CandidatePrepared;
        Ok(())
    }

    fn persist_candidate(
        &self,
        successor: &ResidentSuccessor,
    ) -> Result<LocalTeeAdmissionAnchorV1, SuccessorRuntimeError> {
        let verified = successor
            .candidate
            .admission_anchor()?
            .ok_or(SuccessorRuntimeError::MissingAdmission)?;
        let intent = &successor.attempt.prepare_intent;
        outbe_tee::lifecycle::enrollment::persist_live_join_admission_anchor_v1(
            &self.node_data_dir,
            &successor.candidate,
            outbe_tee::FinalizedJoinAdmissionAnchorV1 {
                chain_id: intent.chain_id,
                genesis_hash: intent.genesis_hash,
                node_id_hash: intent.node_id.node_id_hash()?,
                enclave_id: intent.enclave_id,
                intent_hash: intent.intent_hash()?,
                finalized_height: verified.block_number,
                finalized_hash: verified.block_hash,
                finalized_state_root: verified.state_root,
                finalized_consensus_timestamp: verified.consensus_timestamp,
            },
        )?;
        Ok(LocalTeeAdmissionAnchorV1 {
            finalized_height: verified.block_number,
            finalized_hash: verified.block_hash,
        })
    }

    pub(super) fn complete_cold_recovery(
        &mut self,
    ) -> Result<LocalTeeAdmissionAnchorV1, SuccessorRuntimeError> {
        let pending = self
            .successor
            .as_ref()
            .ok_or(SuccessorRuntimeError::MissingCandidate)?;
        let anchor = self.persist_candidate(pending)?;
        self.live = pending.candidate.clone();
        self.phase = RenewalUpgradePhaseV1::Promoted {
            candidate_manifest_hash: self.live.manifest().authorization_hash()?,
            successor_policy_hash: pending.attempt.prepare_intent.policy_hash,
        };
        self.successor = None;
        Ok(anchor)
    }

    pub(super) fn publish_successor<P>(
        &mut self,
        owner: &Arc<LiveTeeRuntimeV1>,
        provider: &P,
        protocol: &Arc<crate::OutbeRuntime>,
    ) -> Result<(), SuccessorRuntimeError>
    where
        P: crate::BlockIdReader
            + crate::HeaderProvider<Header = crate::OutbeHeader>
            + crate::StateProviderFactory,
    {
        let Some(pending) = &self.successor else {
            return Ok(());
        };
        if !pending.candidate.offer_key_ready()? {
            return Ok(());
        }
        owner.stage_candidate(pending.candidate.clone())?;
        if !pending.finalized {
            return Ok(());
        }
        promote_finalized_successor(owner, provider, protocol, &pending.candidate, || {
            self.persist_candidate(pending)
                .map(|_| ())
                .map_err(Into::into)
        })?;
        self.live = pending.candidate.clone();
        self.phase = RenewalUpgradePhaseV1::Promoted {
            candidate_manifest_hash: self.live.manifest().authorization_hash()?,
            successor_policy_hash: pending.attempt.prepare_intent.policy_hash,
        };
        self.successor = None;
        Ok(())
    }
}

// Verify the local finalized checkpoint before making any durable or live change.
fn promote_finalized_successor<P>(
    owner: &LiveTeeRuntimeV1,
    provider: &P,
    protocol: &crate::OutbeRuntime,
    candidate: &Arc<InProcessTeeContext>,
    persist: impl FnOnce() -> eyre::Result<()>,
) -> Result<(), SuccessorRuntimeError>
where
    P: crate::BlockIdReader
        + crate::HeaderProvider<Header = crate::OutbeHeader>
        + crate::StateProviderFactory,
{
    let head = provider
        .finalized_block_num_hash()?
        .ok_or(SuccessorRuntimeError::MissingAdmission)?;
    let header = provider
        .sealed_header(head.number)?
        .ok_or(SuccessorRuntimeError::MissingAdmission)?;
    if header.hash() != head.hash {
        return Err(SuccessorRuntimeError::MovingFinality);
    }
    let mut identity = owner.lock()?.identity;
    identity.expected_enclave_id = Some(candidate.manifest().enclave_id()?);
    let chain = outbe_execution::tee_remote_session::RegistryChainIdentity {
        chain_id: u64::try_from(alloy_primitives::U256::from_be_bytes(
            candidate.manifest().chain_id,
        ))
        .map_err(|_| SuccessorRuntimeError::InvalidChainId)?,
        genesis_hash: candidate.manifest().genesis_hash,
    };
    let admission = crate::admission::read_local_tee_admission_at_block(
        protocol,
        provider,
        chain,
        identity,
        (head, header.header().inner.timestamp),
    )
    .map_err(SuccessorRuntimeError::LocalAdmission)?;
    let key = outbe_consensus::validators::read_network_encryption_public_at_block(
        protocol, provider, head.hash,
    )
    .map_err(SuccessorRuntimeError::LocalAdmission)?;
    if provider.finalized_block_num_hash()? != Some(head) {
        return Err(SuccessorRuntimeError::MovingFinality);
    }
    owner.promote(
        candidate.clone(),
        LocalTeeAdmissionAnchorV1 {
            finalized_height: head.number,
            finalized_hash: head.hash,
        },
        admission,
        key,
        persist,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "successor_tests.rs"]
mod tests;

async fn reconcile_attempt(
    port: &rpc::RpcLifecyclePort,
    signer: &signer::RuntimeSigner,
    (source, candidate): (&InProcessTeeContext, &InProcessTeeContext),
    journal: &mut ServiceTxJournalV1,
) -> Result<SuccessorStep, SuccessorRuntimeError> {
    if engine::source_renewal_settled(port, signer, source, journal)
        .await
        .map_err(outbe_tee::lifecycle::renewal::RenewalError::Reconcile)
        .map_err(SuccessorError::Lifecycle)?
    {
        return Ok(SuccessorStep::Prepared);
    }
    Ok(reconcile_successor_v1(port, signer, candidate, journal).await?)
}

async fn prepare_candidate(
    port: &rpc::RpcLifecyclePort,
    signer: &signer::RuntimeSigner,
    (source, candidate): (&InProcessTeeContext, &InProcessTeeContext),
    journal: &mut ServiceTxJournalV1,
    lease: SuccessorLeaseV1,
) -> Result<(SuccessorAttemptV1, SuccessorStep), SuccessorRuntimeError> {
    Ok(if source.offer_key_ready()? {
        prepare_successor_v1(port, signer, (source, candidate), journal, lease).await?
    } else {
        prepare_cold_successor_v1(port, signer, candidate, journal, lease).await?
    })
}

fn fresh_candidate(
    source: &InProcessTeeContext,
    descriptor: TrustedNetworkDescriptorV1,
) -> Result<Arc<InProcessTeeContext>, SuccessorRuntimeError> {
    let proof = outbe_tee::in_process::ProofSourceV1::NoHardware;
    proof.ensure_available(source.manifest().attestation_mode)?;
    let keys = outbe_tee::resident::ResidentTeeKeys::generate()?;
    let mut manifest = source.manifest().clone();
    manifest.initialization_challenge = B256::random().0;
    manifest.recipient_x25519 = keys.tribute_offer_public();
    manifest.attestation_ed25519 = keys.attestation_pub();
    manifest.noise_responder_x25519 = keys.noise_public();
    Ok(Arc::new(InProcessTeeContext::new(
        keys,
        *source.identity(),
        manifest,
        descriptor,
        proof,
    )?))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SuccessorRuntimeError {
    #[error(transparent)]
    Port(#[from] LifecyclePortError<rpc::RpcLifecycleError>),
    #[error(transparent)]
    Rpc(#[from] rpc::RpcLifecycleError),
    #[error(transparent)]
    Journal(#[from] ServiceTxJournalError),
    #[error(transparent)]
    Successor(#[from] SuccessorError<rpc::RpcLifecycleError, signer::SigningError>),
    #[error(transparent)]
    Resident(#[from] outbe_tee::in_process::InProcessError),
    #[error(transparent)]
    Crypto(#[from] outbe_tee::resident::TeeCryptoError),
    #[error(transparent)]
    Codec(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    #[error(transparent)]
    Anchor(#[from] outbe_tee::lifecycle::enrollment::LiveJoinAnchorError),
    #[error(transparent)]
    Live(#[from] crate::admission_authority::live::LiveTeeError),
    #[error(transparent)]
    Provider(#[from] reth_provider::ProviderError),
    #[error("local finalized successor admission failed")]
    LocalAdmission(#[source] eyre::Report),
    #[error("chain id exceeds supported width")]
    InvalidChainId,
    #[error("successor lease overflow")]
    LeaseOverflow,
    #[error("resident successor candidate missing")]
    MissingCandidate,
    #[error("successor finalized admission unavailable")]
    MissingAdmission,
    #[error("local finalized checkpoint changed during promotion")]
    MovingFinality,
}

impl SuccessorRuntimeError {
    pub(super) fn is_retryable(&self) -> bool {
        match self {
            Self::Port(_) | Self::Rpc(_) | Self::Provider(_) | Self::MovingFinality => true,
            Self::Successor(SuccessorError::Lifecycle(error)) => retry::renewal(error),
            _ => false,
        }
    }
}
