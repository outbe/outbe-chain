//! Node-owned bounded OCOMP control endpoint.
//!
//! This local service exposes only the one finalized live PoC job retained by
//! [`OcompRetentionCoordinator`]. Connection or protocol failures remain local
//! to OCOMP and are never propagated into consensus/finality.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use alloy_consensus::TxEip1559;
use alloy_eips::Encodable2718;
use alloy_primitives::{Bytes, TxKind, U256};
use metrics::{counter, gauge};
use outbe_compressed_entities::CompressedTreeService;
use outbe_ocomp_protocol::local_control::{
    ControlError, ControlRole, ControlServerSession, EndpointIdentity, ServerPolicy,
};
use outbe_ocomp_protocol::{
    abi::{encode_submit_lysis_result_calldata, METADOSIS_ADDRESS},
    committee::OcompCommitteeSnapshotV1,
    common::BoundedBytes,
    AttestationResponseV1, BuildFinalizedIntentProofV1, BuildLysisOpeningsV1,
    CheckProjectionContainmentV1, CommitSnapshotExportV1, FinalizedJobSpecV1,
    FinalizedJobSummaryV1, GetJobSpecV1, GetSnapshotHandoffV1, ListFinalizedJobsResponseV1,
    ListFinalizedJobsV1, ListSnapshotHandoffsV1, LocalErrorCode, LocalErrorV1, NodeMessageKind,
    OpenSnapshotLeaseV1, PrepareVoteTransactionV1, PreparedVoteTransactionV1, ProtocolError,
    RenewSnapshotLeaseV1, RequestAttestationV1, SchemaLimits,
};
use outbe_primitives::signer::{SharedOutbeEvmSigner, SignerError};
use thiserror::Error;

use super::attestation::{
    AtomicHeightSource, AttestationAuthorityError, AttestationError, CurrentHeightSource,
    OcompAttestationConfig, OcompAttestationGate,
};
use super::retention::{FinalizedJobPinV1, OcompRetentionCoordinator, RetentionError};
use super::sign_once::{SignOnceError, SignOnceStore};
use super::signer::OcompSigner;
use super::snapshot_control::{
    ProjectionContainmentAuthority, SnapshotExportAuthority, SnapshotExportError,
};

#[derive(Default)]
struct ReadinessState {
    ready: AtomicBool,
    compatible_sessions: AtomicU64,
    incompatible_sessions: AtomicU64,
    failed_sessions: AtomicU64,
}

#[derive(Clone, Default)]
pub struct OcompControlReadiness {
    state: Arc<ReadinessState>,
}

impl OcompControlReadiness {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state.ready.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn compatible_sessions(&self) -> u64 {
        self.state.compatible_sessions.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn incompatible_sessions(&self) -> u64 {
        self.state.incompatible_sessions.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn failed_sessions(&self) -> u64 {
        self.state.failed_sessions.load(Ordering::Relaxed)
    }

    fn compatible(&self) {
        self.state.ready.store(true, Ordering::Release);
        self.state
            .compatible_sessions
            .fetch_add(1, Ordering::Relaxed);
        gauge!("outbe_ocomp_ready").set(1.0);
        counter!("outbe_ocomp_control_sessions_total", "result" => "compatible").increment(1);
    }

    fn incompatible(&self) {
        self.state.ready.store(false, Ordering::Release);
        self.state
            .incompatible_sessions
            .fetch_add(1, Ordering::Relaxed);
        gauge!("outbe_ocomp_ready").set(0.0);
        counter!("outbe_ocomp_control_sessions_total", "result" => "incompatible_bundle")
            .increment(1);
    }

    fn failed(&self) {
        self.state.failed_sessions.fetch_add(1, Ordering::Relaxed);
        counter!("outbe_ocomp_control_sessions_total", "result" => "failed").increment(1);
    }
}

pub struct OcompControlServer {
    retention: Arc<OcompRetentionCoordinator>,
    identity: EndpointIdentity,
    session_generation: u64,
    limits: SchemaLimits,
    readiness: OcompControlReadiness,
    snapshot_export: Option<Arc<SnapshotExportAuthority>>,
    attestation: Option<Arc<OcompAttestationGate>>,
    attestation_height: Option<Arc<AtomicHeightSource>>,
    vote_transaction_signer: Option<SharedOutbeEvmSigner>,
}

#[derive(Clone, Debug)]
pub struct OcompNodeAttestationConfig {
    pub key_path: PathBuf,
    pub sign_once_root: PathBuf,
    pub validator_index: u8,
    pub committee: OcompCommitteeSnapshotV1,
    pub initial_height: u64,
}

impl OcompControlServer {
    pub fn new(
        retention: Arc<OcompRetentionCoordinator>,
        identity: EndpointIdentity,
        session_generation: u64,
        limits: SchemaLimits,
    ) -> Result<Self, NodeControlError> {
        if session_generation == 0 {
            return Err(NodeControlError::ZeroSessionGeneration);
        }
        Ok(Self {
            retention,
            identity,
            session_generation,
            limits,
            readiness: OcompControlReadiness::default(),
            snapshot_export: None,
            attestation: None,
            attestation_height: None,
            vote_transaction_signer: None,
        })
    }

    /// Enables the node-owned result attestation gate on the existing
    /// supervisor endpoint. The control caller still supplies only canonical
    /// `LysisResultV1` bytes.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_attestation(mut self, attestation: Arc<OcompAttestationGate>) -> Self {
        self.attestation = Some(attestation);
        self
    }

    /// Loads the node-owned OCOMP key and immutable sign-once store behind the
    /// closed attestation gate. The caller supplies configuration and canonical
    /// height, never a digest, purpose, signer, or signing closure.
    pub fn with_node_attestation(
        self,
        config: OcompNodeAttestationConfig,
    ) -> Result<Self, NodeControlError> {
        let height = Arc::new(AtomicHeightSource::new(config.initial_height));
        let mut server = self.with_node_attestation_height_source(config, height.clone())?;
        server.attestation_height = Some(height);
        Ok(server)
    }

    /// Production variant whose height is reloaded from canonical node state
    /// immediately before signing.
    pub fn with_node_attestation_height_source(
        mut self,
        config: OcompNodeAttestationConfig,
        height: Arc<dyn CurrentHeightSource>,
    ) -> Result<Self, NodeControlError> {
        let signer = OcompSigner::from_file(&config.key_path).map_err(AttestationError::from)?;
        let sign_once = SignOnceStore::open(config.sign_once_root, self.limits)
            .map_err(AttestationError::from)?;
        let gate = OcompAttestationGate::new(
            self.retention.clone(),
            height,
            OcompAttestationConfig {
                identity: self.identity,
                validator_index: config.validator_index,
            },
            config.committee,
            signer,
            sign_once,
            self.limits,
        )?;
        self.attestation = Some(Arc::new(gate));
        Ok(self)
    }

    /// Installs the already validated validator EVM signer behind the
    /// result-vote-only transaction constructor.
    #[must_use]
    pub fn with_vote_transaction_signer(mut self, signer: SharedOutbeEvmSigner) -> Self {
        self.vote_transaction_signer = Some(signer);
        self
    }

    /// Advances the monotonic canonical-height view checked immediately before
    /// the irreversible sign-once transition.
    pub fn advance_ocomp_height(&self, height: u64) -> Result<(), NodeControlError> {
        self.attestation_height
            .as_deref()
            .ok_or(NodeControlError::AttestationUnavailable)?
            .advance_to(height);
        Ok(())
    }

    /// Enables the separate fixed-role snapshot-exporter endpoint without
    /// changing the existing supervisor discovery interface.
    #[must_use]
    pub fn with_snapshot_export(
        self,
        tree: Arc<CompressedTreeService>,
        projection_containment: Arc<dyn ProjectionContainmentAuthority>,
    ) -> Self {
        let authority = Arc::new(SnapshotExportAuthority::new(
            Arc::clone(&self.retention),
            tree,
            projection_containment,
            self.identity.boot_nonce,
            self.limits,
        ));
        self.with_snapshot_export_authority(authority)
    }

    /// Installs the same node-owned snapshot authority used by finalized-job
    /// reconciliation, so a stopped Supervisor cannot miss the exact CE marker.
    #[must_use]
    pub fn with_snapshot_export_authority(
        mut self,
        authority: Arc<SnapshotExportAuthority>,
    ) -> Self {
        self.snapshot_export = Some(authority);
        self
    }

    #[must_use]
    pub fn readiness(&self) -> OcompControlReadiness {
        self.readiness.clone()
    }

    /// Runs a node-local listener until the owner requests shutdown. Individual
    /// connection failures are contained and counted; only listener failure is
    /// returned to the owning background task.
    pub fn serve_until(&self, listener: UnixListener, shutdown: &AtomicBool) -> io::Result<()> {
        self.serve_role_until(listener, shutdown, ControlRole::Supervisor)
    }

    /// Runs the fixed SnapshotExporter role on its own UDS listener.
    pub fn serve_snapshot_exporter_until(
        &self,
        listener: UnixListener,
        shutdown: &AtomicBool,
    ) -> io::Result<()> {
        self.serve_role_until(listener, shutdown, ControlRole::SnapshotExporter)
    }

    fn serve_role_until(
        &self,
        listener: UnixListener,
        shutdown: &AtomicBool,
        peer_role: ControlRole,
    ) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        while !shutdown.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Err(error) = self.serve_connection_for_role(stream, peer_role) {
                        if !matches!(
                            error,
                            NodeControlError::Control(ControlError::NoCommonBundle)
                        ) {
                            tracing::warn!(
                                target: "outbe::ocomp",
                                ?peer_role,
                                %error,
                                "OCOMP node local-control session failed"
                            );
                            self.readiness.failed();
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn serve_connection(&self, stream: UnixStream) -> Result<(), NodeControlError> {
        self.serve_connection_for_role(stream, ControlRole::Supervisor)
    }

    pub fn serve_snapshot_exporter_connection(
        &self,
        stream: UnixStream,
    ) -> Result<(), NodeControlError> {
        self.serve_connection_for_role(stream, ControlRole::SnapshotExporter)
    }

    fn serve_connection_for_role(
        &self,
        stream: UnixStream,
        peer_role: ControlRole,
    ) -> Result<(), NodeControlError> {
        match peer_role {
            ControlRole::Supervisor => {}
            ControlRole::SnapshotExporter => {
                if self.snapshot_export.is_none() {
                    return Err(NodeControlError::SnapshotExportUnavailable);
                }
            }
            _ => return Err(NodeControlError::UnsupportedPeerRole(peer_role)),
        }
        let mut session = ControlServerSession::accept(
            stream,
            ServerPolicy::node(
                peer_role,
                self.identity,
                self.session_generation,
                self.limits,
            ),
        )?;
        match session.handshake() {
            Ok(_) => self.readiness.compatible(),
            Err(ControlError::NoCommonBundle) => {
                self.readiness.incompatible();
                return Err(NodeControlError::Control(ControlError::NoCommonBundle));
            }
            Err(error) => return Err(error.into()),
        }

        loop {
            let frame = match session.receive_request() {
                Ok(frame) => frame,
                Err(ControlError::ConnectionClosed) => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            let response = match frame.message_kind {
                kind if kind == NodeMessageKind::ListFinalizedJobs as u16 => {
                    let request = ListFinalizedJobsV1::decode_body(&frame.body, &self.limits)?;
                    self.list_finalized_jobs(&request)?
                        .encode_body(&self.limits)?
                }
                kind if kind == NodeMessageKind::GetJobSpec as u16 => {
                    let request = GetJobSpecV1::decode_body(&frame.body, &self.limits)?;
                    match self.get_job_spec(request.job_id) {
                        Ok(spec) => spec.encode_body(&self.limits)?,
                        Err(NodeControlError::JobNotFound) => {
                            let error = LocalErrorV1 {
                                rejected_kind: frame.message_kind,
                                error_code: LocalErrorCode::NotFound as u16,
                                retryable: false,
                            };
                            session.send_response(
                                frame.request_id,
                                NodeMessageKind::Error as u16,
                                error.encode_body(&self.limits)?,
                            )?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                }
                kind if kind == NodeMessageKind::RequestAttestation as u16 => {
                    let request = match RequestAttestationV1::decode_body(&frame.body, &self.limits)
                    {
                        Ok(request) => request,
                        Err(_) => {
                            send_local_error(
                                &mut session,
                                frame.request_id,
                                frame.message_kind,
                                LocalErrorCode::Malformed,
                                false,
                                &self.limits,
                            )?;
                            continue;
                        }
                    };
                    match self.request_attestation(&request) {
                        Ok(response) => response.encode_body(&self.limits)?,
                        Err(error) => {
                            let (error_code, retryable) = attestation_local_error(&error);
                            send_local_error(
                                &mut session,
                                frame.request_id,
                                frame.message_kind,
                                error_code,
                                retryable,
                                &self.limits,
                            )?;
                            continue;
                        }
                    }
                }
                kind if kind == NodeMessageKind::PrepareVoteTransaction as u16 => {
                    let request =
                        match PrepareVoteTransactionV1::decode_body(&frame.body, &self.limits) {
                            Ok(request) => request,
                            Err(_) => {
                                send_local_error(
                                    &mut session,
                                    frame.request_id,
                                    frame.message_kind,
                                    LocalErrorCode::Malformed,
                                    false,
                                    &self.limits,
                                )?;
                                continue;
                            }
                        };
                    match self.prepare_vote_transaction(&request) {
                        Ok(response) => response.encode_body(&self.limits)?,
                        Err(error) => {
                            let (error_code, retryable) = attestation_local_error(&error);
                            send_local_error(
                                &mut session,
                                frame.request_id,
                                frame.message_kind,
                                error_code,
                                retryable,
                                &self.limits,
                            )?;
                            continue;
                        }
                    }
                }
                kind if kind == NodeMessageKind::OpenSnapshotLease as u16 => {
                    let request = OpenSnapshotLeaseV1::decode_body(&frame.body, &self.limits)?;
                    self.snapshot_export()?
                        .open(request.job_id)?
                        .encode_body(&self.limits)?
                }
                kind if kind == NodeMessageKind::ListSnapshotHandoffs as u16 => {
                    let request = ListSnapshotHandoffsV1::decode_body(&frame.body, &self.limits)?;
                    self.snapshot_export()?
                        .list(&request)?
                        .encode_body(&self.limits)?
                }
                kind if kind == NodeMessageKind::GetSnapshotHandoff as u16 => {
                    let request = GetSnapshotHandoffV1::decode_body(&frame.body, &self.limits)?;
                    self.snapshot_export()?
                        .get(&request)?
                        .encode_body(&self.limits)?
                }
                kind if kind == NodeMessageKind::RenewSnapshotLease as u16 => {
                    let request = RenewSnapshotLeaseV1::decode_body(&frame.body, &self.limits)?;
                    self.snapshot_export()?
                        .acknowledge_open(&request)?
                        .encode_body(&self.limits)?
                }
                kind if kind == NodeMessageKind::BuildFinalizedIntentProof as u16 => {
                    let request =
                        BuildFinalizedIntentProofV1::decode_body(&frame.body, &self.limits)?;
                    self.snapshot_export()?
                        .build_finalized_intent_proof(request.job_id)?
                        .encode_body(&self.limits)?
                }
                kind if kind == NodeMessageKind::BuildLysisOpenings as u16 => {
                    let request = BuildLysisOpeningsV1::decode_body(&frame.body, &self.limits)?;
                    self.snapshot_export()?
                        .build_lysis_openings(request.job_id, request.subjects)?
                        .encode_body(&self.limits)?
                }
                kind if kind == NodeMessageKind::CheckProjectionContainment as u16 => {
                    let request =
                        CheckProjectionContainmentV1::decode_body(&frame.body, &self.limits)?;
                    self.snapshot_export()?
                        .check_projection_containment(&request)?
                        .encode_body(&self.limits)?
                }
                kind if kind == NodeMessageKind::CommitSnapshotExport as u16 => {
                    let request = CommitSnapshotExportV1::decode_body(&frame.body, &self.limits)?;
                    self.snapshot_export()?
                        .commit(&request)?
                        .encode_body(&self.limits)?
                }
                actual => return Err(NodeControlError::UnexpectedMethod(actual)),
            };
            send_bounded_response(
                &mut session,
                frame.request_id,
                frame.message_kind,
                response,
                &self.limits,
            )?;
        }
    }

    fn snapshot_export(&self) -> Result<&SnapshotExportAuthority, NodeControlError> {
        self.snapshot_export
            .as_deref()
            .ok_or(NodeControlError::SnapshotExportUnavailable)
    }

    fn attestation(&self) -> Result<&OcompAttestationGate, NodeControlError> {
        self.attestation
            .as_deref()
            .ok_or(NodeControlError::AttestationUnavailable)
    }

    pub fn request_attestation(
        &self,
        request: &RequestAttestationV1,
    ) -> Result<AttestationResponseV1, NodeControlError> {
        let vote = self
            .attestation()?
            .attest_canonical_result(&request.canonical_result.0)?;
        Ok(AttestationResponseV1 {
            canonical_vote: BoundedBytes(vote.encode_canonical(&self.limits)?),
        })
    }

    pub fn prepare_vote_transaction(
        &self,
        request: &PrepareVoteTransactionV1,
    ) -> Result<PreparedVoteTransactionV1, NodeControlError> {
        let max_fee_per_gas = u128::try_from(request.max_fee_per_gas).map_err(|_| {
            ProtocolError::InvalidInvariant("restricted result-vote max fee does not fit u128")
        })?;
        if request.gas_limit != outbe_zerofee::MAX_ZERO_FEE_OCOMP_GAS_LIMIT
            || max_fee_per_gas < outbe_zerofee::MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS
        {
            return Err(ProtocolError::InvalidInvariant(
                "restricted result-vote transaction fee envelope",
            )
            .into());
        }
        let signer = self
            .vote_transaction_signer
            .as_deref()
            .ok_or(NodeControlError::VoteTransactionSignerUnavailable)?;
        let vote = self
            .attestation()?
            .attest_canonical_result(&request.canonical_result.0)?;
        let canonical_vote = vote.encode_canonical(&self.limits)?;
        let calldata = encode_submit_lysis_result_calldata(&vote, &self.limits)?;
        if calldata.len() > outbe_zerofee::MAX_ZERO_FEE_OCOMP_CALLDATA_BYTES {
            return Err(ProtocolError::InvalidInvariant(
                "restricted result-vote transaction calldata cap",
            )
            .into());
        }
        let unsigned = TxEip1559 {
            chain_id: self.identity.chain_id,
            nonce: request.nonce,
            gas_limit: request.gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(METADOSIS_ADDRESS),
            value: U256::ZERO,
            input: Bytes::from(calldata),
            access_list: Default::default(),
        };
        let signed = signer.sign_eip1559(unsigned)?;
        let transaction_hash = *signed.hash();
        let mut raw_transaction = Vec::new();
        raw_transaction
            .try_reserve_exact(signed.encode_2718_len())
            .map_err(|_| ProtocolError::InvalidInvariant("result-vote transaction allocation"))?;
        signed.encode_2718(&mut raw_transaction);
        Ok(PreparedVoteTransactionV1 {
            canonical_vote: BoundedBytes(canonical_vote),
            raw_transaction: BoundedBytes(raw_transaction),
            transaction_hash,
        })
    }

    fn list_finalized_jobs(
        &self,
        request: &ListFinalizedJobsV1,
    ) -> Result<ListFinalizedJobsResponseV1, NodeControlError> {
        let mut jobs = self
            .retention
            .finalized_live_jobs()?
            .into_iter()
            .map(summary)
            .filter(|summary| summary.cursor > request.after_cursor)
            .collect::<Vec<_>>();
        jobs.sort_by_key(|summary| (summary.cursor, summary.job_id));
        jobs.truncate(usize::from(request.limit));
        let next_cursor = jobs
            .last()
            .map_or(request.after_cursor, |summary| summary.cursor);
        Ok(ListFinalizedJobsResponseV1 { next_cursor, jobs })
    }

    fn get_job_spec(
        &self,
        job_id: alloy_primitives::B256,
    ) -> Result<FinalizedJobSpecV1, NodeControlError> {
        let pin = self
            .retention
            .finalized_live_jobs()?
            .into_iter()
            .find(|pin| pin.job_id == job_id)
            .ok_or(NodeControlError::JobNotFound)?;
        let proof = self.retention.build_finalized_intent_proof(job_id)?;
        let intent = proof.decoded_intent(&self.limits)?;
        let canonical_job_intent = BoundedBytes(intent.encode_canonical(&self.limits)?);
        Ok(FinalizedJobSpecV1 {
            summary: summary(pin),
            canonical_job_intent,
        })
    }
}

fn send_local_error(
    session: &mut ControlServerSession,
    request_id: u64,
    rejected_kind: u16,
    error_code: LocalErrorCode,
    retryable: bool,
    limits: &SchemaLimits,
) -> Result<(), NodeControlError> {
    let error = LocalErrorV1 {
        rejected_kind,
        error_code: error_code as u16,
        retryable,
    };
    session.send_response(
        request_id,
        NodeMessageKind::Error as u16,
        error.encode_body(limits)?,
    )?;
    Ok(())
}

pub(super) fn send_bounded_response(
    session: &mut ControlServerSession,
    request_id: u64,
    request_kind: u16,
    response: Vec<u8>,
    limits: &SchemaLimits,
) -> Result<(), NodeControlError> {
    if response.len() > limits.max_control_body_bytes {
        return send_local_error(
            session,
            request_id,
            request_kind,
            LocalErrorCode::LimitExceeded,
            false,
            limits,
        );
    }
    session.send_response(request_id, NodeMessageKind::Response as u16, response)?;
    Ok(())
}

fn attestation_local_error(error: &NodeControlError) -> (LocalErrorCode, bool) {
    match error {
        NodeControlError::AttestationUnavailable => {
            (LocalErrorCode::InternalOcompUnavailable, true)
        }
        NodeControlError::Attestation(AttestationError::Authority(
            AttestationAuthorityError::NotExported(_),
        )) => (LocalErrorCode::NotFound, false),
        NodeControlError::Attestation(
            AttestationError::Authority(
                AttestationAuthorityError::Unavailable(_) | AttestationAuthorityError::Retention(_),
            )
            | AttestationError::Height(_),
        ) => (LocalErrorCode::SourceUnavailable, true),
        NodeControlError::Attestation(AttestationError::AuthorityChanged) => {
            (LocalErrorCode::Conflict, true)
        }
        NodeControlError::Attestation(
            AttestationError::DeadlineReached { .. } | AttestationError::LocalMemberInactive { .. },
        ) => (LocalErrorCode::Conflict, false),
        NodeControlError::Attestation(AttestationError::SignOnce(
            SignOnceError::Equivocation { .. },
        )) => (LocalErrorCode::Conflict, false),
        NodeControlError::Attestation(AttestationError::SignOnce(
            SignOnceError::Disabled(_)
            | SignOnceError::UnsafeStore { .. }
            | SignOnceError::UncertainState { .. }
            | SignOnceError::CorruptRecord { .. },
        ))
        | NodeControlError::Attestation(AttestationError::Signer(_)) => {
            (LocalErrorCode::InternalOcompUnavailable, false)
        }
        NodeControlError::Attestation(AttestationError::SignOnce(
            SignOnceError::SigningFailed(_) | SignOnceError::Io { .. },
        )) => (LocalErrorCode::InternalOcompUnavailable, true),
        NodeControlError::Attestation(
            AttestationError::Binding(_)
            | AttestationError::Protocol(_)
            | AttestationError::Authority(AttestationAuthorityError::Protocol(_)),
        )
        | NodeControlError::Protocol(_) => (LocalErrorCode::Malformed, false),
        NodeControlError::VoteTransactionSignerUnavailable | NodeControlError::EvmSigner(_) => {
            (LocalErrorCode::InternalOcompUnavailable, false)
        }
        _ => (LocalErrorCode::InternalOcompUnavailable, false),
    }
}

fn summary(pin: FinalizedJobPinV1) -> FinalizedJobSummaryV1 {
    FinalizedJobSummaryV1 {
        cursor: pin.candidate.block_number,
        job_id: pin.job_id,
        intent_id: pin.candidate.intent_id,
        finalized_block_hash: pin.candidate.block_hash,
        finalized_state_root: pin.candidate.state_root,
        protocol_bundle_hash: pin.candidate.protocol_bundle_hash,
    }
}

#[derive(Debug, Error)]
pub enum NodeControlError {
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Retention(#[from] RetentionError),
    #[error(transparent)]
    SnapshotExport(#[from] SnapshotExportError),
    #[error(transparent)]
    Attestation(#[from] AttestationError),
    #[error(transparent)]
    EvmSigner(#[from] SignerError),
    #[error("node OCOMP control received unsupported method {0:#06x}")]
    UnexpectedMethod(u16),
    #[error("requested finalized OCOMP job is not live")]
    JobNotFound,
    #[error("node OCOMP snapshot export control is not configured")]
    SnapshotExportUnavailable,
    #[error("node OCOMP attestation gate is not configured")]
    AttestationUnavailable,
    #[error("node OCOMP result-vote transaction signer is not configured")]
    VoteTransactionSignerUnavailable,
    #[error("node OCOMP control cannot serve peer role {0:?}")]
    UnsupportedPeerRole(ControlRole),
    #[error("node OCOMP control session generation cannot be zero")]
    ZeroSessionGeneration,
}
