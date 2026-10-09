//! Blocking node-side client for the enclave channel (Noise-IK over framed UDS).
//!
//! Production first discovers an initialization challenge, submits one canonical
//! node-signed manifest, and proves possession of its persistent `NodeHost` Noise
//! initiator key. Later connections use `OpenSession` plus the same key. The
//! legacy `EnclaveClient` GetQuote flow remains for the separate dev/mock
//! transport. The production enclave does not accept this flow.
//!
//! The client is fully synchronous. The `offerTribute` precompile drives it
//! with a blocking UDS round-trip. The enclave batch request is
//! `ProcessTributeOfferBatch`. It uses no async,
//! no `spawn`, and nothing that would capture a `StorageHandle`.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::Path;

use alloy_primitives::{keccak256, B256};
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, AttestationOperationV1, EnclaveInitializationManifestV1, RegistrationIntentV1,
    TransitionKeyReadyProofV1,
};
use rand::RngCore as _;
use zeroize::Zeroizing;

use crate::codec::{decode_response, encode_request, read_frame, write_frame};
use crate::dcap_protocol::{
    dcap_onboarding_attestation_preimage, dcap_onboarding_request_hash,
    dcap_verification_attestation_preimage, dcap_verification_request_hash,
    DcapOnboardingArtifactV1, DcapOnboardingContextV1, DcapOnboardingVerificationResultV1,
    DcapVerificationOutcomeV1, RegistrationVerificationRequest, MAX_DCAP_VERIFICATION_CHUNK_BYTES,
};
use crate::errors::TransportError;
use crate::finalized_admission::{
    onboarding_artifact_ingest_request_hash_v1, MAX_ONBOARDING_INGEST_CHUNK_BYTES,
};
use crate::finalized_admission::{FinalizedAdmissionBeginInputV1, FinalizedAdmissionIngestInputV1};
use crate::protocol::{EnclaveRequest, EnclaveResponse};
use crate::remote_session::RemoteSessionAdmissionV1;
use crate::NOISE_PARAMS;

mod transport;
mod verification;

#[cfg(test)]
use transport::timeout_seconds_from;
use transport::{connect_endpoint_transport, enclave_io_timeout, with_io_phase, Transport};

use verification::{
    validate_dcap_onboarding_response, validate_dcap_verification_response,
    validate_generated_dcap_quote, verify_quote,
};
pub use verification::{
    verify_fidelity_cohort_attestation, verify_fidelity_query_attestation,
    verify_fidelity_snapshot_attestation, verify_gratis_op_attestation, verify_peer_quote,
    verify_promis_op_attestation, verify_tribute_offer_attestation, AttestedPeerKeys,
};

/// The enclave identity fields captured from the quote response at connect time.
#[derive(Debug, Clone)]
struct QuoteIdentity {
    mrenclave: B256,
    mrsigner: B256,
    isv_svn: u16,
    attestation_pub: [u8; 32],
    /// The enclave's Noise static key as pinned by [`verify_quote`]. This is the
    /// key the Noise-IK handshake actually authenticated. Retained so a session
    /// reconnect can require the byte-identical peer.
    noise_static_pub: [u8; 32],
    /// The attestation environment the enclave self-reported (e.g.
    /// `none (gramine-direct / no SGX)`).
    attestation: String,
}

/// Blocking client for one enclave session.
pub struct EnclaveClient {
    stream: Transport,
    noise: snow::TransportState,
    identity: QuoteIdentity,
    /// The raw `EnclaveResponse::Quote` this session verified at connect. Retained
    /// for development/bootstrap flows that must relay the exact quote bytes after
    /// local verification. It grants no key-delivery capability by itself.
    raw_quote: EnclaveResponse,
}

/// Persistent Noise IK initiator identity authorized by one enclave manifest.
/// The private bytes are zeroized on drop. This API never exposes them.
#[derive(Clone)]
pub struct NodeHostNoiseKey {
    private: Zeroizing<[u8; 32]>,
    public: [u8; 32],
}

impl core::fmt::Debug for NodeHostNoiseKey {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("NodeHostNoiseKey")
            .field("public", &self.public)
            .finish_non_exhaustive()
    }
}

impl NodeHostNoiseKey {
    /// Construct from bytes loaded by the node's persistent secret store.
    pub fn from_private(private: [u8; 32]) -> Self {
        Self::from_zeroizing(Zeroizing::new(private))
    }

    fn from_zeroizing(private: Zeroizing<[u8; 32]>) -> Self {
        let static_secret = x25519_dalek::StaticSecret::from(*private);
        let public = x25519_dalek::PublicKey::from(&static_secret).to_bytes();
        Self { private, public }
    }

    /// Create the NodeHost identity exactly once with owner-only permissions.
    /// Existing files reject. Callers must never rotate this key implicitly.
    pub fn create_new(path: &Path) -> Result<Self, TransportError> {
        let mut private = Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(&mut *private);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(private.as_ref())?;
        file.sync_all()?;
        if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
            File::open(parent)?.sync_all()?;
        }
        Ok(Self::from_zeroizing(private))
    }

    /// Load the one existing NodeHost identity. Missing, truncated, symlinked or
    /// over-permissive/foreign-owned files fail closed. Loss never triggers key
    /// regeneration. `O_NOFOLLOW` and metadata from the opened descriptor avoid
    /// a path-check/read race.
    pub fn load(path: &Path) -> Result<Self, TransportError> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(TransportError::Codec(
                "NodeHost key path must be a regular non-symlink file".to_string(),
            ));
        }
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(TransportError::Codec(
                "NodeHost key file mode must be exactly 0600".to_string(),
            ));
        }
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(TransportError::Codec(
                "NodeHost key file must be owned by the current effective user".to_string(),
            ));
        }
        let mut private = Zeroizing::new([0u8; 32]);
        file.read_exact(&mut *private)?;
        let mut trailing = [0u8; 1];
        if file.read(&mut trailing)? != 0 {
            return Err(TransportError::Codec(
                "NodeHost key must be exactly 32 bytes".to_string(),
            ));
        }
        Ok(Self::from_zeroizing(private))
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnclaveInitializationChallenge {
    pub challenge: [u8; 32],
    pub recipient_x25519: [u8; 32],
    pub attestation_ed25519: [u8; 32],
    pub noise_responder_x25519: [u8; 32],
}

/// Quote material returned only after the host has verified the enclave's
/// exact intent echo and persistent Ed25519 proof of possession.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedDcapQuoteV1 {
    pub quote_body: Vec<u8>,
    pub enclave_signature: [u8; 64],
    pub transition_key_ready_proof: Option<TransitionKeyReadyProofV1>,
}

/// Production session authenticated by the sealed NodeHost initiator key.
pub struct AuthorizedEnclaveClient {
    call_context: crate::call_context::StreamContext,
    stream: Transport,
    noise: snow::TransportState,
    attestation_pub: [u8; 32],
}

/// One unpredictable, one-use authorization installed by the target's local
/// NodeHost after finalized Registry verification. It contains no secret key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteSessionTicketV1 {
    ticket_id: B256,
    initiator_static_x25519: [u8; 32],
    responder_static_x25519: [u8; 32],
    deadline: u64,
    finalized_block_hash: B256,
}

impl RemoteSessionTicketV1 {
    #[must_use]
    pub const fn ticket_id(&self) -> B256 {
        self.ticket_id
    }

    #[must_use]
    pub const fn deadline(&self) -> u64 {
        self.deadline
    }

    #[must_use]
    pub const fn finalized_block_hash(&self) -> B256 {
        self.finalized_block_hash
    }
}

/// Remote peer session. Its intentionally small interface exposes only the
/// non-secret public-key request. Owner and secret-bearing commands are not
/// part of this client surface, and the enclave matrix still denies them.
pub struct RemoteEnclaveClient {
    stream: Transport,
    noise: snow::TransportState,
}

/// Public, non-secret target enclave keys available to a remote peer. Keeping
/// this result typed prevents the narrow remote client from exposing the full
/// owner protocol response enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEnclavePublicKeysV1 {
    pub offer_key_ready: bool,
    pub recipient_x25519_pub: [u8; 32],
    pub attestation_pub: [u8; 32],
    pub noise_static_pub: [u8; 32],
    pub tee_bls_pub: Vec<u8>,
    pub dkg_enc_pub: [u8; 32],
    pub dkg_enc_sig: Vec<u8>,
}

impl EnclaveClient {
    /// Connect to the enclave over a Unix domain socket (native sidecar), then
    /// validate its key bindings, pin the enclave Noise static key, and complete
    /// the Noise-IK handshake.
    pub fn connect(path: &Path) -> Result<Self, TransportError> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(enclave_io_timeout()))?;
        stream.set_write_timeout(Some(enclave_io_timeout()))?;
        Self::from_transport(Transport::Unix(stream))
    }

    /// Connect to the enclave over TCP (`host:port`). Use this when the enclave
    /// runs under Gramine, whose pathname UDS a host process cannot reach.
    pub fn connect_tcp(addr: &str) -> Result<Self, TransportError> {
        let stream = TcpStream::connect(addr)?;
        let _ = stream.set_nodelay(true);
        stream.set_read_timeout(Some(enclave_io_timeout()))?;
        stream.set_write_timeout(Some(enclave_io_timeout()))?;
        Self::from_transport(Transport::Tcp(stream))
    }

    /// True when the enclave reports a DCAP/EPID attestation type. This structural
    /// connect path does not verify the quote signature or establish production
    /// admission. Those guarantees come from native QVL and TeeRegistry. Under
    /// gramine-sgx with `sgx.remote_attestation = "none"`, the enclave reports
    /// REAL measurements (read from the local SGX report) but produces no quote.
    /// It is therefore confidential and measured, yet unattested. False under
    /// gramine-direct / bare too. Gate quote-dependent trust on this, not on
    /// non-zero measurements.
    pub fn is_hardware_attested(&self) -> bool {
        let a = &self.identity.attestation;
        a.starts_with("dcap") || a.starts_with("epid")
    }

    /// The connected enclave's measurements `(mrenclave, mrsigner, isv_svn)`.
    pub fn measurements(&self) -> (B256, B256, u16) {
        (
            self.identity.mrenclave,
            self.identity.mrsigner,
            self.identity.isv_svn,
        )
    }

    /// The exact attestation environment the enclave self-reported (e.g.
    /// `dcap (gramine-sgx)` or `none (gramine-direct / no SGX)`).
    pub fn attestation_label(&self) -> &str {
        &self.identity.attestation
    }

    /// The enclave's Ed25519 attestation public key, pinned from this session's
    /// structurally validated quote response. Used to verify per-offer
    /// attestation tags (`verify_tribute_offer_attestation`). That is a local
    /// verify-then-discard check that binds a batch's results to the peer holding
    /// this session key. Production enclave identity is established separately.
    pub fn attestation_pub(&self) -> [u8; 32] {
        self.identity.attestation_pub
    }

    /// The enclave Noise static key this session's handshake authenticated,
    /// pinned from the structurally validated quote at connect time.
    pub fn noise_static_pub(&self) -> [u8; 32] {
        self.identity.noise_static_pub
    }

    /// Connect using an endpoint string: `host:port` -> TCP, otherwise a UDS path.
    pub fn connect_endpoint(endpoint: &str) -> Result<Self, TransportError> {
        if endpoint.contains(':') {
            Self::connect_tcp(endpoint)
        } else {
            Self::connect(Path::new(endpoint))
        }
    }

    /// Run GetQuote, structural binding validation, and the Noise-IK handshake
    /// over an established transport.
    fn from_transport(mut stream: Transport) -> Result<Self, TransportError> {
        // 1. GetQuote (cleartext, pre-handshake) with a fresh nonce.
        let nonce: [u8; 32] = rand::random();
        with_io_phase(
            write_frame(
                &mut stream,
                &encode_request(&EnclaveRequest::GetQuote { nonce })?,
            ),
            "quote request write",
        )?;
        let quote = decode_response(&with_io_phase(
            read_frame(&mut stream),
            "quote response read",
        )?)?;
        let enclave_static = verify_quote(&quote)?;
        let identity = quote_identity(&quote, enclave_static)?;

        // 2. Noise-IK handshake (initiator). The host static key is ephemeral
        //    per connection. The enclave static key is the bound, pinned one.
        let params = NOISE_PARAMS
            .parse()
            .map_err(|e| TransportError::Noise(format!("{e:?}")))?;
        let builder = snow::Builder::new(params);
        let host_keys = builder
            .generate_keypair()
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        let mut handshake = builder
            .local_private_key(&host_keys.private)
            .remote_public_key(&enclave_static)
            .build_initiator()
            .map_err(|e| TransportError::Handshake(e.to_string()))?;

        let mut buf = [0u8; 1024];
        let n = handshake
            .write_message(&[], &mut buf)
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        with_io_phase(
            write_frame(&mut stream, &buf[..n]),
            "Noise handshake request write",
        )?;

        let msg2 = with_io_phase(read_frame(&mut stream), "Noise handshake response read")?;
        handshake
            .read_message(&msg2, &mut buf)
            .map_err(|e| TransportError::Handshake(e.to_string()))?;

        let noise = handshake
            .into_transport_mode()
            .map_err(|e| TransportError::Handshake(e.to_string()))?;
        Ok(Self {
            stream,
            noise,
            identity,
            raw_quote: quote,
        })
    }

    /// This session's quote response, structurally validated at connect.
    pub fn quote(&self) -> &EnclaveResponse {
        &self.raw_quote
    }

    /// Send an operation with an explicit block context. Retries preserve it.
    pub fn request_with_context(
        &mut self,
        ctx: crate::call_context::EnclaveCallContextV1,
        request: &EnclaveRequest,
    ) -> Result<EnclaveResponse, TransportError> {
        let _scope = crate::call_context::ContextScope::enter(ctx);
        self.request(request)
    }

    pub fn request(&mut self, req: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
        let plain = crate::codec::encode_call(crate::call_context::resolve()?, req)?;
        let mut ct = vec![0u8; plain.len() + 64];
        let n = self
            .noise
            .write_message(&plain, &mut ct)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        with_io_phase(
            write_frame(&mut self.stream, &ct[..n]),
            "encrypted enclave request write",
        )?;

        let resp_ct = with_io_phase(
            read_frame(&mut self.stream),
            "encrypted enclave response read",
        )?;
        let mut pt = vec![0u8; resp_ct.len()];
        let n = self
            .noise
            .read_message(&resp_ct, &mut pt)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        let resp = decode_response(&pt[..n])?;
        if let EnclaveResponse::Error { message } = &resp {
            return Err(TransportError::EnclaveError(message.clone()));
        }
        Ok(resp)
    }

    /// Ask the enclave to sign one exact canonical GramineDirectDev
    /// registration intent. This development transport never produces a quote
    /// or DCAP verdict.
    pub fn sign_registration_intent_dev_v1(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64], TransportError> {
        let attestation_pub = self.attestation_pub();
        request_gramine_direct_dev_signature(attestation_pub, intent, |request| {
            self.request(request)
        })
    }
}

impl AuthorizedEnclaveClient {
    /// Fetch the one-time production initialization challenge and persistent
    /// enclave public keys. The discovery connection closes after this response.
    pub fn discover_endpoint(
        endpoint: &str,
    ) -> Result<EnclaveInitializationChallenge, TransportError> {
        let mut stream = connect_endpoint_transport(endpoint)?;
        with_io_phase(
            write_frame(
                &mut stream,
                &encode_request(&EnclaveRequest::GetInitializationChallenge)?,
            ),
            "initialization challenge write",
        )?;
        let response = decode_response(&with_io_phase(
            read_frame(&mut stream),
            "initialization challenge read",
        )?)?;
        match response {
            EnclaveResponse::InitializationChallenge {
                challenge,
                recipient_x25519_pub,
                attestation_pub,
                noise_static_pub,
            } => Ok(EnclaveInitializationChallenge {
                challenge,
                recipient_x25519: recipient_x25519_pub,
                attestation_ed25519: attestation_pub,
                noise_responder_x25519: noise_static_pub,
            }),
            EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
            _ => Err(TransportError::UnexpectedResponse),
        }
    }

    /// Commit one canonical signed initialization manifest. The enclave accepts
    /// it only if Noise message 1 proves the exact NodeHost key embedded in it.
    pub fn initialize_endpoint(
        endpoint: &str,
        manifest: &EnclaveInitializationManifestV1,
        node_signature: &[u8; 65],
        node_host: &NodeHostNoiseKey,
    ) -> Result<Self, TransportError> {
        if manifest.node_host_noise_x25519 != node_host.public() {
            return Err(TransportError::Handshake(
                "initialization manifest does not bind the supplied NodeHost key".to_string(),
            ));
        }
        let manifest_bytes = manifest
            .encode_canonical()
            .map_err(|error| TransportError::Codec(error.to_string()))?;
        let preamble = EnclaveRequest::Initialize {
            manifest: manifest_bytes,
            node_signature: node_signature.to_vec(),
        };
        let mut client = Self::connect_with_preamble(
            endpoint,
            &preamble,
            manifest.noise_responder_x25519,
            node_host,
            manifest.attestation_ed25519,
        )?;
        let response = client.receive_response("initialization acknowledgement read")?;
        let expected_enclave_id = manifest
            .enclave_id()
            .map_err(|error| TransportError::Codec(error.to_string()))?;
        let expected_node_host_authorization_hash = manifest
            .node_host_authorization_hash()
            .map_err(|error| TransportError::Codec(error.to_string()))?;
        match response {
            EnclaveResponse::Initialized {
                enclave_id,
                node_host_authorization_hash,
                sealed_loaded: false,
            } if enclave_id == expected_enclave_id
                && node_host_authorization_hash == expected_node_host_authorization_hash =>
            {
                Ok(client)
            }
            EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
            _ => Err(TransportError::UnexpectedResponse),
        }
    }

    /// Reconnect to an initialized production enclave using the same manifest and
    /// persistent NodeHost key. No quote is generated for this local session.
    pub fn connect_endpoint(
        endpoint: &str,
        manifest: &EnclaveInitializationManifestV1,
        node_host: &NodeHostNoiseKey,
    ) -> Result<Self, TransportError> {
        if manifest.node_host_noise_x25519 != node_host.public() {
            return Err(TransportError::Handshake(
                "sealed manifest does not bind the supplied NodeHost key".to_string(),
            ));
        }
        manifest
            .encode_canonical()
            .map_err(|error| TransportError::Codec(error.to_string()))?;
        Self::connect_with_preamble(
            endpoint,
            &EnclaveRequest::OpenSession,
            manifest.noise_responder_x25519,
            node_host,
            manifest.attestation_ed25519,
        )
    }

    fn connect_with_preamble(
        endpoint: &str,
        preamble: &EnclaveRequest,
        enclave_static: [u8; 32],
        node_host: &NodeHostNoiseKey,
        attestation_pub: [u8; 32],
    ) -> Result<Self, TransportError> {
        let mut stream = connect_endpoint_transport(endpoint)?;
        with_io_phase(
            write_frame(&mut stream, &encode_request(preamble)?),
            "production preamble write",
        )?;
        let params = NOISE_PARAMS
            .parse()
            .map_err(|error| TransportError::Noise(format!("{error:?}")))?;
        let mut handshake = snow::Builder::new(params)
            .local_private_key(node_host.private.as_ref())
            .remote_public_key(&enclave_static)
            .build_initiator()
            .map_err(|error| TransportError::Handshake(error.to_string()))?;
        let mut buffer = [0u8; 1024];
        let length = handshake
            .write_message(&[], &mut buffer)
            .map_err(|error| TransportError::Handshake(error.to_string()))?;
        with_io_phase(
            write_frame(&mut stream, &buffer[..length]),
            "Noise handshake request write",
        )?;
        let message = with_io_phase(read_frame(&mut stream), "Noise handshake response read")?;
        handshake
            .read_message(&message, &mut buffer)
            .map_err(|error| TransportError::Handshake(error.to_string()))?;
        let noise = handshake
            .into_transport_mode()
            .map_err(|error| TransportError::Handshake(error.to_string()))?;
        Ok(Self {
            stream,
            noise,
            attestation_pub,
            call_context: Default::default(),
        })
    }

    fn receive_response(
        &mut self,
        operation: &'static str,
    ) -> Result<EnclaveResponse, TransportError> {
        let ciphertext = with_io_phase(read_frame(&mut self.stream), operation)?;
        let mut plaintext = vec![0u8; ciphertext.len()];
        let length = self
            .noise
            .read_message(&ciphertext, &mut plaintext)
            .map_err(|error| TransportError::Noise(error.to_string()))?;
        let response = decode_response(&plaintext[..length])?;
        if let EnclaveResponse::Error { message } = &response {
            return Err(TransportError::EnclaveError(message.clone()));
        }
        Ok(response)
    }

    /// Send an operation with an explicit block context. Retries preserve it.
    pub fn request_with_context(
        &mut self,
        ctx: crate::call_context::EnclaveCallContextV1,
        request: &EnclaveRequest,
    ) -> Result<EnclaveResponse, TransportError> {
        let _scope = crate::call_context::ContextScope::enter(ctx);
        self.request(request)
    }

    /// Sends an ordinary authenticated owner request. Remote-ticket
    /// installation is reserved for the opaque finalized-admission route.
    pub fn request(&mut self, request: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
        if matches!(request, EnclaveRequest::AuthorizeRemoteSessionV1 { .. }) {
            return Err(TransportError::Handshake(
                "remote session authorization requires a finalized admission capability".into(),
            ));
        }
        if matches!(
            request,
            EnclaveRequest::BeginDcapOnboardingArtifactIngestV1 { .. }
                | EnclaveRequest::BeginUpgradeKeyTransferV1 { .. }
                | EnclaveRequest::DcapOnboardingArtifactChunkV1 { .. }
                | EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 { .. }
                | EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. }
        ) {
            return Err(TransportError::DcapVerification(
                "multi-frame onboarding ingest must use the whole-operation client API".into(),
            ));
        }
        self.request_internal(request)
    }

    /// Transfer a complete upgrade proof on one authenticated connection.
    /// Never retry a frame: callers must restart the whole operation on failure.
    pub fn transfer_upgrade_key_v1(
        &mut self,
        proof: &crate::upgrade_transfer::UpgradeKeyProofV1,
        artifact: &[u8],
        export: bool,
    ) -> Result<EnclaveResponse, TransportError> {
        let _call_context =
            crate::call_context::ContextScope::enter(crate::call_context::resolve()?);
        crate::upgrade_transfer::transfer(
            |request| self.request_internal(request),
            proof,
            artifact,
            export,
        )
    }

    /// Upload and install one exact finalized onboarding admission over this
    /// authenticated owner connection. There is no per-frame retry: after any
    /// fault the caller must reconnect and restart from `Begin`.
    pub fn ingest_finalized_admission_v1(
        &mut self,
        input: FinalizedAdmissionIngestInputV1<'_>,
    ) -> Result<[u8; 32], TransportError> {
        let _call_context =
            crate::call_context::ContextScope::enter(crate::call_context::resolve()?);
        let expected_tribute_offer_public = input.begin.expected_tribute_offer_public;
        let request_hash = self.begin_finalized_admission_v1(input.begin)?;
        for transition in input.committee_transitions {
            self.upload_finalized_admission_record_v1(
                request_hash,
                crate::finalized_admission::FinalizedAdmissionRecordKindV1::CommitteeTransition,
                transition,
            )?;
        }
        self.upload_finalized_admission_record_v1(
            request_hash,
            crate::finalized_admission::FinalizedAdmissionRecordKindV1::Admission,
            input.finalized_admission_witness,
        )?;
        self.finish_finalized_admission_v1(request_hash, expected_tribute_offer_public)
    }

    pub fn begin_finalized_admission_v1(
        &mut self,
        input: FinalizedAdmissionBeginInputV1<'_>,
    ) -> Result<B256, TransportError> {
        let FinalizedAdmissionBeginInputV1 {
            artifact,
            anchor_outcome,
            expected_intent_hash,
            expected_tribute_offer_public,
            expected_key_epoch,
            expected_tribute_offer_epoch,
        } = input;
        let request_hash = onboarding_artifact_ingest_request_hash_v1(input)
            .map_err(|error| TransportError::DcapVerification(error.to_string()))?;
        match self.request_internal(&EnclaveRequest::BeginDcapOnboardingArtifactIngestV1 {
            request_hash,
            artifact: artifact.to_vec(),
            anchor_outcome: anchor_outcome.to_vec(),
            expected_intent_hash,
            expected_tribute_offer_public,
            expected_key_epoch,
            expected_tribute_offer_epoch,
        })? {
            EnclaveResponse::DcapOnboardingArtifactIngestStartedV1 {
                request_hash: echoed,
            } if echoed == request_hash => Ok(request_hash),
            _ => Err(TransportError::DcapVerification(
                "enclave did not acknowledge the exact onboarding ingest".into(),
            )),
        }
    }

    pub fn finish_finalized_admission_v1(
        &mut self,
        request_hash: B256,
        expected_tribute_offer_public: [u8; 32],
    ) -> Result<[u8; 32], TransportError> {
        match self.request_internal(&EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 {
            request_hash,
        })? {
            EnclaveResponse::FinalizedAdmissionIngestedV1 {
                request_hash: echoed,
                tribute_offer_public,
            } if echoed == request_hash
                && tribute_offer_public == expected_tribute_offer_public =>
            {
                Ok(tribute_offer_public)
            }
            _ => Err(TransportError::DcapVerification(
                "enclave finalized a different onboarding admission".into(),
            )),
        }
    }

    pub fn upload_finalized_admission_record_v1(
        &mut self,
        request_hash: B256,
        kind: crate::finalized_admission::FinalizedAdmissionRecordKindV1,
        record: &[u8],
    ) -> Result<(), TransportError> {
        if record.is_empty() {
            return Err(TransportError::DcapVerification(
                "finalized admission record is empty".into(),
            ));
        }
        let mut offset = 0usize;
        for chunk in record.chunks(MAX_ONBOARDING_INGEST_CHUNK_BYTES) {
            let wire_offset = u32::try_from(offset).map_err(|_| {
                TransportError::DcapVerification(
                    "finalized admission proof offset exceeds u32".into(),
                )
            })?;
            let next = offset.checked_add(chunk.len()).ok_or_else(|| {
                TransportError::DcapVerification("finalized admission proof offset overflow".into())
            })?;
            let expected_next = u32::try_from(next).map_err(|_| {
                TransportError::DcapVerification(
                    "finalized admission proof offset exceeds u32".into(),
                )
            })?;
            match self.request_internal(&EnclaveRequest::DcapOnboardingArtifactChunkV1 {
                request_hash,
                kind,
                offset: wire_offset,
                bytes: chunk.to_vec(),
            })? {
                EnclaveResponse::DcapOnboardingArtifactChunkAcceptedV1 {
                    request_hash: echoed,
                    next_offset,
                } if echoed == request_hash && next_offset == expected_next => {}
                _ => {
                    return Err(TransportError::DcapVerification(
                        "enclave acknowledged a different onboarding proof chunk".into(),
                    ))
                }
            }
            offset = next;
        }
        match self.request_internal(&EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 {
            request_hash,
            kind,
        })? {
            EnclaveResponse::DcapOnboardingArtifactRecordAcceptedV1 {
                request_hash: echoed,
                kind: echoed_kind,
            } if echoed == request_hash && echoed_kind == kind => Ok(()),
            _ => Err(TransportError::DcapVerification(
                "enclave did not accept the exact finalized admission record".into(),
            )),
        }
    }

    fn request_internal(
        &mut self,
        request: &EnclaveRequest,
    ) -> Result<EnclaveResponse, TransportError> {
        let plaintext =
            crate::codec::encode_call(self.call_context.for_request(request)?, request)?;
        let mut ciphertext = vec![0u8; plaintext.len() + 64];
        let length = self
            .noise
            .write_message(&plaintext, &mut ciphertext)
            .map_err(|error| TransportError::Noise(error.to_string()))?;
        with_io_phase(
            write_frame(&mut self.stream, &ciphertext[..length]),
            "encrypted enclave request write",
        )?;
        self.receive_response("encrypted enclave response read")
    }

    /// Installs one one-use remote admission after the caller has verified its
    /// source and target bindings from local or anchored finalized state.
    /// Production node code uses the finalized facade in `outbe-node`. This
    /// low-level transport method remains public only across the crate seam.
    #[doc(hidden)]
    pub fn authorize_remote_session(
        &mut self,
        admission: &RemoteSessionAdmissionV1,
    ) -> Result<RemoteSessionTicketV1, TransportError> {
        let now = unix_time_seconds()?;
        if admission.deadline() <= now {
            return Err(TransportError::Handshake(
                "remote session admission is already expired".into(),
            ));
        }
        let mut ticket_bytes = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut ticket_bytes);
        if ticket_bytes == [0; 32] {
            return Err(TransportError::Handshake(
                "remote session ticket RNG returned zero".into(),
            ));
        }
        let ticket = RemoteSessionTicketV1 {
            ticket_id: B256::from(ticket_bytes),
            initiator_static_x25519: admission.initiator_static_x25519(),
            responder_static_x25519: admission.responder_static_x25519(),
            deadline: admission.deadline(),
            finalized_block_hash: admission.finalized_view().block_hash,
        };
        let request = if admission.retirement_height() == 0 {
            EnclaveRequest::AuthorizeRemoteSessionV1 {
                ticket_id: ticket.ticket_id,
                initiator_static_x25519: ticket.initiator_static_x25519,
                responder_static_x25519: ticket.responder_static_x25519,
                deadline: ticket.deadline,
                finalized_block_hash: ticket.finalized_block_hash,
            }
        } else {
            EnclaveRequest::AuthorizeRemoteSessionV2 {
                ticket_id: ticket.ticket_id,
                initiator_static_x25519: ticket.initiator_static_x25519,
                responder_static_x25519: ticket.responder_static_x25519,
                deadline: ticket.deadline,
                finalized_block_hash: ticket.finalized_block_hash,
                retirement_height: admission.retirement_height(),
            }
        };
        let response = self.request_internal(&request)?;
        match response {
            EnclaveResponse::RemoteSessionAuthorizedV1 { ticket_id }
                if ticket_id == ticket.ticket_id =>
            {
                Ok(ticket)
            }
            _ => Err(TransportError::UnexpectedResponse),
        }
    }

    /// Generate one quote for the exact canonical intent and authenticate the
    /// enclave proof before returning anything to registration assembly.
    pub fn generate_dcap_quote(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1, TransportError> {
        if intent.attestation_ed25519 != self.attestation_pub {
            return Err(TransportError::Attestation(
                "registration intent does not bind the initialized enclave attestation key".into(),
            ));
        }
        let canonical_intent = intent
            .encode_canonical()
            .map_err(|error| TransportError::Codec(error.to_string()))?;
        let response = self.request(&EnclaveRequest::GenerateDcapQuote {
            intent: canonical_intent.clone(),
        })?;
        validate_generated_dcap_quote(intent, &canonical_intent, self.attestation_pub, response)
    }

    /// Sign GramineDirectDev evidence through the authenticated production
    /// NodeHost session. The enclave itself permits this only while running in
    /// real SGX with remote attestation disabled (`SgxNoAttest`).
    pub fn sign_registration_intent_dev_v1(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<[u8; 64], TransportError> {
        let attestation_pub = self.attestation_pub;
        request_gramine_direct_dev_signature(attestation_pub, intent, |request| {
            self.request(request)
        })
    }

    /// Verify exact canonical evidence against exact canonical policy and
    /// consensus time inside the initialized Gramine enclave. This method hides
    /// the bounded multi-frame protocol from callers.
    pub fn verify_dcap_evidence_v1(
        &mut self,
        evidence: &[u8],
        policy: &[u8],
        block_timestamp: u64,
    ) -> Result<DcapVerificationOutcomeV1, TransportError> {
        let request_hash = dcap_verification_request_hash(evidence, policy, block_timestamp)
            .map_err(|code| {
                TransportError::DcapVerification(format!(
                    "invalid verifier input dimensions: {:#06x}",
                    code.code()
                ))
            })?;
        let response = self.upload_dcap_verification_v1(
            evidence,
            policy,
            request_hash,
            EnclaveRequest::BeginDcapVerificationV1 {
                request_hash,
                evidence_len: u32::try_from(evidence.len()).map_err(|_| {
                    TransportError::DcapVerification("evidence length exceeds u32".into())
                })?,
                policy_len: u32::try_from(policy.len()).map_err(|_| {
                    TransportError::DcapVerification("policy length exceeds u32".into())
                })?,
                block_timestamp,
            },
        )?;
        validate_dcap_verification_response(self.attestation_pub, request_hash, response)
    }

    /// Verify one exact `RegisterEnclave` request and request the deterministic
    /// one-time offer-key artifact from a resident source enclave.
    pub fn verify_dcap_registration_and_seal_v1(
        &mut self,
        request: RegistrationVerificationRequest<'_>,
    ) -> Result<DcapOnboardingVerificationResultV1, TransportError> {
        let RegistrationVerificationRequest {
            evidence,
            policy,
            block_timestamp,
            node_signature,
            enclave_signature,
            expected_tribute_offer_public,
            key_epoch,
            tribute_offer_epoch,
        } = request;
        let request_hash = dcap_onboarding_request_hash(request).map_err(|code| {
            TransportError::DcapVerification(format!(
                "invalid onboarding verifier input dimensions: {:#06x}",
                code.code()
            ))
        })?;
        let response = self.upload_dcap_verification_v1(
            evidence,
            policy,
            request_hash,
            EnclaveRequest::BeginDcapOnboardingVerificationV1 {
                request_hash,
                evidence_len: u32::try_from(evidence.len()).map_err(|_| {
                    TransportError::DcapVerification("evidence length exceeds u32".into())
                })?,
                policy_len: u32::try_from(policy.len()).map_err(|_| {
                    TransportError::DcapVerification("policy length exceeds u32".into())
                })?,
                block_timestamp,
                node_signature: node_signature.to_vec(),
                enclave_signature: enclave_signature.to_vec(),
                expected_tribute_offer_public,
                key_epoch,
                tribute_offer_epoch,
            },
        )?;
        validate_dcap_onboarding_response(self.attestation_pub, request_hash, response)
    }

    pub fn prepare_gramine_direct_dev_onboarding_artifact_v1(
        &mut self,
        context: DcapOnboardingContextV1,
    ) -> Result<DcapOnboardingArtifactV1, TransportError> {
        let request_hash = context.context_hash();
        let response = self.request(
            &EnclaveRequest::PrepareGramineDirectDevOnboardingArtifactV1 {
                request_hash,
                context: context.encode_canonical(),
            },
        )?;
        let EnclaveResponse::GramineDirectDevOnboardingArtifactPreparedV1 {
            request_hash: actual_request_hash,
            onboarding_artifact,
        } = response
        else {
            return Err(TransportError::UnexpectedResponse);
        };
        if actual_request_hash != request_hash {
            return Err(TransportError::EnclaveError(
                "GramineDirectDev onboarding response context hash mismatch".into(),
            ));
        }
        let artifact =
            DcapOnboardingArtifactV1::decode_canonical(&onboarding_artifact).map_err(|code| {
                TransportError::EnclaveError(format!(
                    "GramineDirectDev onboarding artifact is non-canonical: {:#06x}",
                    code.code()
                ))
            })?;
        if artifact.context != context {
            return Err(TransportError::EnclaveError(
                "GramineDirectDev onboarding artifact context mismatch".into(),
            ));
        }
        Ok(artifact)
    }

    fn upload_dcap_verification_v1(
        &mut self,
        evidence: &[u8],
        policy: &[u8],
        request_hash: B256,
        begin: EnclaveRequest,
    ) -> Result<EnclaveResponse, TransportError> {
        let _call_context =
            crate::call_context::ContextScope::enter(crate::call_context::resolve()?);
        match self.request(&begin)? {
            EnclaveResponse::DcapVerificationStartedV1 {
                request_hash: echoed,
            } if echoed == request_hash => {}
            _ => {
                return Err(TransportError::DcapVerification(
                    "enclave did not acknowledge the exact verifier request".into(),
                ))
            }
        }

        let mut offset = 0usize;
        for chunk in evidence
            .chunks(MAX_DCAP_VERIFICATION_CHUNK_BYTES)
            .chain(policy.chunks(MAX_DCAP_VERIFICATION_CHUNK_BYTES))
        {
            if chunk.is_empty() {
                continue;
            }
            let wire_offset = u32::try_from(offset).map_err(|_| {
                TransportError::DcapVerification("verification offset exceeds u32".into())
            })?;
            let next = offset.checked_add(chunk.len()).ok_or_else(|| {
                TransportError::DcapVerification("verification offset overflow".into())
            })?;
            let expected_next = u32::try_from(next).map_err(|_| {
                TransportError::DcapVerification("verification offset exceeds u32".into())
            })?;
            match self.request(&EnclaveRequest::DcapVerificationChunkV1 {
                request_hash,
                offset: wire_offset,
                bytes: chunk.to_vec(),
            })? {
                EnclaveResponse::DcapVerificationChunkAcceptedV1 {
                    request_hash: echoed,
                    next_offset,
                } if echoed == request_hash && next_offset == expected_next => {}
                _ => {
                    return Err(TransportError::DcapVerification(
                        "enclave acknowledged a different verifier chunk".into(),
                    ))
                }
            }
            offset = next;
        }

        self.request(&EnclaveRequest::FinishDcapVerificationV1 { request_hash })
    }

    pub fn attestation_pub(&self) -> [u8; 32] {
        self.attestation_pub
    }
}

fn request_gramine_direct_dev_signature(
    attestation_pub: [u8; 32],
    intent: &RegistrationIntentV1,
    request: impl FnOnce(&EnclaveRequest) -> Result<EnclaveResponse, TransportError>,
) -> Result<[u8; 64], TransportError> {
    if intent.attestation_mode != AttestationMode::GramineDirectDev
        || intent.attestation_ed25519 != attestation_pub
    {
        return Err(TransportError::Attestation(
            "GramineDirectDev registration intent does not bind this enclave and mode".into(),
        ));
    }
    let canonical = intent
        .encode_canonical()
        .map_err(|error| TransportError::Codec(error.to_string()))?;
    match request(&EnclaveRequest::SignRegistrationIntentDevV1 {
        intent: canonical.clone(),
    })? {
        EnclaveResponse::RegistrationIntentSignedDevV1 {
            intent: echoed,
            enclave_signature,
        } if echoed == canonical => {
            let signature: [u8; 64] = enclave_signature.try_into().map_err(|_| {
                TransportError::Attestation(
                    "enclave returned a non-canonical GramineDirectDev Ed25519 signature".into(),
                )
            })?;
            if !intent.verify_enclave_signature(&signature) {
                return Err(TransportError::Attestation(
                    "enclave returned an invalid GramineDirectDev intent signature".into(),
                ));
            }
            Ok(signature)
        }
        _ => Err(TransportError::UnexpectedResponse),
    }
}

impl RemoteEnclaveClient {
    pub fn connect_endpoint(
        endpoint: &str,
        ticket: &RemoteSessionTicketV1,
        node_host: &NodeHostNoiseKey,
    ) -> Result<Self, TransportError> {
        if ticket.initiator_static_x25519 != node_host.public() {
            return Err(TransportError::Handshake(
                "remote ticket does not bind the supplied source NodeHost key".into(),
            ));
        }
        if ticket.deadline <= unix_time_seconds()? {
            return Err(TransportError::Handshake(
                "remote session ticket is expired".into(),
            ));
        }
        let client = AuthorizedEnclaveClient::connect_with_preamble(
            endpoint,
            &EnclaveRequest::OpenRemoteSessionV1 {
                ticket_id: ticket.ticket_id,
            },
            ticket.responder_static_x25519,
            node_host,
            [0; 32],
        )?;
        Ok(Self {
            stream: client.stream,
            noise: client.noise,
        })
    }

    pub fn public_keys(&mut self) -> Result<RemoteEnclavePublicKeysV1, TransportError> {
        match self.request(&EnclaveRequest::GetPublicKeys)? {
            EnclaveResponse::PublicKeys {
                offer_key_ready,
                recipient_x25519_pub,
                attestation_pub,
                noise_static_pub,
                tee_bls_pub,
                dkg_enc_pub,
                dkg_enc_sig,
            } => Ok(RemoteEnclavePublicKeysV1 {
                offer_key_ready,
                recipient_x25519_pub,
                attestation_pub,
                noise_static_pub,
                tee_bls_pub,
                dkg_enc_pub,
                dkg_enc_sig,
            }),
            _ => Err(TransportError::UnexpectedResponse),
        }
    }

    fn request(&mut self, request: &EnclaveRequest) -> Result<EnclaveResponse, TransportError> {
        let plaintext = crate::codec::encode_call(crate::call_context::resolve()?, request)?;
        let mut ciphertext = vec![0_u8; plaintext.len() + 64];
        let length = self
            .noise
            .write_message(&plaintext, &mut ciphertext)
            .map_err(|error| TransportError::Noise(error.to_string()))?;
        with_io_phase(
            write_frame(&mut self.stream, &ciphertext[..length]),
            "remote encrypted enclave request write",
        )?;
        let ciphertext = with_io_phase(
            read_frame(&mut self.stream),
            "remote encrypted enclave response read",
        )?;
        let mut plaintext = vec![0_u8; ciphertext.len()];
        let length = self
            .noise
            .read_message(&ciphertext, &mut plaintext)
            .map_err(|error| TransportError::Noise(error.to_string()))?;
        let response = decode_response(&plaintext[..length])?;
        if let EnclaveResponse::Error { message } = response {
            return Err(TransportError::EnclaveError(message));
        }
        Ok(response)
    }
}

fn unix_time_seconds() -> Result<u64, TransportError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| TransportError::Handshake("system time precedes Unix epoch".into()))
}

/// Extract the enclave identity fields from the quote response.
fn quote_identity(
    quote: &EnclaveResponse,
    noise_static_pub: [u8; 32],
) -> Result<QuoteIdentity, TransportError> {
    let EnclaveResponse::Quote {
        mrenclave,
        mrsigner,
        isv_svn,
        attestation_pub,
        attestation,
        ..
    } = quote
    else {
        return Err(TransportError::UnexpectedResponse);
    };
    Ok(QuoteIdentity {
        mrenclave: *mrenclave,
        mrsigner: *mrsigner,
        isv_svn: *isv_svn,
        attestation_pub: *attestation_pub,
        noise_static_pub,
        attestation: attestation.clone(),
    })
}

#[cfg(test)]
mod tests;
