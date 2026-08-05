//! Typed node client used only by the fixed snapshot-exporter role.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use alloy_primitives::B256;
use outbe_node::ocomp::finality::{
    authenticate_snapshot_handoff, SnapshotHandoffVerificationError,
};
use outbe_ocomp_protocol::local_control::{
    ClientPolicy, ControlClientSession, ControlError, EndpointIdentity,
};
use outbe_ocomp_protocol::{
    intent::{ExpectedFinalizedIntentBindingV1, VerifiedFinalizedIntentV1},
    opening::{LysisOpeningsProofV1, OpeningSubjectsV1},
    BuildFinalizedIntentProofV1, BuildLysisOpeningsV1, CheckProjectionContainmentV1,
    CommitSnapshotExportV1, FinalizedIntentProofResponseV1, GetSnapshotHandoffV1,
    ListSnapshotHandoffsResponseV1, ListSnapshotHandoffsV1, LocalErrorV1, NodeMessageKind,
    ProjectionCheckpointV1, ProjectionContainedV1, ProtocolError, RenewSnapshotLeaseV1,
    SchemaLimits, SnapshotExportCommittedV1, SnapshotHandoffV1, SnapshotLeaseOpenedV1,
    MAX_SNAPSHOT_HANDOFFS_PER_RESPONSE,
};
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct SnapshotExporterNodeConfig {
    pub node_socket: PathBuf,
    pub identity: EndpointIdentity,
    pub limits: SchemaLimits,
}

pub struct SnapshotExporterNodeClient {
    config: SnapshotExporterNodeConfig,
    session: Option<ControlClientSession>,
    limits: SchemaLimits,
}

impl SnapshotExporterNodeClient {
    pub fn connect(config: &SnapshotExporterNodeConfig) -> Result<Self, SnapshotClientError> {
        let session = Self::connect_session(config)?;
        Ok(Self {
            config: config.clone(),
            session: Some(session),
            limits: config.limits,
        })
    }

    fn connect_session(
        config: &SnapshotExporterNodeConfig,
    ) -> Result<ControlClientSession, SnapshotClientError> {
        let stream =
            UnixStream::connect(&config.node_socket).map_err(|source| SnapshotClientError::Io {
                path: config.node_socket.clone(),
                source,
            })?;
        let mut session = ControlClientSession::connect(
            stream,
            ClientPolicy::exporter_to_node(config.identity, config.limits),
        )?;
        session.handshake()?;
        Ok(session)
    }

    fn ensure_session(&mut self) -> Result<(), SnapshotClientError> {
        if self.session.is_none() {
            self.session = Some(Self::connect_session(&self.config)?);
        }
        Ok(())
    }

    pub fn list(
        &mut self,
        after_lease_generation: u64,
    ) -> Result<ListSnapshotHandoffsResponseV1, SnapshotClientError> {
        let request = ListSnapshotHandoffsV1 {
            after_lease_generation,
            limit: MAX_SNAPSHOT_HANDOFFS_PER_RESPONSE,
        };
        let body = self.call(
            NodeMessageKind::ListSnapshotHandoffs,
            request.encode_body(&self.limits)?,
        )?;
        ListSnapshotHandoffsResponseV1::decode_body(&body, &self.limits).map_err(Into::into)
    }

    pub fn get(
        &mut self,
        job_id: B256,
        lease_generation: u64,
    ) -> Result<SnapshotHandoffV1, SnapshotClientError> {
        let request = GetSnapshotHandoffV1 {
            job_id,
            lease_generation,
        };
        let body = self.call(
            NodeMessageKind::GetSnapshotHandoff,
            request.encode_body(&self.limits)?,
        )?;
        SnapshotHandoffV1::decode_body(&body, &self.limits).map_err(Into::into)
    }

    pub fn acknowledge_open(
        &mut self,
        request: RenewSnapshotLeaseV1,
    ) -> Result<SnapshotLeaseOpenedV1, SnapshotClientError> {
        let body = self.call(
            NodeMessageKind::RenewSnapshotLease,
            request.encode_body(&self.limits)?,
        )?;
        SnapshotLeaseOpenedV1::decode_body(&body, &self.limits).map_err(Into::into)
    }

    pub fn finalized_intent_proof(
        &mut self,
        job_id: B256,
    ) -> Result<FinalizedIntentProofResponseV1, SnapshotClientError> {
        let request = BuildFinalizedIntentProofV1 { job_id };
        let body = self.call(
            NodeMessageKind::BuildFinalizedIntentProof,
            request.encode_body(&self.limits)?,
        )?;
        FinalizedIntentProofResponseV1::decode_body(&body, &self.limits).map_err(Into::into)
    }

    pub fn authenticate_handoff(
        &mut self,
        handoff: &SnapshotHandoffV1,
        expected: ExpectedFinalizedIntentBindingV1,
    ) -> Result<VerifiedFinalizedIntentV1, SnapshotClientError> {
        let response = self.finalized_intent_proof(handoff.job_id)?;
        authenticate_snapshot_handoff(handoff, &response, expected, &self.limits)
            .map_err(Into::into)
    }

    pub fn lysis_openings(
        &mut self,
        job_id: B256,
        subjects: OpeningSubjectsV1,
    ) -> Result<LysisOpeningsProofV1, SnapshotClientError> {
        let request = BuildLysisOpeningsV1 { job_id, subjects };
        let body = self.call(
            NodeMessageKind::BuildLysisOpenings,
            request.encode_body(&self.limits)?,
        )?;
        LysisOpeningsProofV1::decode_body(&body, &self.limits).map_err(Into::into)
    }

    pub fn require_projection_contains(
        &mut self,
        handoff: &SnapshotHandoffV1,
        block_number: u64,
        block_hash: B256,
    ) -> Result<ProjectionContainedV1, SnapshotClientError> {
        let request = CheckProjectionContainmentV1 {
            job_id: handoff.job_id,
            lease_generation: handoff.lease_generation,
            projection_checkpoint: ProjectionCheckpointV1 {
                block_number,
                block_hash,
            },
        };
        let body = self.call(
            NodeMessageKind::CheckProjectionContainment,
            request.encode_body(&self.limits)?,
        )?;
        ProjectionContainedV1::decode_body(&body, &self.limits).map_err(Into::into)
    }

    pub fn commit(
        &mut self,
        request: CommitSnapshotExportV1,
    ) -> Result<SnapshotExportCommittedV1, SnapshotClientError> {
        let body = self.call(
            NodeMessageKind::CommitSnapshotExport,
            request.encode_body(&self.limits)?,
        )?;
        SnapshotExportCommittedV1::decode_body(&body, &self.limits).map_err(Into::into)
    }

    fn call(
        &mut self,
        request_kind: NodeMessageKind,
        body: Vec<u8>,
    ) -> Result<Vec<u8>, SnapshotClientError> {
        self.ensure_session()?;
        let response = {
            let Some(session) = self.session.as_mut() else {
                return Err(SnapshotClientError::SessionUnavailable);
            };
            (|| {
                session.send_request(request_kind as u16, body)?;
                session.receive_response()
            })()
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                // A request may have reached the node even when its response
                // did not reach us. Never replay it inside this call. Drop the
                // poisoned request-id state so the next reconciliation opens
                // a fresh authenticated session and retries idempotently.
                self.session = None;
                return Err(error.into());
            }
        };
        if response.message_kind == NodeMessageKind::Error as u16 {
            let error = LocalErrorV1::decode_body(&response.body, &self.limits)?;
            return Err(SnapshotClientError::RemoteRejected {
                request_kind: request_kind as u16,
                error_code: error.error_code,
                retryable: error.retryable,
            });
        }
        if response.message_kind != NodeMessageKind::Response as u16 {
            return Err(SnapshotClientError::UnexpectedResponseKind(
                response.message_kind,
            ));
        }
        Ok(response.body)
    }
}

#[derive(Debug, Error)]
pub enum SnapshotClientError {
    #[error("connect node snapshot control socket {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Handoff(#[from] SnapshotHandoffVerificationError),
    #[error(
        "node rejected snapshot request {request_kind:#06x} with code {error_code}, retryable={retryable}"
    )]
    RemoteRejected {
        request_kind: u16,
        error_code: u16,
        retryable: bool,
    },
    #[error("node returned unexpected snapshot response kind {0:#06x}")]
    UnexpectedResponseKind(u16),
    #[error("node snapshot control session is unavailable")]
    SessionUnavailable,
}
