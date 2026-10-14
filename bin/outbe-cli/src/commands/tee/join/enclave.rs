use crate::errors::TeeJoinEnclaveError;
use alloy_primitives::B256;

use eyre::Result;

use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

use outbe_tee::finalized_admission::FinalizedAdmissionRecordKindV1;

use outbe_tee::protocol::EnclaveRequest;
use outbe_tee::protocol::EnclaveResponse;
use outbe_tee::AuthorizedEnclaveClient;

use outbe_tee::EnclaveClient;

use outbe_tee::GeneratedDcapQuoteV1;

use outbe_tee::ReplacementCandidateEnclaveV1;

use outbe_tee::TransportError;

pub(in super::super) enum JoinEnclave {
    Committed(Box<AuthorizedEnclaveClient>),
    Candidate(Box<ReplacementCandidateEnclaveV1>),
    Development(Box<EnclaveClient>),
}

impl JoinEnclave {
    pub(in super::super) fn is_candidate(&self) -> bool {
        matches!(self, Self::Candidate(_))
    }

    pub(in super::super) fn generate_dcap_quote(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1> {
        match self {
            Self::Committed(client) => client.generate_dcap_quote(intent),
            Self::Candidate(client) => client.generate_dcap_quote(intent),
            Self::Development(_) => unreachable!("DCAP join cannot use development transport"),
        }
        .map_err(|error| TeeJoinEnclaveError::GenerateDcapQuote(error).into())
    }

    pub(in super::super) fn sign_registration_intent_dev_v1(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64]> {
        match self {
            Self::Committed(client) => client.sign_registration_intent_dev_v1(intent),
            Self::Candidate(client) => client.sign_registration_intent_dev_v1(intent),
            Self::Development(client) => client.sign_registration_intent_dev_v1(intent),
        }
        .map_err(|error| TeeJoinEnclaveError::SignDevIntent(error).into())
    }

    pub(in super::super) fn request(
        &mut self,
        request: &EnclaveRequest,
    ) -> Result<EnclaveResponse> {
        self.request_transport(request)
            .map_err(|error| TeeJoinEnclaveError::Transport(error).into())
    }

    pub(in super::super) fn request_transport(
        &mut self,
        request: &EnclaveRequest,
    ) -> std::result::Result<EnclaveResponse, TransportError> {
        match self {
            Self::Committed(client) => client.request(request),
            Self::Candidate(client) => client.request(request),
            Self::Development(client) => client.request(request),
        }
    }

    pub(in super::super) fn begin_finalized_admission_v1(
        &mut self,
        input: outbe_tee::FinalizedAdmissionBeginInputV1<'_>,
    ) -> std::result::Result<B256, TransportError> {
        match self {
            Self::Committed(client) => client.begin_finalized_admission_v1(input),
            Self::Candidate(client) => client.begin_finalized_admission_v1(input),
            Self::Development(_) => {
                unreachable!("finalized admission cannot use development transport")
            }
        }
    }

    pub(in super::super) fn upload_finalized_admission_record_v1(
        &mut self,
        request_hash: B256,
        kind: FinalizedAdmissionRecordKindV1,
        record: &[u8],
    ) -> std::result::Result<(), TransportError> {
        match self {
            Self::Committed(client) => {
                client.upload_finalized_admission_record_v1(request_hash, kind, record)
            }
            Self::Candidate(client) => {
                client.upload_finalized_admission_record_v1(request_hash, kind, record)
            }
            Self::Development(_) => {
                unreachable!("finalized admission cannot use development transport")
            }
        }
    }

    pub(in super::super) fn finish_finalized_admission_v1(
        &mut self,
        request_hash: B256,
        expected_tribute_offer_public: [u8; 32],
    ) -> std::result::Result<[u8; 32], TransportError> {
        match self {
            Self::Committed(client) => {
                client.finish_finalized_admission_v1(request_hash, expected_tribute_offer_public)
            }
            Self::Candidate(client) => {
                client.finish_finalized_admission_v1(request_hash, expected_tribute_offer_public)
            }
            Self::Development(_) => {
                unreachable!("finalized admission cannot use development transport")
            }
        }
    }
}

pub(super) struct EnclaveIdentityKeys {
    pub(super) recipient_x25519: [u8; 32],
    pub(super) attestation_ed25519: [u8; 32],
    pub(super) noise_responder_x25519: [u8; 32],
    pub(super) enclave_id: B256,
    pub(super) node_host_authorization_hash: B256,
}

pub(super) struct ConnectedJoinEnclave {
    pub(super) enclave: JoinEnclave,
    pub(super) keys: EnclaveIdentityKeys,
}

pub(super) struct EnclaveConnectionContext<'a> {
    pub(super) join_transport: super::JoinTransport,
    pub(super) enclave_socket: &'a str,
    pub(super) node_data_dir: Option<&'a std::path::Path>,
    pub(super) policy: &'a outbe_primitives::tee_attestation_v1::TeePolicyV1,
    pub(super) has_finalized_binding: bool,
    pub(super) reth_p2p_public: [u8; 33],
    pub(super) node_id: outbe_primitives::tee_attestation_v1::NodeIdV1,
    pub(super) node_signing_key: &'a k256::ecdsa::SigningKey,
}

pub(super) fn connect_join_enclave(
    ctx: EnclaveConnectionContext<'_>,
) -> Result<ConnectedJoinEnclave> {
    match ctx.join_transport {
        super::JoinTransport::AuthorizedNodeHost => connect_authorized_node_host(ctx),
        super::JoinTransport::Development => connect_development_join_enclave(ctx),
    }
}

fn connect_authorized_node_host(ctx: EnclaveConnectionContext<'_>) -> Result<ConnectedJoinEnclave> {
    use eyre::WrapErr as _;
    let node_data_dir = ctx
        .node_data_dir
        .expect("transport selection requires node data dir");
    std::fs::create_dir_all(node_data_dir)
        .wrap_err_with(|| format!("create NodeHost data directory {}", node_data_dir.display()))?;
    let manifest_path = node_data_dir
        .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
        .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1);
    let identity = outbe_tee::NodeHostIdentityV1 {
        network_binding: ctx.policy.network_binding(),
        reth_p2p_public: ctx.reth_p2p_public,
    };
    let (enclave, manifest) = if manifest_path.exists() {
        open_existing_or_candidate_node_host(&ctx, node_data_dir, identity)?
    } else {
        initialize_fresh_node_host(&ctx, node_data_dir, identity)?
    };
    if manifest.network_binding() != ctx.policy.network_binding() || manifest.node_id != ctx.node_id
    {
        return Err(TeeJoinEnclaveError::ManifestIdentityMismatch.into());
    }
    let enclave_id = manifest
        .enclave_id()
        .map_err(TeeJoinEnclaveError::InvalidCommittedEnclaveIdentity)?;
    let authorization = manifest
        .node_host_authorization_hash()
        .map_err(TeeJoinEnclaveError::InvalidCommittedAuthorization)?;
    Ok(ConnectedJoinEnclave {
        enclave,
        keys: EnclaveIdentityKeys {
            recipient_x25519: manifest.recipient_x25519,
            attestation_ed25519: manifest.attestation_ed25519,
            noise_responder_x25519: manifest.noise_responder_x25519,
            enclave_id,
            node_host_authorization_hash: authorization,
        },
    })
}

fn open_existing_or_candidate_node_host(
    ctx: &EnclaveConnectionContext<'_>,
    node_data_dir: &std::path::Path,
    identity: outbe_tee::NodeHostIdentityV1,
) -> Result<(
    JoinEnclave,
    outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
)> {
    let committed = outbe_tee::load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(TeeJoinEnclaveError::LoadCommittedManifest)?;
    match outbe_tee::connect_committed_node_host_enclave(ctx.enclave_socket, node_data_dir) {
        Ok(client) => Ok((JoinEnclave::Committed(Box::new(client)), committed)),
        Err(committed_error) if ctx.has_finalized_binding => {
            let candidate = outbe_tee::prepare_node_host_enclave_replacement_candidate(
                ctx.enclave_socket,
                node_data_dir,
                identity,
                |hash| super::super::sign_node_hash(ctx.node_signing_key, hash),
            )
            .map_err(|candidate_error| {
                TeeJoinEnclaveError::EndpointMatchesNeither {
                    committed: committed_error,
                    candidate: candidate_error,
                }
            })?;
            let manifest = candidate.manifest().clone();
            Ok((JoinEnclave::Candidate(Box::new(candidate)), manifest))
        }
        Err(error) => Err(TeeJoinEnclaveError::ReconnectUnregisteredCommitted(error).into()),
    }
}

fn initialize_fresh_node_host(
    ctx: &EnclaveConnectionContext<'_>,
    node_data_dir: &std::path::Path,
    identity: outbe_tee::NodeHostIdentityV1,
) -> Result<(
    JoinEnclave,
    outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
)> {
    let client = outbe_tee::connect_or_initialize_node_host_enclave(
        ctx.enclave_socket,
        node_data_dir,
        identity,
        |hash| super::super::sign_node_hash(ctx.node_signing_key, hash),
    )
    .map_err(TeeJoinEnclaveError::InitializeNodeHost)?;
    let manifest = outbe_tee::load_committed_enclave_manifest_v1(node_data_dir)
        .map_err(TeeJoinEnclaveError::LoadCommittedManifest)?;
    Ok((JoinEnclave::Committed(Box::new(client)), manifest))
}

fn connect_development_join_enclave(
    ctx: EnclaveConnectionContext<'_>,
) -> Result<ConnectedJoinEnclave> {
    let development = EnclaveClient::connect_endpoint(ctx.enclave_socket).map_err(|source| {
        TeeJoinEnclaveError::ConnectDevelopmentEnclave {
            socket: ctx.enclave_socket.to_owned(),
            source,
        }
    })?;
    if development.is_hardware_attested() {
        return Err(TeeJoinEnclaveError::GramineDirectDevHardwareAttested.into());
    }
    let (recipient, attestation, noise) = match development.quote() {
        EnclaveResponse::Quote {
            recipient_x25519_pub,
            attestation_pub,
            noise_static_pub,
            ..
        } => (*recipient_x25519_pub, *attestation_pub, *noise_static_pub),
        other => {
            return Err(
                TeeJoinEnclaveError::UnexpectedDevelopmentQuote(format!("{other:?}")).into(),
            )
        }
    };
    let (enclave_id, authorization) = super::super::development_identity_v1(
        ctx.policy,
        &ctx.node_id,
        recipient,
        attestation,
        noise,
    )?;
    Ok(ConnectedJoinEnclave {
        enclave: JoinEnclave::Development(Box::new(development)),
        keys: EnclaveIdentityKeys {
            recipient_x25519: recipient,
            attestation_ed25519: attestation,
            noise_responder_x25519: noise,
            enclave_id,
            node_host_authorization_hash: authorization,
        },
    })
}
