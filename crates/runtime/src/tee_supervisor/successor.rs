use super::*;
use crate::admission::TeeLeaseGuardGateV1;
use outbe_tee::{lifecycle::successor::*, registry_view::NodeBindingSelectorV1};

pub(super) struct ResidentSuccessor {
    pub(super) candidate: Arc<InProcessTeeContext>,
    pub(super) attempt: SuccessorAttemptV1,
    pub(super) activation: Option<LifecycleCheckpointV1>,
    pub(super) cold: bool,
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
            validate_activation_checkpoint(&pending.candidate, view.checkpoint)?;
            reconcile_attempt(
                &self.port,
                &self.signer,
                (&self.live, &pending.candidate),
                &mut self.journal,
            )
            .await?;
            pending.activation.get_or_insert(view.checkpoint);
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
                (lease, pending.cold),
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
        if self.journal.load()?.is_some() {
            let status =
                reconcile_successor_v1(&self.port, &self.signer, &self.live, &mut self.journal)
                    .await?;
            if matches!(
                status,
                SuccessorStep::AwaitingFinality | SuccessorStep::Submitted { .. }
            ) {
                return Ok(());
            }
        }
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
        let cold = cold_successor(&self.live, view, &self.node_data_dir)?;
        if cold {
            write_cold_marker(&self.node_data_dir)?;
        }
        // Cold recovery of an expired own binding must not reuse that enclave id.
        // A new attempt also starts with offer_key_ready == false.
        let candidate = fresh_candidate(&self.live, self.descriptor.clone())?;
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
        let (attempt, step) = prepare_candidate(
            &self.port,
            &self.signer,
            (&self.live, &candidate),
            &mut self.journal,
            (lease, cold),
        )
        .await?;
        if step == SuccessorStep::AwaitingFinality {
            return Ok(());
        }
        self.successor = Some(ResidentSuccessor {
            candidate,
            attempt,
            activation: None,
            cold,
        });
        self.phase = RenewalUpgradePhaseV1::CandidatePrepared;
        Ok(())
    }

    fn persist_candidate(
        &self,
        successor: &ResidentSuccessor,
    ) -> Result<LocalTeeAdmissionAnchorV1, SuccessorRuntimeError> {
        let (record, local) = finalized_join_anchor(successor)?;
        outbe_tee::lifecycle::enrollment::persist_live_join_admission_anchor_v1(
            &self.node_data_dir,
            &successor.candidate,
            record,
        )?;
        successor
            .candidate
            .persist_for_promotion(&self.node_data_dir)?;
        Ok(local)
    }

    pub(super) fn cold_finalized(&self) -> bool {
        self.successor
            .as_ref()
            .is_some_and(|pending| pending.activation.is_some() && pending.cold)
    }

    pub(super) fn complete_cold_recovery(
        &mut self,
        owner: Option<&LiveTeeRuntimeV1>,
    ) -> Result<LocalTeeAdmissionAnchorV1, SuccessorRuntimeError> {
        let mut owner_guard = owner.map(LiveTeeRuntimeV1::lock).transpose()?;
        let (record, anchor, candidate, enclave_id, phase, marked) = {
            let pending = self
                .successor
                .as_ref()
                .ok_or(SuccessorRuntimeError::MissingCandidate)?;
            let cold = pending.cold;
            if cold {
                ensure_marker_clearable(&self.node_data_dir, false)?;
            }
            let (record, anchor) = finalized_join_anchor(pending)?;
            let candidate = pending.candidate.clone();
            let enclave_id = candidate.manifest().enclave_id()?;
            let phase = RenewalUpgradePhaseV1::Promoted {
                candidate_manifest_hash: candidate.manifest().authorization_hash()?,
                successor_policy_hash: pending.attempt.prepare_intent.policy_hash,
            };
            let marked = cold && cold_recovery_marked(&self.node_data_dir);
            (record, anchor, candidate, enclave_id, phase, marked)
        };
        // A restarted process keeps the previous enclave's anchor.bin. Do not
        // load it through this enclave. persist overwrites a same-host anchor
        // whose height and timestamp are strictly older. replace runs that
        // write only when the installed context is still self.live, and swaps
        // the context only after the closure returns Ok.
        if outbe_tee::in_process_tee_context().is_none() {
            outbe_tee::install_in_process_tee_context(Arc::clone(&self.live))
                .map_err(SuccessorRuntimeError::Install)?;
        }
        outbe_tee::replace_in_process_tee_context(&self.live, candidate.clone(), || {
            outbe_tee::lifecycle::enrollment::persist_live_join_admission_anchor_v1(
                &self.node_data_dir,
                &candidate,
                record,
            )?;
            candidate.persist_for_promotion(&self.node_data_dir)?;
            if let (Some(owner), Some(snapshot)) = (owner, owner_guard.as_deref_mut()) {
                // Do not expose the old enclave's open verdict after switching dispatch.
                crate::admission_authority::publish_state(
                    &owner.authority,
                    outbe_tee::TeeAuthorityState::SuspendedPolicyMismatch,
                );
                owner
                    .authority
                    .note_observed_finalized_height(anchor.finalized_height);
                snapshot.identity.expected_enclave_id = Some(enclave_id);
                snapshot.context = candidate.clone();
                snapshot.gate = TeeLeaseGuardGateV1::Pending {
                    target: anchor,
                    floor: None,
                };
            }
            Ok::<(), SuccessorRuntimeError>(())
        })?;
        self.live = candidate;
        self.phase = phase;
        self.successor = None;
        if marked {
            if let Err(error) = clear_cold_marker(&self.node_data_dir) {
                tracing::warn!(
                    ?error,
                    "deferred cold-recovery marker cleanup after committed promotion"
                );
            }
        }
        Ok(anchor)
    }

    pub(super) async fn owns_live_binding_now(&self) -> Result<bool, SuccessorRuntimeError> {
        let at = self.port.finalized_checkpoint().await?;
        let view = self
            .port
            .registry_state(
                at,
                &NodeBindingSelectorV1::Validator(self.signer.operator.address()),
            )
            .await?;
        owns_live_binding(&self.live, view.binding.as_ref(), view.checkpoint.timestamp)
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
        if pending.activation.is_none() {
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
    (lease, cold): (SuccessorLeaseV1, bool),
) -> Result<(SuccessorAttemptV1, SuccessorStep), SuccessorRuntimeError> {
    Ok(if cold {
        prepare_cold_successor_v1(port, signer, candidate, journal, lease).await?
    } else {
        prepare_successor_v1(port, signer, (source, candidate), journal, lease).await?
    })
}

fn cold_successor(
    live: &InProcessTeeContext,
    view: &LifecycleRegistryStateV1,
    node_data_dir: &std::path::Path,
) -> Result<bool, SuccessorRuntimeError> {
    let owns = owns_live_binding(live, view.binding.as_ref(), view.checkpoint.timestamp)?;
    if owns {
        let _ = ensure_marker_clearable(node_data_dir, true);
        return Ok(false);
    }
    Ok(cold_from_signals(owns, cold_recovery_marked(node_data_dir)))
}

/// A provisioned offer key is not a live on-chain binding.
fn cold_from_signals(owns_live_binding: bool, marker: bool) -> bool {
    marker || !owns_live_binding
}

fn owns_live_binding(
    live: &InProcessTeeContext,
    binding: Option<&outbe_tee::registry_view::RenewalBindingV1>,
    timestamp: u64,
) -> Result<bool, SuccessorRuntimeError> {
    let enclave_id = live.manifest().enclave_id()?;
    Ok(binding
        .is_some_and(|binding| binding.enclave_id == enclave_id && timestamp < binding.valid_until))
}

fn cold_marker_path(node_data_dir: &std::path::Path) -> std::path::PathBuf {
    node_data_dir.join("tee-cold-recovery-v1")
}

pub(super) fn cold_recovery_marked(node_data_dir: &std::path::Path) -> bool {
    std::fs::symlink_metadata(cold_marker_path(node_data_dir))
        .is_ok_and(|meta| meta.file_type().is_file())
}

fn write_cold_marker(node_data_dir: &std::path::Path) -> Result<(), SuccessorRuntimeError> {
    if cold_recovery_marked(node_data_dir) {
        return Ok(());
    }
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(cold_marker_path(node_data_dir))
        .map_err(SuccessorRuntimeError::Marker)?;
    file.write_all(b"cold-recovery-v1")
        .map_err(SuccessorRuntimeError::Marker)?;
    Ok(())
}

fn ensure_marker_clearable(
    node_data_dir: &std::path::Path,
    owns_live_binding: bool,
) -> Result<(), SuccessorRuntimeError> {
    if owns_live_binding {
        let _ = clear_cold_marker(node_data_dir);
        return Ok(());
    }
    let path = cold_marker_path(node_data_dir);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => Ok(()),
        Ok(_) => Err(SuccessorRuntimeError::Marker(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cold recovery marker is not a regular file",
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SuccessorRuntimeError::Marker(error)),
    }
}

fn validate_activation_checkpoint(
    candidate: &InProcessTeeContext,
    activation: LifecycleCheckpointV1,
) -> Result<outbe_tee::in_process::admission::VerifiedAdmissionAnchorV1, SuccessorRuntimeError> {
    let verified = candidate
        .admission_anchor()?
        .ok_or(SuccessorRuntimeError::MissingAdmission)?;
    let same_height = activation.height == verified.block_number;
    let preserves_checkpoint = (activation.hash, activation.state_root, activation.timestamp)
        == (
            verified.block_hash,
            verified.state_root,
            verified.consensus_timestamp,
        );
    if activation.height < verified.block_number
        || activation.timestamp < verified.consensus_timestamp
        || (same_height && !preserves_checkpoint)
    {
        return Err(SuccessorRuntimeError::ActivationCheckpointMismatch {
            activation_height: activation.height,
            proof_height: verified.block_number,
        });
    }
    Ok(verified)
}

fn finalized_join_anchor(
    successor: &ResidentSuccessor,
) -> Result<
    (
        outbe_tee::FinalizedJoinAdmissionAnchorV1,
        LocalTeeAdmissionAnchorV1,
    ),
    SuccessorRuntimeError,
> {
    let activation = successor
        .activation
        .ok_or(SuccessorRuntimeError::MissingAdmission)?;
    let verified = validate_activation_checkpoint(&successor.candidate, activation)?;
    let intent = &successor.attempt.prepare_intent;
    Ok((
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
        // Unlike the durable key-import proof, local lease admission needs
        // the checkpoint that actually contains the new active binding.
        LocalTeeAdmissionAnchorV1 {
            finalized_height: activation.height,
            finalized_hash: activation.hash,
        },
    ))
}

#[cfg(test)]
static FAIL_CLEAR_COLD_MARKER_DIRS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeSet<std::path::PathBuf>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::BTreeSet::new()));

#[cfg(test)]
pub(super) struct FailClearColdMarkerGuard(std::path::PathBuf);

#[cfg(test)]
impl FailClearColdMarkerGuard {
    pub(super) fn arm(node_data_dir: &std::path::Path) -> Self {
        let dir = node_data_dir.to_path_buf();
        FAIL_CLEAR_COLD_MARKER_DIRS
            .lock()
            .unwrap()
            .insert(dir.clone());
        Self(dir)
    }
}

#[cfg(test)]
impl Drop for FailClearColdMarkerGuard {
    fn drop(&mut self) {
        if let Ok(mut dirs) = FAIL_CLEAR_COLD_MARKER_DIRS.lock() {
            dirs.remove(&self.0);
        }
    }
}

pub(super) fn clear_cold_marker(
    node_data_dir: &std::path::Path,
) -> Result<(), SuccessorRuntimeError> {
    let path = cold_marker_path(node_data_dir);
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_file() => {
            #[cfg(test)]
            if FAIL_CLEAR_COLD_MARKER_DIRS
                .lock()
                .unwrap()
                .contains(node_data_dir)
            {
                return Err(SuccessorRuntimeError::Marker(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected marker unlink failure",
                )));
            }
            std::fs::remove_file(path).map_err(SuccessorRuntimeError::Marker)
        }
        Ok(_) => Err(SuccessorRuntimeError::Marker(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "cold recovery marker is not a regular file",
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SuccessorRuntimeError::Marker(error)),
    }
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
    #[error("successor activation at {activation_height} disagrees with verified preparation at {proof_height}")]
    ActivationCheckpointMismatch {
        activation_height: u64,
        proof_height: u64,
    },
    #[error("local finalized checkpoint changed during promotion")]
    MovingFinality,
    #[error("cold recovery marker update failed")]
    Marker(#[source] std::io::Error),
    #[error(transparent)]
    Dispatch(#[from] outbe_tee::InProcessDispatchError),
    #[error("resident TEE context install failed")]
    Install(#[source] outbe_tee::InstallError),
}

impl SuccessorRuntimeError {
    pub(super) fn is_retryable(&self) -> bool {
        match self {
            Self::Port(_) | Self::Rpc(_) | Self::Provider(_) | Self::MovingFinality => true,
            Self::Successor(SuccessorError::Lifecycle(error)) => retry::renewal(error),
            Self::Successor(_)
            | Self::Journal(_)
            | Self::Resident(_)
            | Self::Crypto(_)
            | Self::Codec(_)
            | Self::Anchor(_)
            | Self::Live(_)
            | Self::LocalAdmission(_)
            | Self::InvalidChainId
            | Self::LeaseOverflow
            | Self::MissingCandidate
            | Self::MissingAdmission
            | Self::ActivationCheckpointMismatch { .. }
            | Self::Marker(_)
            | Self::Dispatch(_)
            | Self::Install(_) => false,
        }
    }
}

#[cfg(test)]
#[path = "cold_recovery_tests.rs"]
mod cold_recovery_tests;
#[cfg(test)]
#[path = "successor_tests.rs"]
mod tests;
