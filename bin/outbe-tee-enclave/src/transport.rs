//! Enclave-side transport: framed UDS server + Noise-IK responder + dispatch.
//!
//! Production accepts only initialization discovery, a signed write-once
//! initialization manifest, or `OpenSession` before Noise IK. The responder
//! authenticates the persistent NodeHost static key immediately after message 1
//! and before decoding any encrypted request. The legacy cleartext `GetQuote`
//! preamble exists only in the separate development/mock mode.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use zeroize::Zeroizing;

use outbe_primitives::tee_attestation_v1::{AttestationMode, RegistrationIntentV1};
use outbe_tee::codec::{decode_request, encode_response, read_frame, write_frame};
use outbe_tee::errors::TransportError;
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::NOISE_PARAMS;

use crate::dcap_verifier::{
    complete_verification_response, DcapVerificationProgressV1, DcapVerificationSessionV1,
};
use crate::dkg::{build_ceremony_info, DkgSessionStore};
use crate::initialization::{
    InitializationMode, InitializationState, PendingInitialization, PendingRemoteSessionV1,
    SessionAuthorityV1,
};
use crate::keys::EnclaveKeys;
use crate::process::process_tribute_offer_batch;
use crate::seal::{
    seal_tribute_offer_and_group_sig, unseal_tribute_offer_and_group_sig, EnclaveBootConfig,
    KeyPolicy, SealHeader, SEAL_FORMAT,
};

/// The tribute offer key derived once from the DKG group threshold signature
/// (Seam F): the secret stays resident, clients encrypt to `public`. Written on
/// the founding DKG connection's `DkgFinalizeTributeOffer`, then read by the
/// offer-decrypt path on other connections. Also carries the resident group
/// threshold signature so the restart fast-path restores the same permanent key.
pub struct DerivedTributeOfferKey {
    secret: Zeroizing<[u8; 32]>,
    public: [u8; 32],
    group_sig: Zeroizing<Vec<u8>>,
}

impl DerivedTributeOfferKey {
    /// The resident offer secret (never leaves the enclave).
    fn secret(&self) -> &[u8; 32] {
        &self.secret
    }
    /// The offer public key clients encrypt to (registered on-chain at bootstrap).
    pub fn public(&self) -> [u8; 32] {
        self.public
    }
    /// The resident group threshold signature (Seam F output). It never leaves
    /// the enclave unsealed and is persisted only inside sealed restart state.
    fn group_sig(&self) -> &[u8] {
        &self.group_sig
    }
    /// Build from explicit founding-finalization or registry-onboarding material,
    /// whose enclave-only path already holds the public component.
    fn from_parts(
        secret: Zeroizing<[u8; 32]>,
        public: [u8; 32],
        group_sig: Zeroizing<Vec<u8>>,
    ) -> Self {
        Self {
            secret,
            public,
            group_sig,
        }
    }
    /// Reconstruct from a resident offer secret + group signature (recomputes the
    /// public key). Used by the seal/unseal boot path to restore the DKG-derived
    /// offer key on restart without re-running the ceremony.
    fn from_secret_and_group_sig(
        secret: Zeroizing<[u8; 32]>,
        group_sig: Zeroizing<Vec<u8>>,
    ) -> Self {
        let public = crate::crypto::x25519_public(&secret);
        Self::from_parts(secret, public, group_sig)
    }
}

/// Process-wide, write-once slot for the DKG-derived offer key, shared across
/// every connection thread. `OnceLock` makes the first ceremony's key canonical;
/// a divergent founding finalization is rejected by the enclave request arm. No
/// `StorageHandle` exists in this binary, so std sync primitives apply here.
pub type SharedTributeOfferKey = Arc<OnceLock<DerivedTributeOfferKey>>;

/// Transport carriers supported by the production enclave server. Remote
/// sessions use the socket read timeout to close an otherwise idle connection
/// at its exclusive finalized-lease deadline.
pub trait EnclaveTransportStream: Read + Write {
    fn set_session_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
}

impl EnclaveTransportStream for UnixStream {
    fn set_session_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }
}

impl EnclaveTransportStream for TcpStream {
    fn set_session_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
}

/// The TSEAL sealing key + its policy, or `None` when no confidential key is
/// available. Real `EGETKEY(MRSIGNER)` under `gramine-sgx`; a fixed mock key
/// under `mock`/test (stable across rebuilds, simulating MRSIGNER); nothing under
/// `gramine-direct` prod, where there is no confidential at-rest persistence.
pub(crate) fn sealing_key() -> Option<([u8; 32], KeyPolicy)> {
    if let Ok(k) = crate::gramine::sealing_key_256(true) {
        return Some((k, KeyPolicy::MrSigner));
    }
    #[cfg(any(test, feature = "mock"))]
    {
        Some((crate::seal::MOCK_SEALING_KEY, KeyPolicy::Mock))
    }
    #[cfg(not(any(test, feature = "mock")))]
    {
        None
    }
}

/// Create one owner-only durable sealed blob without replacing existing state.
/// A crash can leave a partial final file, which is deliberately fail-closed:
/// subsequent startup rejects it and never converts corruption into rotation.
pub(crate) fn write_once_0600(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Restore the permanent offer key + group signature from the sealed blob at
/// boot. A missing blob is the only keyless result and is valid only for a fresh
/// identity whose startup context will prove founding/onboarding eligibility.
/// Any existing blob that cannot be read or unsealed is terminal: a permanent
/// key is never recovered, replaced, or regenerated for the same identity.
pub fn unseal_tribute_offer_and_group_sig_on_boot(
    cfg: &EnclaveBootConfig,
) -> Result<Option<DerivedTributeOfferKey>, String> {
    let path = cfg.sealed_root_path();
    let blob = match std::fs::read(&path) {
        Ok(blob) => blob,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "read sealed permanent offer key {}: {error}; no recovery or fallback exists",
                path.display()
            ));
        }
    };
    let (key, _policy) = sealing_key().ok_or_else(|| {
        format!(
            "sealed permanent offer key {} exists but no SGX sealing key is available; no recovery or fallback exists",
            path.display()
        )
    })?;
    match unseal_tribute_offer_and_group_sig(&blob, &key, cfg.chain_id, cfg.isv_svn) {
        Ok((secret, group_sig, _header)) => {
            eprintln!(
                "outbe-tee-enclave: unsealed offer key + group signature <- {} (restart fast-path)",
                path.display()
            );
            Ok(Some(DerivedTributeOfferKey::from_secret_and_group_sig(
                secret, group_sig,
            )))
        }
        Err(error) => Err(format!(
            "unseal permanent offer key {} failed: {error}; this identity has lost its key and no recovery or fallback exists",
            path.display()
        )),
    }
}

/// Persist the DKG-derived offer secret + the group threshold signature sealed
/// under the enclave's MRSIGNER key. Write-once and best-effort: no-op without a
/// boot config, before the offer key exists, without a sealing key (gramine-direct
/// prod => not confidential), or when the blob already exists. Never fatal —
/// sealing is local persistence, not consensus state.
fn seal_tribute_offer_and_group_sig_if_configured(
    boot: Option<&EnclaveBootConfig>,
    offer_key: &SharedTributeOfferKey,
) {
    let Some(cfg) = boot else { return };
    let Some(derived) = offer_key.get() else {
        return;
    };
    let path = cfg.sealed_root_path();
    if path.exists() {
        return;
    }
    let Some((key, policy)) = sealing_key() else {
        eprintln!(
            "outbe-tee-enclave: no sealing key (not gramine-sgx / not mock) — offer key \
             NOT persisted (no confidential at-rest storage on this platform)"
        );
        return;
    };
    let mut nonce = [0u8; 12];
    if ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce).is_err() {
        eprintln!("outbe-tee-enclave: seal nonce RNG failed — offer key NOT persisted");
        return;
    }
    let header = SealHeader {
        format_version: SEAL_FORMAT,
        key_policy: policy,
        isv_svn: cfg.isv_svn,
        key_epoch: 0,
        tribute_offer_epoch: 0,
        nonce,
    };
    match seal_tribute_offer_and_group_sig(
        derived.secret(),
        derived.group_sig(),
        &key,
        cfg.chain_id,
        &header,
    ) {
        Ok(blob) => match write_once_0600(&path, &blob) {
            Ok(()) => eprintln!(
                "outbe-tee-enclave: sealed offer key + group signature -> {}",
                path.display()
            ),
            Err(e) => eprintln!("outbe-tee-enclave: write {} failed: {e}", path.display()),
        },
        Err(e) => eprintln!("outbe-tee-enclave: seal_tribute_offer_and_group_sig failed: {e}"),
    }
}

/// Serve a single client connection end-to-end (no boot config / sealing).
/// Thin wrapper over [`serve_connection_with`]; kept for tests and callers that
/// do not seal.
pub fn serve_connection<S: EnclaveTransportStream>(
    stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
) -> Result<(), TransportError> {
    let initialization = InitializationState::development();
    serve_connection_with(stream, keys, offer_key, None, &initialization)
}

/// Serve a single client connection end-to-end. `offer_key` is the shared,
/// write-once DKG-derived offer key slot (populated by the DKG connection's
/// Seam F, read by the offer-decrypt path). `boot` carries the seal/unseal
/// configuration (chain_id / tee-dir / isv_svn); when `Some`, the sealing path
/// persists the offer secret + threshold share after Seam F. The production
/// accept loop passes the resident chain independently because non-sealing
/// enclaves still need a chain-scoped state-key domain.
pub fn serve_connection_with<S: EnclaveTransportStream>(
    stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
    initialization: &InitializationState,
) -> Result<(), TransportError> {
    let chain_id = boot
        .map(|config| config.chain_id)
        .unwrap_or(alloy_primitives::B256::ZERO);
    serve_connection_with_resident_chain(
        stream,
        keys,
        offer_key,
        boot,
        initialization,
        chain_id,
        crate::gramine::dcap_quote,
    )
}

/// Explicit hardware-free lifecycle harness. The synthetic quote only carries
/// the requested REPORT_DATA through the normal candidate transport path; it is
/// never a QVL-positive or hardware-evidence seam. The production binary is
/// built without `mock`, so this entrypoint and generator are absent from it.
#[cfg(feature = "mock")]
pub fn serve_connection_with_synthetic_dcap<S: EnclaveTransportStream>(
    stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
    initialization: &InitializationState,
) -> Result<(), TransportError> {
    let chain_id = boot
        .map(|config| config.chain_id)
        .unwrap_or(alloy_primitives::B256::ZERO);
    serve_connection_with_resident_chain(
        stream,
        keys,
        offer_key,
        boot,
        initialization,
        chain_id,
        synthetic_dcap_quote,
    )
}

fn serve_connection_with_resident_chain<S: EnclaveTransportStream>(
    mut stream: S,
    keys: &EnclaveKeys,
    offer_key: &SharedTributeOfferKey,
    boot: Option<&EnclaveBootConfig>,
    initialization: &InitializationState,
    chain_id: alloy_primitives::B256,
    quote_generator: fn(&[u8; 64]) -> Result<Vec<u8>, String>,
) -> Result<(), TransportError> {
    // 1. Minimal cleartext preamble. Production never emits a quote here.
    let first = decode_request(&read_frame(&mut stream)?)?;
    let mut remote_session: Option<PendingRemoteSessionV1> = None;
    let pending: Option<PendingInitialization> = match (initialization.mode(), first) {
        (InitializationMode::Development, EnclaveRequest::GetQuote { nonce }) => {
            write_frame(&mut stream, &encode_response(&keys.quote(nonce))?)?;
            None
        }
        (InitializationMode::Production, EnclaveRequest::GetInitializationChallenge) => {
            let response = initialization
                .challenge_response(keys)
                .map_err(TransportError::Handshake)?;
            write_frame(&mut stream, &encode_response(&response)?)?;
            return Ok(());
        }
        (
            InitializationMode::Production,
            EnclaveRequest::Initialize {
                manifest,
                node_signature,
            },
        ) => Some(
            initialization
                .prepare(&manifest, &node_signature, keys)
                .map_err(TransportError::Handshake)?,
        ),
        (InitializationMode::Production, EnclaveRequest::OpenSession) => {
            initialization
                .expected_node_host()
                .map_err(TransportError::Handshake)?;
            None
        }
        (InitializationMode::Production, EnclaveRequest::OpenRemoteSessionV1 { ticket_id }) => {
            remote_session = Some(
                initialization
                    .take_remote_session(ticket_id)
                    .map_err(TransportError::Handshake)?,
            );
            None
        }
        (InitializationMode::Development, _) => {
            return Err(TransportError::Handshake(
                "development transport expected legacy GetQuote before handshake".to_string(),
            ));
        }
        (InitializationMode::Production, _) => {
            return Err(TransportError::Handshake(
                "production transport expected initialization discovery, Initialize, OpenSession, or OpenRemoteSessionV1"
                    .to_string(),
            ));
        }
    };
    let session_authority = remote_session.map_or(SessionAuthorityV1::LocalNodeHost, |session| {
        SessionAuthorityV1::RemoteActiveNode {
            deadline: session.deadline(),
        }
    });
    set_remote_read_deadline(&stream, remote_session)?;

    // 2. Noise-IK responder handshake.
    let params = NOISE_PARAMS
        .parse()
        .map_err(|e| TransportError::Noise(format!("{e:?}")))?;
    let mut handshake = snow::Builder::new(params)
        .local_private_key(keys.noise_private())
        .build_responder()
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

    let mut buf = [0u8; 1024];
    let msg1 = read_frame(&mut stream)?;
    handshake
        .read_message(&msg1, &mut buf)
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

    // Noise IK authenticates the initiator static in message 1. Reject it here,
    // before message 2, transport mode, encrypted request decoding, or effects.
    if initialization.mode() == InitializationMode::Production {
        let expected = match remote_session {
            Some(session) => session.initiator_static_x25519(),
            None => pending
                .as_ref()
                .map(PendingInitialization::node_host_noise_x25519)
                .map(Ok)
                .unwrap_or_else(|| initialization.expected_node_host())
                .map_err(TransportError::Handshake)?,
        };
        let remote = handshake.get_remote_static().ok_or_else(|| {
            TransportError::Handshake("Noise IK message 1 omitted initiator static key".to_string())
        })?;
        if remote != expected {
            return Err(TransportError::Handshake(
                "Noise IK initiator is not the authorized NodeHost".to_string(),
            ));
        }
    }

    let initialized_this_connection = pending.is_some();
    if let Some(pending) = pending {
        initialization
            .commit(pending, keys)
            .map_err(TransportError::Handshake)?;
    }

    let n = handshake
        .write_message(&[], &mut buf)
        .map_err(|e| TransportError::Handshake(e.to_string()))?;
    write_frame(&mut stream, &buf[..n])?;

    let mut noise = handshake
        .into_transport_mode()
        .map_err(|e| TransportError::Handshake(e.to_string()))?;

    // Initialization success is disclosed only inside the newly authenticated
    // channel. OpenSession and the dev path wait for the first explicit command.
    if initialized_this_connection {
        let response = initialization
            .initialized_response()
            .map_err(TransportError::Handshake)?;
        let plain = encode_response(&response)?;
        let mut ct = vec![0u8; plain.len() + 64];
        let n = noise
            .write_message(&plain, &mut ct)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        write_frame(&mut stream, &ct[..n])?;
    }

    // Resident DKG ceremonies for this connection. A ceremony spans many
    // request/response round-trips on one connection (PoC: one connection per
    // enclave for the whole ceremony).
    let mut dkg = DkgSessionStore::new();
    let mut dcap_verification = DcapVerificationSessionV1::default();
    // Seal the DKG-derived offer key + share once installed (Seam F). Tracked
    // per-connection so we attempt the write-once seal at most once here.
    let mut seal_attempted = false;

    // 3. Encrypted request/response loop. Exits when the peer closes (read EOF).
    // Remote traffic is checked both before and after every blocking read, so a
    // frame arriving at or after the exclusive lease deadline is never decoded.
    loop {
        session_authority
            .ensure_live()
            .map_err(|message| TransportError::Handshake(message.to_string()))?;
        set_remote_read_deadline(&stream, remote_session)?;
        let frame = match read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(TransportError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                ) =>
            {
                break;
            }
            Err(TransportError::Io(error))
                if remote_session.is_some()
                    && matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
            {
                session_authority
                    .ensure_live()
                    .map_err(|message| TransportError::Handshake(message.to_string()))?;
                return Err(TransportError::Io(error));
            }
            Err(error) => return Err(error),
        };
        session_authority
            .ensure_live()
            .map_err(|message| TransportError::Handshake(message.to_string()))?;
        let mut pt = vec![0u8; frame.len()];
        let n = noise
            .read_message(&frame, &mut pt)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        let req = decode_request(&pt[..n])?;
        if let Err(message) =
            initialization.authorize_command(&req, offer_key.get().is_some(), session_authority)
        {
            let response = EnclaveResponse::Error {
                message: message.to_string(),
            };
            let plain = encode_response(&response)?;
            let mut ct = vec![0u8; plain.len() + 64];
            let n = noise
                .write_message(&plain, &mut ct)
                .map_err(|e| TransportError::Noise(e.to_string()))?;
            write_frame(&mut stream, &ct[..n])?;
            continue;
        }

        let resp = match req {
            request @ (EnclaveRequest::BeginDcapVerificationV1 { .. }
            | EnclaveRequest::DcapVerificationChunkV1 { .. }
            | EnclaveRequest::FinishDcapVerificationV1 { .. }) => {
                if initialization.mode() != InitializationMode::Production {
                    EnclaveResponse::Error {
                        message: "DCAP verification requires initialized production state"
                            .to_string(),
                    }
                } else {
                    match dcap_verification.handle(request) {
                        Ok(DcapVerificationProgressV1::Started { request_hash }) => {
                            EnclaveResponse::DcapVerificationStartedV1 { request_hash }
                        }
                        Ok(DcapVerificationProgressV1::ChunkAccepted {
                            request_hash,
                            next_offset,
                        }) => EnclaveResponse::DcapVerificationChunkAcceptedV1 {
                            request_hash,
                            next_offset,
                        },
                        Ok(DcapVerificationProgressV1::Complete(request)) => {
                            complete_verification_response(request, keys)
                        }
                        Err(message) => EnclaveResponse::Error {
                            message: message.to_string(),
                        },
                    }
                }
            }
            request => dispatch_with_initialization(
                request,
                keys,
                &mut dkg,
                offer_key,
                chain_id,
                Some(initialization),
                quote_generator,
            ),
        };

        // Persist the offer key + share the first time it becomes available
        // (write-once).
        if !seal_attempted && offer_key.get().is_some() {
            seal_tribute_offer_and_group_sig_if_configured(boot, offer_key);
            seal_attempted = true;
        }
        session_authority
            .ensure_live()
            .map_err(|message| TransportError::Handshake(message.to_string()))?;

        let plain = encode_response(&resp)?;
        let mut ct = vec![0u8; plain.len() + 64];
        let n = noise
            .write_message(&plain, &mut ct)
            .map_err(|e| TransportError::Noise(e.to_string()))?;
        write_frame(&mut stream, &ct[..n])?;
    }
    Ok(())
}

fn set_remote_read_deadline(
    stream: &impl EnclaveTransportStream,
    remote_session: Option<PendingRemoteSessionV1>,
) -> Result<(), TransportError> {
    let Some(session) = remote_session else {
        return Ok(());
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TransportError::Handshake("system time precedes Unix epoch".into()))?;
    let remaining = Duration::from_secs(session.deadline())
        .checked_sub(now)
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| TransportError::Handshake("remote session lease expired".into()))?;
    stream.set_session_read_timeout(Some(remaining))?;
    Ok(())
}

/// Dispatch a post-handshake request to a response. `offer_key` is the shared
/// DKG-derived offer key slot: once Seam F populates it, the offer-decrypt path
/// and `GetPublicKeys` use it instead of the pre-DKG dev offer key.
pub fn dispatch(
    req: EnclaveRequest,
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    offer_key: &SharedTributeOfferKey,
    chain_id: alloy_primitives::B256,
) -> EnclaveResponse {
    dispatch_with_initialization(
        req,
        keys,
        dkg,
        offer_key,
        chain_id,
        None,
        crate::gramine::dcap_quote,
    )
}

fn dispatch_with_initialization(
    req: EnclaveRequest,
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    offer_key: &SharedTributeOfferKey,
    chain_id: alloy_primitives::B256,
    initialization: Option<&InitializationState>,
    quote_generator: fn(&[u8; 64]) -> Result<Vec<u8>, String>,
) -> EnclaveResponse {
    match req {
        EnclaveRequest::GetQuote { .. }
        | EnclaveRequest::GetInitializationChallenge
        | EnclaveRequest::Initialize { .. }
        | EnclaveRequest::OpenSession
        | EnclaveRequest::OpenRemoteSessionV1 { .. }
        | EnclaveRequest::SessionHandshake { .. } => EnclaveResponse::Error {
            message: "pre-handshake request is not valid inside a Noise session".to_string(),
        },
        EnclaveRequest::BeginDcapVerificationV1 { .. }
        | EnclaveRequest::DcapVerificationChunkV1 { .. }
        | EnclaveRequest::FinishDcapVerificationV1 { .. } => EnclaveResponse::Error {
            message: "DCAP verification requires an authenticated production session".to_string(),
        },
        EnclaveRequest::AuthorizeRemoteSessionV1 {
            ticket_id,
            initiator_static_x25519,
            responder_static_x25519,
            deadline,
            finalized_block_hash,
        } => {
            let Some(initialization) = initialization else {
                return EnclaveResponse::Error {
                    message: "remote session authorization requires production initialization"
                        .into(),
                };
            };
            match initialization.authorize_remote_session(
                ticket_id,
                initiator_static_x25519,
                responder_static_x25519,
                deadline,
                finalized_block_hash,
                keys,
            ) {
                Ok(()) => EnclaveResponse::RemoteSessionAuthorizedV1 { ticket_id },
                Err(message) => EnclaveResponse::Error { message },
            }
        }
        EnclaveRequest::GetPublicKeys => EnclaveResponse::PublicKeys {
            offer_key_ready: offer_key.get().is_some(),
            // Advertise the DKG-derived offer key once available, so clients
            // encrypt to it. Before readiness the same field carries only the
            // one-time onboarding recipient and is never permanent chain state.
            recipient_x25519_pub: offer_key
                .get()
                .map(|k| k.public())
                .unwrap_or_else(|| keys.tribute_offer_public()),
            attestation_pub: keys.attestation_pub(),
            noise_static_pub: keys.noise_public(),
            tee_bls_pub: keys.tee_bls_public_bytes(),
            dkg_enc_pub: keys.dkg_enc_public(),
            dkg_enc_sig: keys.sign_dkg_enc_binding(chain_id),
        },
        EnclaveRequest::GenerateDcapQuote { intent } => {
            let Some(initialization) = initialization else {
                return EnclaveResponse::Error {
                    message: "DCAP quote generation requires initialized production state"
                        .to_string(),
                };
            };
            let result = (|| -> Result<(Vec<u8>, Vec<u8>), String> {
                let report_data = initialization.quote_report_data(&intent)?;
                let quote = quote_generator(&report_data)?;
                let quote_body = validate_generated_quote_binding(report_data, quote)?;
                let enclave_signature = keys.sign_attestation(&report_data[..32]);
                Ok((quote_body, enclave_signature.to_vec()))
            })();
            match result {
                Ok((quote_body, enclave_signature)) => EnclaveResponse::DcapQuote {
                    intent,
                    quote_body,
                    enclave_signature,
                },
                Err(message) => EnclaveResponse::Error { message },
            }
        }
        EnclaveRequest::SignRegistrationIntentDevV1 { intent } => {
            let Some(initialization) = initialization else {
                return EnclaveResponse::Error {
                    message: "development intent signing requires the development transport"
                        .to_string(),
                };
            };
            if initialization.mode() != InitializationMode::Development {
                return EnclaveResponse::Error {
                    message: "production enclave cannot sign GramineDirectDev evidence".to_string(),
                };
            }
            let result = (|| -> Result<Vec<u8>, String> {
                let decoded = RegistrationIntentV1::decode_canonical(&intent)
                    .map_err(|error| error.to_string())?;
                if decoded.attestation_mode != AttestationMode::GramineDirectDev
                    || decoded.attestation_ed25519 != keys.attestation_pub()
                    || decoded
                        .derived_enclave_id()
                        .map_err(|error| error.to_string())?
                        != decoded.enclave_id
                {
                    return Err(
                        "GramineDirectDev intent does not bind this enclave identity".into(),
                    );
                }
                let intent_hash = decoded.intent_hash().map_err(|error| error.to_string())?;
                Ok(keys.sign_attestation(intent_hash.as_slice()).to_vec())
            })();
            match result {
                Ok(enclave_signature) => EnclaveResponse::RegistrationIntentSignedDevV1 {
                    intent,
                    enclave_signature,
                },
                Err(message) => EnclaveResponse::Error { message },
            }
        }
        EnclaveRequest::ProcessTributeOfferBatch { offers } => {
            let derived = offer_key.get();
            let km = match derived {
                Some(d) => keys.tribute_offer_key_material_with(d.secret()),
                None => keys.tribute_offer_key_material(),
            };
            let (results, inputs_canonical_hash) = process_tribute_offer_batch(&km, &offers);
            // Sign (inputs_canonical_hash ‖ results) with the enclave's
            // Ed25519 attestation key. The host verifies this against the
            // attestation key it pinned from the quote, proving the results were
            // produced inside this attested enclave (not substituted by the host).
            let preimage = outbe_tee::protocol::tribute_offer_attestation_preimage(
                inputs_canonical_hash,
                &results,
            );
            let attestation_tag = keys.sign_attestation(&preimage).to_vec();
            EnclaveResponse::TributeOfferBatch {
                results,
                inputs_canonical_hash,
                attestation_tag,
            }
        }
        EnclaveRequest::ApplyGratisOp { request } => {
            // Derive the resident Gratis state key from the same DKG group
            // signature as the offer key — identical on every enclave, so the
            // re-encrypted state is byte-identical (consensus determinism).
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "ApplyGratisOp: no resident group key (DKG not complete)".to_string(),
                };
            };
            let state_key =
                match crate::gratis::derive_gratis_state_key(derived.group_sig(), chain_id, 0) {
                    Ok(k) => k,
                    Err(e) => {
                        return EnclaveResponse::Error {
                            message: e.to_string(),
                        }
                    }
                };
            let mut result = crate::gratis::apply_op(&state_key, &request);
            // Co-located Fidelity cohort section: applied atomically with the
            // Gratis op under its own independent key domain. A failing section
            // rejects the WHOLE op — the host writes neither ledger.
            if let (outbe_tee::protocol::GratisOpStatus::Applied, Some(section)) =
                (&result.status, &request.fidelity)
            {
                let fidelity_outcome =
                    crate::fidelity::derive_fidelity_state_key(derived.group_sig(), chain_id, 0)
                        .and_then(|fidelity_key| {
                            crate::fidelity::apply_cohort_section(
                                &fidelity_key,
                                request.account,
                                request.amount,
                                section,
                            )
                        });
                match fidelity_outcome {
                    Ok(outcome) => result.fidelity = Some(outcome),
                    Err(e) => {
                        result = crate::gratis::rejected_result(
                            format!("fidelity section failed: {e}"),
                            result.inputs_canonical_hash,
                        );
                    }
                }
            }
            // Sign (inputs_canonical_hash ‖ result) with the attestation key so the
            // host can prove the result came from this attested enclave.
            let preimage = outbe_tee::protocol::gratis_op_attestation_preimage(
                result.inputs_canonical_hash,
                &result,
            );
            result.attestation_tag = keys.sign_attestation(&preimage).to_vec();
            EnclaveResponse::GratisOpApplied {
                result: Box::new(result),
            }
        }
        EnclaveRequest::ApplyFidelityCohortOp { request } => {
            // Standalone cohort mutation over the independent Fidelity key
            // domain. Consensus path, re-executed by every validator.
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "ApplyFidelityCohortOp: no resident group key (DKG not complete)"
                        .to_string(),
                };
            };
            let result =
                crate::fidelity::derive_fidelity_state_key(derived.group_sig(), chain_id, 0)
                    .and_then(|key| crate::fidelity::apply_cohort_op(&key, &request));
            match result {
                Ok(mut result) => {
                    let preimage = outbe_tee::protocol::fidelity_cohort_attestation_preimage(
                        result.inputs_canonical_hash,
                        &result,
                    );
                    result.attestation_tag = keys.sign_attestation(&preimage).to_vec();
                    EnclaveResponse::FidelityCohortApplied {
                        result: Box::new(result),
                    }
                }
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::SnapshotFidelityLeagues { request } => {
            // Same resident-key derivation + attestation as ApplyGratisOp, over
            // the independent Fidelity key domain. Consensus path (metadosis
            // OCOMP prepare, re-executed by every validator).
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "SnapshotFidelityLeagues: no resident group key (DKG not complete)"
                        .to_string(),
                };
            };
            let leagues =
                crate::fidelity::derive_fidelity_state_key(derived.group_sig(), chain_id, 0)
                    .and_then(|key| crate::fidelity::snapshot_leagues(&key, &request));
            match leagues {
                Ok(leagues) => {
                    let inputs_canonical_hash =
                        outbe_tee::protocol::fidelity_snapshot_canonical_hash(&request);
                    let preimage = outbe_tee::protocol::fidelity_snapshot_attestation_preimage(
                        inputs_canonical_hash,
                        &leagues,
                    );
                    let attestation_tag = keys.sign_attestation(&preimage).to_vec();
                    EnclaveResponse::FidelityLeaguesSnapshotted {
                        leagues,
                        inputs_canonical_hash,
                        attestation_tag,
                    }
                }
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::QueryFidelityIndex { request } => {
            // Owner-authorized read (eth_call path, NOT consensus). The signed,
            // expiring authorization is verified inside the engine — the trust
            // boundary is the enclave, not the host that forwards the call.
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "QueryFidelityIndex: no resident group key (DKG not complete)"
                        .to_string(),
                };
            };
            let result =
                crate::fidelity::derive_fidelity_state_key(derived.group_sig(), chain_id, 0)
                    .and_then(|key| crate::fidelity::query_index(&key, chain_id, &request));
            match result {
                Ok(mut result) => {
                    let preimage = outbe_tee::protocol::fidelity_query_attestation_preimage(
                        result.inputs_canonical_hash,
                        &result,
                    );
                    result.attestation_tag = keys.sign_attestation(&preimage).to_vec();
                    EnclaveResponse::FidelityIndexQueried {
                        result: Box::new(result),
                    }
                }
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::ApplyPromisOp { request } => {
            // Same resident-key derivation + attestation as ApplyGratisOp, over the
            // independent Promis key domain (mint/burn on an encrypted balance).
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "ApplyPromisOp: no resident group key (DKG not complete)".to_string(),
                };
            };
            let state_key =
                match crate::promis::derive_promis_state_key(derived.group_sig(), chain_id, 0) {
                    Ok(k) => k,
                    Err(e) => {
                        return EnclaveResponse::Error {
                            message: e.to_string(),
                        }
                    }
                };
            let mut result = crate::promis::apply_op(&state_key, &request);
            let preimage = outbe_tee::protocol::promis_op_attestation_preimage(
                result.inputs_canonical_hash,
                &result,
            );
            result.attestation_tag = keys.sign_attestation(&preimage).to_vec();
            EnclaveResponse::PromisOpApplied {
                result: Box::new(result),
            }
        }
        EnclaveRequest::DeriveAccountKeys {
            ledger,
            account,
            requester_ephemeral_pubkey,
            owner_sig,
        } => {
            // OFF-CHAIN key delivery only (served over RPC, never during block
            // execution): derive the account's view + modify keys and seal them to
            // the requester's ephemeral X25519 key.
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "DeriveAccountKeys: no resident group key (DKG not complete)"
                        .to_string(),
                };
            };
            // Prove the caller controls `account` INSIDE the enclave before releasing
            // its (secret) view/modify keys. The host RPC recovers the same signature
            // as a fast reject, but a compromised host reaches this transport directly,
            // so the release decision must run in the enclave's trust domain, not the
            // host's. Same shared preimage + recover_signer the host uses (one impl, no
            // divergence).
            let Ok(sig65) = <[u8; 65]>::try_from(owner_sig.as_slice()) else {
                return EnclaveResponse::Error {
                    message: "DeriveAccountKeys: owner signature must be 65 bytes".to_string(),
                };
            };
            let prehash = outbe_tee::protocol::eip191_hash(
                &outbe_tee::protocol::derive_account_keys_message(
                    ledger,
                    account,
                    alloy_primitives::B256::from(requester_ephemeral_pubkey),
                ),
            );
            match outbe_primitives::tee_signatures::recover_signer(&prehash, &sig65) {
                Ok(signer) if signer == account => {}
                _ => {
                    return EnclaveResponse::Error {
                        message: "DeriveAccountKeys: owner signature does not control account"
                            .to_string(),
                    };
                }
            }
            let sealed = (|| -> crate::errors::Result<crate::crypto::EncryptedShare> {
                let domain = crate::confidential::domain_for(ledger);
                let state_key = domain.derive_state_key(derived.group_sig(), chain_id, 0)?;
                let view_key = domain.derive_view_key(&state_key, account)?;
                let modify_key = domain.derive_modify_key(&state_key, account)?;
                let mut plaintext = view_key.to_vec();
                plaintext.extend_from_slice(&modify_key);
                crate::crypto::encrypt_share(&requester_ephemeral_pubkey, &plaintext)
            })();
            match sealed {
                Ok(blob) => EnclaveResponse::AccountKeysSealed {
                    account,
                    sealed: blob.ciphertext,
                    nonce: blob.nonce,
                    enclave_ephemeral_pubkey: blob.ephemeral_pub,
                },
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::SealOfferKeyForRegistry { recipient_x25519 } => {
            // On-chain key delivery SERVER: DETERMINISTICALLY
            // seal the resident group signature to `recipient_x25519` so the blob can
            // be committed on-chain. static-static ECDH from the resident offer secret
            // makes every committee enclave produce a byte-identical blob; the
            // plaintext group signature never leaves the enclave. The registered
            // recipient opens it with `IngestSealedOfferKeyForRegistry`.
            let Some(derived) = offer_key.get() else {
                return EnclaveResponse::Error {
                    message: "SealOfferKeyForRegistry: no resident offer key to seal".to_string(),
                };
            };
            match crate::crypto::encrypt_share_deterministic(
                derived.secret(),
                &recipient_x25519,
                derived.group_sig(),
            ) {
                Ok(blob) => EnclaveResponse::SealedOfferKeyForRegistry {
                    sealed: blob.to_bytes(),
                },
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::IngestSealedOfferKeyForRegistry {
            sealed,
            expected_tribute_offer_public,
            chain_id,
            tribute_offer_epoch,
        } => {
            // One-time on-chain onboarding: decrypt the registry artifact with this
            // enclave's recipient secret and accept it only if its derived public key
            // matches the chain commitment. The key becomes resident write-once and
            // is then sealed for restart; there is no peer or recovery path.
            let derived = (|| -> crate::errors::Result<DerivedTributeOfferKey> {
                let blob = crate::crypto::EncryptedShare::from_bytes(&sealed)?;
                // Decrypt with the secret behind this enclave's advertised
                // `recipient_x25519` (its `tribute_offer` X25519 key) — the key the
                // finalized registry artifact was sealed to.
                let group_sig = Zeroizing::new(crate::crypto::decrypt_share(
                    keys.tribute_offer_x25519_secret(),
                    &blob,
                )?);
                let (secret, public) = crate::crypto::derive_tribute_offer_secret_from_group_sig(
                    group_sig.as_ref(),
                    chain_id,
                    tribute_offer_epoch,
                )?;
                if public != expected_tribute_offer_public {
                    return Err(crate::errors::TeeError::Dkg(
                        "sealed offer key rejected: derived public key != on-chain commitment"
                            .to_string(),
                    ));
                }
                Ok(DerivedTributeOfferKey::from_parts(
                    secret, public, group_sig,
                ))
            })();
            match derived {
                Ok(d) => {
                    let public = d.public();
                    // Write-once: idempotent on the same key; a divergent key is a
                    // determinism fault (reject, keep the resident key).
                    if let Err(rejected) = offer_key.set(d) {
                        if offer_key.get().map(|k| k.public) != Some(rejected.public) {
                            return EnclaveResponse::Error {
                                message: "offer key divergence: registry key differs from \
                                          the resident offer key"
                                    .to_string(),
                            };
                        }
                    }
                    EnclaveResponse::OfferKeyForRegistryIngested {
                        tribute_offer_public: public,
                    }
                }
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EnclaveRequest::DkgOpen {
            ceremony_id,
            round,
            participants,
        } => dispatch_dkg_open(keys, dkg, chain_id, ceremony_id, round, participants),
        EnclaveRequest::DkgStartDealer { ceremony_id } => {
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                let (pub_msg, sealed_shares) = s.start_dealer_encoded()?;
                Ok(EnclaveResponse::DkgDealt {
                    pub_msg,
                    sealed_shares,
                })
            }))
        }
        EnclaveRequest::DkgPlayerIngest {
            ceremony_id,
            dealer_bls,
            pub_msg,
            sealed_share,
        } => into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
            let ack = s.player_ingest_encoded(&dealer_bls, &pub_msg, &sealed_share)?;
            Ok(EnclaveResponse::DkgPlayerAck { ack })
        })),
        EnclaveRequest::DkgDealerReceiveAck {
            ceremony_id,
            player_bls,
            ack,
        } => into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
            s.dealer_receive_ack_encoded(&player_bls, &ack)?;
            Ok(EnclaveResponse::Ack)
        })),
        EnclaveRequest::DkgDealerFinalize { ceremony_id } => {
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                Ok(EnclaveResponse::DkgSignedLog {
                    signed_log: s.dealer_finalize_encoded()?,
                })
            }))
        }
        EnclaveRequest::DkgPlayerFinalize {
            ceremony_id,
            signed_logs,
        } => {
            // The session stays resident after finalize: Seam F (below) needs the
            // recovered share. It is released by `DkgFinalizeTributeOffer`.
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                let (group_public, share_commitment) = s.player_finalize_encoded(&signed_logs)?;
                Ok(EnclaveResponse::DkgPlayerFinalized {
                    group_public,
                    share_commitment,
                })
            }))
        }
        EnclaveRequest::DkgTributeOfferPartial { ceremony_id } => {
            into_response(dkg.get_mut(&ceremony_id.0).and_then(|s| {
                let sealed = s
                    .tribute_offer_partials_sealed()?
                    .into_iter()
                    .map(|(pk, blob)| {
                        (
                            commonware_codec::Encode::encode(&pk).to_vec(),
                            blob.to_bytes(),
                        )
                    })
                    .collect();
                Ok(EnclaveResponse::DkgTributeOfferPartial { sealed })
            }))
        }
        EnclaveRequest::DkgFinalizeTributeOffer {
            ceremony_id,
            sealed_partials,
            chain_id,
            tribute_offer_epoch,
        } => {
            // Complete founding Seam F and retain the group threshold signature
            // in sealed restart state. Authorization has already proved this is
            // a keyless Validator; a ready enclave cannot enter this arm.
            let result = dkg.get_mut(&ceremony_id.0).and_then(|s| {
                // The group public KEY (constant term) is the public verification key
                // carried into the bootstrap payload for later reshare-endorsement
                // checks; capture it while the session is still resident.
                let group_public_key = s.group_public_key_bytes()?;
                let (secret, public, group_sig) = s.recover_tribute_offer_secret(
                    &sealed_partials,
                    chain_id,
                    tribute_offer_epoch,
                )?;
                Ok((secret, public, group_sig, group_public_key))
            });
            match result {
                Ok((secret, public, group_sig, group_public_key)) => {
                    let derived = DerivedTributeOfferKey::from_parts(secret, public, group_sig);
                    // Write-once: the first ceremony's key is canonical. A re-run
                    // that derives the SAME key is idempotent; a DIVERGENT key is a
                    // determinism fault — reject rather than keep a stale key.
                    if let Err(rejected) = offer_key.set(derived) {
                        if offer_key.get().map(|k| k.public) != Some(rejected.public) {
                            return EnclaveResponse::Error {
                                message: "offer key divergence: recovered key differs from \
                                          the resident offer key"
                                    .to_string(),
                            };
                        }
                    }
                    // Release the ceremony's resident secret state.
                    dkg.remove(&ceremony_id.0);
                    EnclaveResponse::DkgTributeOfferKey {
                        tribute_offer_public: public,
                        group_public_key,
                    }
                }
                Err(e) => EnclaveResponse::Error {
                    message: e.to_string(),
                },
            }
        }
    }
}

#[cfg(feature = "mock")]
fn synthetic_dcap_quote(report_data: &[u8; 64]) -> Result<Vec<u8>, String> {
    let mut quote = vec![0_u8; outbe_tee::quote::MIN_QUOTE_LEN];
    let report_data_offset = quote
        .len()
        .checked_sub(report_data.len())
        .ok_or_else(|| "synthetic quote REPORT_DATA offset underflow".to_string())?;
    quote[report_data_offset..].copy_from_slice(report_data);
    Ok(quote)
}

fn dispatch_dkg_open(
    keys: &EnclaveKeys,
    dkg: &mut DkgSessionStore,
    chain_id: alloy_primitives::B256,
    ceremony_id: alloy_primitives::B256,
    round: u64,
    participants: Vec<outbe_tee::protocol::ParticipantAnnounce>,
) -> EnclaveResponse {
    let result = (|| {
        // The host relays each `(bls, enc, sig)` it gathered from peers' GetPublicKeys.
        // Before trusting any pairing: verify every enc key is signed by the BLS
        // identity it is paired with, and reject duplicate enc keys / identities — so
        // an untrusted host cannot mis-pair an enc key onto a foreign identity or
        // collapse two participants onto one enc key (cross-decryption of shares).
        let mut enc_by_bls = std::collections::BTreeMap::new();
        let mut seen_enc = std::collections::BTreeSet::new();
        let participant_bls: Vec<Vec<u8>> =
            participants.iter().map(|p| p.bls_pub.clone()).collect();
        for p in &participants {
            if !crate::keys::verify_dkg_enc_binding(&p.bls_pub, chain_id, &p.enc_pub, &p.enc_sig) {
                return Err(crate::errors::TeeError::Dkg(
                    "DkgOpen: enc-key identity binding failed verification".to_string(),
                ));
            }
            if !seen_enc.insert(p.enc_pub) {
                return Err(crate::errors::TeeError::Dkg(
                    "DkgOpen: duplicate enc key across participants".to_string(),
                ));
            }
            if enc_by_bls.insert(p.bls_pub.clone(), p.enc_pub).is_some() {
                return Err(crate::errors::TeeError::Dkg(
                    "DkgOpen: duplicate BLS identity across participants".to_string(),
                ));
            }
        }
        let (info, pubkeys) = build_ceremony_info(round, &participant_bls)?;
        let mut recipient_enc_keys = std::collections::BTreeMap::new();
        for pk in &pubkeys {
            let bls_bytes = commonware_codec::Encode::encode(pk).to_vec();
            let enc = enc_by_bls.get(&bls_bytes).ok_or_else(|| {
                crate::errors::TeeError::Dkg("DkgOpen: enc key missing for participant".to_string())
            })?;
            recipient_enc_keys.insert(pk.clone(), *enc);
        }
        dkg.open(
            ceremony_id.0,
            info,
            keys.tee_bls_key().clone(),
            keys.dkg_enc_secret(),
            recipient_enc_keys,
        )?;
        Ok(EnclaveResponse::Ack)
    })();
    into_response(result)
}

/// Map a seam `Result` into an `EnclaveResponse`, turning errors into the typed
/// `Error` response the host surfaces (never a panic).
fn into_response(result: crate::errors::Result<EnclaveResponse>) -> EnclaveResponse {
    result.unwrap_or_else(|e| EnclaveResponse::Error {
        message: e.to_string(),
    })
}

/// Re-parse the quote returned by Gramine before exposing it to NodeHost. This
/// keeps a stale/misbound device response from being paired with the requested
/// canonical intent even though the later consensus QVL remains authoritative.
fn validate_generated_quote_binding(
    expected_report_data: [u8; 64],
    quote: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let parsed = outbe_tee::quote::parse_quote_measurements(&quote)?;
    if parsed.report_data != expected_report_data {
        return Err("generated DCAP quote REPORT_DATA does not match requested intent".to_string());
    }
    Ok(quote)
}

/// Accept loop (used by the enclave binary). Each connection is served on its
/// own thread so multiple long-lived clients are handled concurrently — the node
/// keeps one connection open for offer decryption for its whole lifetime *and*
/// opens a second one for the startup TEE-bootstrap registration fetch; a
/// sequential loop would deadlock the second behind the first. `keys` is
/// read-only and shared via `Arc`; each connection still keeps its own
/// `DkgSessionStore`. A per-connection error is logged and never stops the
/// server.
pub fn serve(
    listener: &UnixListener,
    keys: Arc<EnclaveKeys>,
    boot: Option<Arc<EnclaveBootConfig>>,
    offer_key: SharedTributeOfferKey,
    initialization: Arc<InitializationState>,
    chain_id: alloy_primitives::B256,
) -> Result<(), TransportError> {
    // The DKG-derived offer key is shared across all connection threads: the DKG
    // ceremony connection writes it (Seam F), the offer-decrypt connection reads
    // it. `main` may pre-seed it from a sealed blob (restart fast-path).
    for conn in listener.incoming() {
        let stream = conn?;
        let keys = Arc::clone(&keys);
        let offer_key = Arc::clone(&offer_key);
        let boot = boot.clone();
        let initialization = Arc::clone(&initialization);
        std::thread::spawn(move || {
            // PoC: surface to stderr; one bad client must not kill the enclave.
            if let Err(err) = serve_connection_with_resident_chain(
                stream,
                &keys,
                &offer_key,
                boot.as_deref(),
                &initialization,
                chain_id,
                crate::gramine::dcap_quote,
            ) {
                eprintln!("tee enclave: connection error: {err}");
            }
        });
    }
    Ok(())
}

/// TCP accept loop — same thread-per-connection model as [`serve`], but over
/// TCP. Used when the enclave runs under Gramine, where pathname Unix domain
/// sockets are process-internal and a host process (the node) cannot reach them;
/// Gramine passes TCP through to the host network. The Noise-IK handshake still
/// authenticates + encrypts every byte, so TCP only changes the carrier, not the
/// confidentiality of the channel.
pub fn serve_tcp(
    listener: &TcpListener,
    keys: Arc<EnclaveKeys>,
    boot: Option<Arc<EnclaveBootConfig>>,
    offer_key: SharedTributeOfferKey,
    initialization: Arc<InitializationState>,
    chain_id: alloy_primitives::B256,
) -> Result<(), TransportError> {
    for conn in listener.incoming() {
        let stream = conn?;
        // Low-latency request/response (the protocol is many small round-trips).
        let _ = stream.set_nodelay(true);
        let keys = Arc::clone(&keys);
        let offer_key = Arc::clone(&offer_key);
        let boot = boot.clone();
        let initialization = Arc::clone(&initialization);
        std::thread::spawn(move || {
            if let Err(err) = serve_connection_with_resident_chain(
                stream,
                &keys,
                &offer_key,
                boot.as_deref(),
                &initialization,
                chain_id,
                crate::gramine::dcap_quote,
            ) {
                eprintln!("tee enclave: connection error: {err}");
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    #[test]
    fn generated_quote_binding_rejects_post_syscall_report_data_substitution() {
        let expected = [0xA1; 64];
        let mut quote = vec![0u8; outbe_tee::quote::MIN_QUOTE_LEN];
        let report_data_offset = outbe_tee::quote::MIN_QUOTE_LEN - 64;
        quote[report_data_offset..].copy_from_slice(&expected);
        assert_eq!(
            validate_generated_quote_binding(expected, quote.clone()).unwrap(),
            quote
        );

        let mut substituted = quote;
        substituted[report_data_offset] ^= 1;
        assert!(validate_generated_quote_binding(expected, substituted)
            .unwrap_err()
            .contains("does not match requested intent"));
    }

    fn spawn_production_connection(
        keys: Arc<EnclaveKeys>,
        boot: Arc<EnclaveBootConfig>,
        offer_key: SharedTributeOfferKey,
        initialization: Arc<InitializationState>,
    ) -> (
        std::os::unix::net::UnixStream,
        std::thread::JoinHandle<Result<(), TransportError>>,
    ) {
        let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        let handle = std::thread::spawn(move || {
            serve_connection_with(server, &keys, &offer_key, Some(&boot), &initialization)
        });
        (client, handle)
    }

    fn signed_initialization_manifest(
        keys: &EnclaveKeys,
        challenge: [u8; 32],
        node_host_noise_x25519: [u8; 32],
        profile: outbe_primitives::tee_attestation_v1::EnclaveProfile,
    ) -> (
        outbe_primitives::tee_attestation_v1::EnclaveInitializationManifestV1,
        [u8; 65],
    ) {
        use k256::ecdsa::signature::hazmat::PrehashSigner as _;
        use outbe_primitives::tee_attestation_v1::{
            EnclaveInitializationManifestV1, EnclaveProfile, NodeIdV1,
        };

        let signing = k256::ecdsa::SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
        let public = signing.verifying_key().to_encoded_point(false);
        let hash = alloy_primitives::keccak256(&public.as_bytes()[1..]);
        let mut address = [0u8; 20];
        address.copy_from_slice(&hash[12..]);
        let node_id = match profile {
            EnclaveProfile::Validator => NodeIdV1::Validator {
                address,
                bls_minpk_public: [0x32; 48],
            },
            EnclaveProfile::FullNode => NodeIdV1::FullNode {
                reth_p2p_public: signing
                    .verifying_key()
                    .to_encoded_point(true)
                    .as_bytes()
                    .try_into()
                    .unwrap(),
            },
        };
        let manifest = EnclaveInitializationManifestV1 {
            chain_id: [0x10; 32],
            genesis_hash: B256::repeat_byte(0x11),
            enclave_profile: profile,
            node_id,
            initialization_challenge: challenge,
            node_host_noise_x25519,
            recipient_x25519: keys.tribute_offer_public(),
            attestation_ed25519: keys.attestation_pub(),
            noise_responder_x25519: keys.noise_public(),
        };
        let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing
            .sign_prehash(manifest.authorization_hash().unwrap().as_slice())
            .unwrap();
        let mut signature_bytes = [0u8; 65];
        signature_bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
        signature_bytes[64] = recovery.to_byte();
        (manifest, signature_bytes)
    }

    #[cfg(all(feature = "native-dcap", target_arch = "x86_64", target_os = "linux"))]
    fn intent_bound_processor_fixture_wire_bytes() -> (Vec<u8>, Vec<u8>) {
        use outbe_primitives::tee_attestation_v1::{
            AttestationEvidenceV1, DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1,
            RegistrationIntentV1,
        };

        const ROOT: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/system/tee/tests/fixtures/",
            "intel-dcap-1.26-intent-bound-processor/"
        );
        let intent = RegistrationIntentV1::decode_canonical(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/system/tee/tests/fixtures/",
            "intel-dcap-1.26-intent-bound-processor/intent.bin"
        )))
        .unwrap();
        let components = [
            (
                DcapCollateralKind::PckCertificateChain,
                "pck-certificate-chain.pem0",
            ),
            (DcapCollateralKind::PckCrl, "pck.crl.der"),
            (
                DcapCollateralKind::PckCrlIssuerChain,
                "pck-crl-issuer-chain.pem",
            ),
            (DcapCollateralKind::RootCaCrl, "root-ca.crl.der"),
            (DcapCollateralKind::TcbInfo, "tcb-info.json"),
            (
                DcapCollateralKind::TcbInfoIssuerChain,
                "tcb-info-issuer-chain.pem",
            ),
            (DcapCollateralKind::QeIdentity, "qe-identity.json"),
            (
                DcapCollateralKind::QeIdentityIssuerChain,
                "qe-identity-issuer-chain.pem",
            ),
        ]
        .into_iter()
        .map(|(kind, name)| DcapCollateralComponentV1 {
            kind,
            bytes: std::fs::read(format!("{ROOT}{name}")).unwrap(),
        })
        .collect();
        let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
            intent,
            quote: std::fs::read(format!("{ROOT}quote.bin")).unwrap(),
            components,
        })
        .encode_canonical()
        .unwrap();
        let policy = std::fs::read(format!("{ROOT}policy.bin")).unwrap();
        (evidence, policy)
    }

    #[cfg(all(feature = "native-dcap", target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn authenticated_noise_rpc_replays_real_processor_acceptance_through_enclave_qvl() {
        use outbe_primitives::tee_attestation_v1::{
            AttestationEvidenceV1, AttestationMode, EnclaveProfile, GramineDirectEvidenceV1,
        };
        use outbe_tee::{
            dcap_protocol::{DcapPlatformTcbStatusV1, DcapRejectCodeV1, DcapVerificationOutcomeV1},
            AuthorizedEnclaveClient, NodeHostNoiseKey,
        };

        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("enclave.sock");
        let endpoint = socket.to_str().unwrap().to_string();
        let boot = Arc::new(EnclaveBootConfig::new(
            [0x10; 32],
            root.path().to_path_buf(),
            0,
        ));
        let keys = Arc::new(EnclaveKeys::new([0x73; 32], Some([0x73; 32])).unwrap());
        let initialization =
            Arc::new(InitializationState::production(boot.clone(), &keys).unwrap());
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let listener = UnixListener::bind(&socket).unwrap();
        let server_keys = keys.clone();
        let server_initialization = initialization.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                serve_connection_with(
                    stream,
                    &server_keys,
                    &offer_key,
                    Some(&boot),
                    &server_initialization,
                )
                .unwrap();
            }
        });

        let challenge = AuthorizedEnclaveClient::discover_endpoint(&endpoint).unwrap();
        let node_host_path = root.path().join("node-host-noise.key");
        let node_host = NodeHostNoiseKey::create_new(&node_host_path).unwrap();
        let (manifest, node_signature) = signed_initialization_manifest(
            &keys,
            challenge.challenge,
            node_host.public(),
            EnclaveProfile::Validator,
        );
        let mut client = AuthorizedEnclaveClient::initialize_endpoint(
            &endpoint,
            &manifest,
            &node_signature,
            &node_host,
        )
        .unwrap();
        let (evidence, policy) = intent_bound_processor_fixture_wire_bytes();

        let DcapVerificationOutcomeV1::Accepted(verdict) = client
            .verify_dcap_evidence_v1(&evidence, &policy, 1_785_491_440)
            .unwrap()
        else {
            panic!("testnet policy must accept the authenticated Processor fixture")
        };
        assert_eq!(
            verdict.platform_tcb_status,
            DcapPlatformTcbStatusV1::ConfigurationAndSWHardeningNeeded
        );
        assert_eq!(
            verdict.advisory_ids,
            vec!["INTEL-SA-00289".to_string(), "INTEL-SA-00615".to_string()]
        );

        let AttestationEvidenceV1::Dcap(dcap) =
            AttestationEvidenceV1::decode_canonical(&evidence).unwrap()
        else {
            unreachable!("fixture must be DCAP evidence")
        };
        let mut unattested_intent = dcap.intent;
        unattested_intent.attestation_mode = AttestationMode::GramineDirectDev;
        let unattested = AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
            intent: unattested_intent,
            dev_attestation_public: [0x91; 32],
            dev_signature: [0x92; 64],
        })
        .encode_canonical()
        .unwrap();
        assert_eq!(
            client
                .verify_dcap_evidence_v1(&unattested, &policy, 1_785_491_440)
                .unwrap(),
            DcapVerificationOutcomeV1::Rejected(DcapRejectCodeV1::EvidenceNonCanonical)
        );
        drop(client);
        server.join().unwrap();
    }

    /// One in-process enclave: its key material, resident DKG store, and the
    /// shared DKG-derived offer key slot.
    struct Enclave {
        keys: EnclaveKeys,
        dkg: DkgSessionStore,
        offer_key: SharedTributeOfferKey,
        chain_id: B256,
    }

    impl Enclave {
        fn new(seed: u8) -> Self {
            Self {
                keys: EnclaveKeys::new([seed; 32], None).expect("keys"),
                dkg: DkgSessionStore::new(),
                offer_key: Arc::new(OnceLock::new()),
                chain_id: B256::repeat_byte(0xC1),
            }
        }

        fn call(&mut self, req: EnclaveRequest) -> EnclaveResponse {
            dispatch(
                req,
                &self.keys,
                &mut self.dkg,
                &self.offer_key,
                self.chain_id,
            )
        }

        fn identity(&mut self) -> outbe_tee::protocol::ParticipantAnnounce {
            match self.call(EnclaveRequest::GetPublicKeys) {
                EnclaveResponse::PublicKeys {
                    tee_bls_pub,
                    dkg_enc_pub,
                    dkg_enc_sig,
                    ..
                } => outbe_tee::protocol::ParticipantAnnounce {
                    bls_pub: tee_bls_pub,
                    enc_pub: dkg_enc_pub,
                    enc_sig: dkg_enc_sig,
                },
                other => panic!("unexpected GetPublicKeys response: {other:?}"),
            }
        }
    }

    // --- Fix A (C1) adversarial: DkgOpen must reject host tampering of the
    // (bls, enc, sig) bundle before trusting any pairing ----------------------

    fn honest_announces(n: usize) -> (Vec<Enclave>, Vec<outbe_tee::protocol::ParticipantAnnounce>) {
        let mut enclaves: Vec<Enclave> = (0..n).map(|i| Enclave::new(i as u8 + 1)).collect();
        let participants = enclaves.iter_mut().map(|e| e.identity()).collect();
        (enclaves, participants)
    }

    fn open_on(
        enclave: &mut Enclave,
        participants: Vec<outbe_tee::protocol::ParticipantAnnounce>,
    ) -> EnclaveResponse {
        enclave.call(EnclaveRequest::DkgOpen {
            ceremony_id: B256::repeat_byte(0x11),
            round: 0,
            participants,
        })
    }

    #[test]
    fn dkg_open_rejects_forged_enc_signature() {
        let (mut enclaves, mut participants) = honest_announces(4);
        // Honest list opens fine.
        assert!(matches!(
            open_on(&mut enclaves[0], participants.clone()),
            EnclaveResponse::Ack
        ));
        // Corrupt one binding signature: the enclave must reject the whole open.
        participants[1].enc_sig[0] ^= 0xff;
        assert!(
            matches!(
                open_on(&mut enclaves[0], participants),
                EnclaveResponse::Error { .. }
            ),
            "forged enc_sig must be rejected"
        );
    }

    #[test]
    fn dkg_open_rejects_mispaired_enc_signature() {
        let (mut enclaves, mut participants) = honest_announces(4);
        // Swap two participants' signatures: each now pairs a (bls, enc) with the
        // OTHER party's signature, so neither binding verifies.
        participants.swap(0, 1);
        let s0 = participants[0].enc_sig.clone();
        participants[0].enc_sig = participants[1].enc_sig.clone();
        participants[1].enc_sig = s0;
        assert!(
            matches!(
                open_on(&mut enclaves[2], participants),
                EnclaveResponse::Error { .. }
            ),
            "mispaired enc signature must be rejected"
        );
    }

    #[test]
    fn dkg_open_rejects_duplicate_enc_key() {
        let (mut enclaves, mut participants) = honest_announces(4);
        // A host replays one party's full announce into another slot: the enc key
        // (and identity) now collide. The dedup guard must reject before open.
        participants[1] = participants[0].clone();
        assert!(
            matches!(
                open_on(&mut enclaves[3], participants),
                EnclaveResponse::Error { .. }
            ),
            "duplicate enc key must be rejected"
        );
    }

    #[test]
    fn dkg_open_rejects_wrong_chain_binding() {
        let (mut enclaves, mut participants) = honest_announces(4);
        // A participant whose enc binding was signed under a DIFFERENT chain_id
        // than the verifier's must be rejected (cross-chain replay defense).
        let mut foreign = Enclave::new(9);
        foreign.chain_id = B256::repeat_byte(0xEE);
        participants[0] = foreign.identity();
        assert!(
            matches!(
                open_on(&mut enclaves[1], participants),
                EnclaveResponse::Error { .. }
            ),
            "enc binding signed under a foreign chain_id must be rejected"
        );
    }

    /// Drive a full n-party TEE DKG ceremony through `dispatch` (the real protocol
    /// request/response path), exercising the byte serialization of every seam.
    /// Validates that the protocol-level ceremony converges to one group key with
    /// distinct per-party share commitments.
    #[test]
    fn dispatch_drives_full_dkg_ceremony_over_protocol() {
        let n = 4usize;
        let ceremony_id = B256::repeat_byte(0x7c);

        let mut enclaves: Vec<Enclave> = (0..n).map(|i| Enclave::new(i as u8 + 1)).collect();

        // Announce identities, build the participant list (bls + enc + binding sig).
        let participants: Vec<outbe_tee::protocol::ParticipantAnnounce> =
            enclaves.iter_mut().map(|e| e.identity()).collect();
        let participant_bls: Vec<Vec<u8>> =
            participants.iter().map(|p| p.bls_pub.clone()).collect();

        // Open the ceremony on every enclave.
        for e in enclaves.iter_mut() {
            let resp = e.call(EnclaveRequest::DkgOpen {
                ceremony_id,
                round: 0,
                participants: participants.clone(),
            });
            assert!(matches!(resp, EnclaveResponse::Ack), "DkgOpen: {resp:?}");
        }

        // Seam A: every enclave deals; collect (pub_msg, sealed shares per recipient).
        // `Deal` = (encoded pub_msg, recipient BLS -> sealed share bytes).
        type Deal = (Vec<u8>, std::collections::BTreeMap<Vec<u8>, Vec<u8>>);
        let mut deals: Vec<Deal> = Vec::new();
        for e in enclaves.iter_mut() {
            match e.call(EnclaveRequest::DkgStartDealer { ceremony_id }) {
                EnclaveResponse::DkgDealt {
                    pub_msg,
                    sealed_shares,
                } => deals.push((pub_msg, sealed_shares.into_iter().collect())),
                other => panic!("DkgStartDealer: {other:?}"),
            }
        }

        // Seams B + C: deliver dealer i's sealed share to player j, then the ack
        // back to dealer i.
        for i in 0..n {
            let dealer_bls = participant_bls[i].clone();
            let pub_msg = deals[i].0.clone();
            for j in 0..n {
                let player_bls = participant_bls[j].clone();
                let sealed_share = deals[i]
                    .1
                    .get(&player_bls)
                    .expect("sealed share for player")
                    .clone();
                let ack = match enclaves[j].call(EnclaveRequest::DkgPlayerIngest {
                    ceremony_id,
                    dealer_bls: dealer_bls.clone(),
                    pub_msg: pub_msg.clone(),
                    sealed_share,
                }) {
                    EnclaveResponse::DkgPlayerAck { ack } => ack.expect("valid dealing acks"),
                    other => panic!("DkgPlayerIngest: {other:?}"),
                };
                let resp = enclaves[i].call(EnclaveRequest::DkgDealerReceiveAck {
                    ceremony_id,
                    player_bls,
                    ack,
                });
                assert!(
                    matches!(resp, EnclaveResponse::Ack),
                    "DkgDealerReceiveAck: {resp:?}"
                );
            }
        }

        // Seam D: every dealer finalizes its signed log.
        let signed_logs: Vec<Vec<u8>> = enclaves
            .iter_mut()
            .map(
                |e| match e.call(EnclaveRequest::DkgDealerFinalize { ceremony_id }) {
                    EnclaveResponse::DkgSignedLog { signed_log } => signed_log,
                    other => panic!("DkgDealerFinalize: {other:?}"),
                },
            )
            .collect();

        // Seam E: every player verifies all logs and recovers its threshold share.
        let mut groups: Vec<Vec<u8>> = Vec::new();
        let mut commitments: Vec<B256> = Vec::new();
        for e in enclaves.iter_mut() {
            match e.call(EnclaveRequest::DkgPlayerFinalize {
                ceremony_id,
                signed_logs: signed_logs.clone(),
            }) {
                EnclaveResponse::DkgPlayerFinalized {
                    group_public,
                    share_commitment,
                } => {
                    groups.push(group_public);
                    commitments.push(share_commitment);
                }
                other => panic!("DkgPlayerFinalize: {other:?}"),
            }
        }

        // All parties agree on the group key; each holds a distinct share.
        assert!(
            groups.iter().all(|g| *g == groups[0]),
            "group key must agree"
        );
        assert!(!groups[0].is_empty());
        let mut sorted = commitments.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), n, "share commitments must be distinct");

        // Seam F: every enclave threshold-signs the fixed offer message and SEALS
        // its partial to every recipient (n² sealed blobs). The host only ever
        // relays ciphertexts: `(recipient_bls, sealed_blob)`. Each enclave then
        // decrypts in-SGX the blobs addressed to it and recovers the group
        // signature → shared offer key.
        let mut all_sealed: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for e in enclaves.iter_mut() {
            match e.call(EnclaveRequest::DkgTributeOfferPartial { ceremony_id }) {
                EnclaveResponse::DkgTributeOfferPartial { sealed } => all_sealed.extend(sealed),
                other => panic!("DkgTributeOfferPartial: {other:?}"),
            }
        }

        let chain_id = B256::repeat_byte(0xc1);
        let mut tribute_offer_keys: Vec<[u8; 32]> = Vec::new();
        for (i, e) in enclaves.iter_mut().enumerate() {
            let sealed_partials: Vec<Vec<u8>> = all_sealed
                .iter()
                .filter(|(recipient_bls, _)| *recipient_bls == participant_bls[i])
                .map(|(_, blob)| blob.clone())
                .collect();
            match e.call(EnclaveRequest::DkgFinalizeTributeOffer {
                ceremony_id,
                sealed_partials,
                chain_id,
                tribute_offer_epoch: 0,
            }) {
                EnclaveResponse::DkgTributeOfferKey {
                    tribute_offer_public,
                    group_public_key,
                } => {
                    assert!(
                        !group_public_key.is_empty(),
                        "group public key must be emitted at founding offer finalization"
                    );
                    tribute_offer_keys.push(tribute_offer_public);
                }
                other => panic!("DkgFinalizeTributeOffer: {other:?}"),
            }
        }
        // Every enclave derives the byte-identical shared offer public key.
        assert!(
            tribute_offer_keys
                .iter()
                .all(|k| *k == tribute_offer_keys[0]),
            "offer public key must agree across enclaves"
        );
        assert_ne!(tribute_offer_keys[0], [0u8; 32]);

        // The ceremony session is released after founding offer-key finalization.
        assert!(enclaves.iter().all(|e| e.dkg.is_empty()));
    }

    #[test]
    fn dispatch_unknown_ceremony_is_typed_error_not_panic() {
        let mut e = Enclave::new(1);
        let resp = e.call(EnclaveRequest::DkgStartDealer {
            ceremony_id: B256::repeat_byte(0xEE),
        });
        assert!(matches!(resp, EnclaveResponse::Error { .. }), "{resp:?}");
    }

    #[test]
    fn public_keys_distinguish_onboarding_recipient_from_permanent_offer_key() {
        let mut enclave = Enclave::new(0x31);
        let onboarding_recipient = enclave.keys.tribute_offer_public();
        match enclave.call(EnclaveRequest::GetPublicKeys) {
            EnclaveResponse::PublicKeys {
                offer_key_ready,
                recipient_x25519_pub,
                ..
            } => {
                assert!(!offer_key_ready);
                assert_eq!(recipient_x25519_pub, onboarding_recipient);
            }
            other => panic!("unexpected pre-ready response: {other:?}"),
        }

        let permanent = DerivedTributeOfferKey::from_secret_and_group_sig(
            Zeroizing::new([0x6a; 32]),
            Zeroizing::new(vec![0x44; 96]),
        );
        let permanent_public = permanent.public();
        assert!(enclave.offer_key.set(permanent).is_ok(), "install once");

        match enclave.call(EnclaveRequest::GetPublicKeys) {
            EnclaveResponse::PublicKeys {
                offer_key_ready,
                recipient_x25519_pub,
                ..
            } => {
                assert!(offer_key_ready);
                assert_eq!(recipient_x25519_pub, permanent_public);
                assert_ne!(recipient_x25519_pub, onboarding_recipient);
            }
            other => panic!("unexpected ready response: {other:?}"),
        }
    }

    // ---- Seal / unseal offer secret + share (cfg(test) mock sealing key) ----

    fn install_tribute_offer_key(
        secret: [u8; 32],
        share: Vec<u8>,
    ) -> (SharedTributeOfferKey, [u8; 32]) {
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let derived = DerivedTributeOfferKey::from_secret_and_group_sig(
            Zeroizing::new(secret),
            Zeroizing::new(share),
        );
        let public = derived.public();
        offer_key.set(derived).ok().expect("set offer key");
        (offer_key, public)
    }

    /// Seal the DKG-derived offer key + group signature, then a fresh boot unseals
    /// and restores the byte-identical offer public key AND the group signature —
    /// the restart fast-path that skips the ceremony.
    #[test]
    fn ws_c_seal_then_unseal_restores_tribute_offer_key_and_group_sig() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EnclaveBootConfig::new([0xCD; 32], dir.path().to_path_buf(), 2);
        let group_sig = vec![0x9b_u8; 96];
        let (offer_key, public) = install_tribute_offer_key([0x5a; 32], group_sig.clone());

        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg), &offer_key);
        assert!(cfg.sealed_root_path().exists(), "sealed blob written");

        let restored = unseal_tribute_offer_and_group_sig_on_boot(&cfg)
            .expect("read sealed state")
            .expect("unseal on boot");
        assert_eq!(restored.public(), public);
        assert_eq!(
            restored.group_sig(),
            group_sig.as_slice(),
            "group signature restored"
        );
    }

    /// vA seals at SVN 1; a vB enclave of the SAME signer (same mock MRSIGNER key,
    /// different build) boots at SVN 2 and unseals vA's blob — cross-version
    /// unseal with the anti-rollback floor satisfied.
    #[test]
    fn ws_c_cross_version_unseal_same_signer_key() {
        let dir = tempfile::tempdir().unwrap();
        let chain = [0xAB; 32];
        let cfg_a = EnclaveBootConfig::new(chain, dir.path().to_path_buf(), 1);
        let (offer_key, public) = install_tribute_offer_key([0x77; 32], vec![0x11; 36]);
        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg_a), &offer_key);

        let cfg_b = EnclaveBootConfig::new(chain, dir.path().to_path_buf(), 2);
        let restored = unseal_tribute_offer_and_group_sig_on_boot(&cfg_b)
            .expect("read sealed state")
            .expect("vB unseals vA blob");
        assert_eq!(restored.public(), public);
    }

    /// A blob sealed for one chain does not unseal under a different `chain_id`
    /// (it is bound into the AEAD AAD).
    #[test]
    fn ws_c_unseal_rejects_wrong_chain_id() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EnclaveBootConfig::new([0x01; 32], dir.path().to_path_buf(), 1);
        let (offer_key, _public) = install_tribute_offer_key([0x33; 32], vec![0x22; 36]);
        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg), &offer_key);

        let cfg_wrong = EnclaveBootConfig::new([0x02; 32], dir.path().to_path_buf(), 1);
        let error = match unseal_tribute_offer_and_group_sig_on_boot(&cfg_wrong) {
            Err(error) => error,
            Ok(_) => panic!("wrong-chain sealed state must fail closed"),
        };
        assert!(error.contains("lost its key"), "{error}");
        assert!(error.contains("no recovery or fallback"), "{error}");
    }

    #[test]
    fn ws_c_missing_blob_is_the_only_keyless_boot_result() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EnclaveBootConfig::new([0x18; 32], dir.path().to_path_buf(), 1);
        assert!(unseal_tribute_offer_and_group_sig_on_boot(&cfg)
            .expect("missing file is not corruption")
            .is_none());
    }

    #[test]
    fn ws_c_corrupt_blob_is_terminal_and_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EnclaveBootConfig::new([0x19; 32], dir.path().to_path_buf(), 1);
        let corrupt = b"corrupt-permanent-offer-key";
        std::fs::write(cfg.sealed_root_path(), corrupt).unwrap();

        let error = match unseal_tribute_offer_and_group_sig_on_boot(&cfg) {
            Err(error) => error,
            Ok(_) => panic!("corrupt sealed state must fail closed"),
        };
        assert!(error.contains("lost its key"), "{error}");
        assert!(error.contains("no recovery or fallback"), "{error}");
        assert_eq!(std::fs::read(cfg.sealed_root_path()).unwrap(), corrupt);
    }

    /// Sealing is write-once: a second call cannot replace the persisted key.
    #[test]
    fn ws_c_seal_is_write_once() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EnclaveBootConfig::new([0x09; 32], dir.path().to_path_buf(), 1);
        let (offer_key, public) = install_tribute_offer_key([0x44; 32], vec![0x33; 36]);
        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg), &offer_key);
        let first = std::fs::read(cfg.sealed_root_path()).unwrap();

        // A different resident key must not overwrite the existing blob.
        let (offer_key2, _other) = install_tribute_offer_key([0x55; 32], vec![0x44; 36]);
        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg), &offer_key2);
        let second = std::fs::read(cfg.sealed_root_path()).unwrap();
        assert_eq!(first, second, "seal is write-once");
        // The persisted key is still the first one.
        assert_eq!(
            unseal_tribute_offer_and_group_sig_on_boot(&cfg)
                .unwrap()
                .unwrap()
                .public(),
            public
        );
    }

    /// The encoded share is only present inside the AEAD ciphertext — it never
    /// appears as plaintext bytes in the on-disk blob (secret-at-rest invariant).
    #[test]
    fn ws_c_share_never_in_host_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EnclaveBootConfig::new([0x07; 32], dir.path().to_path_buf(), 1);
        let share = vec![0xC3_u8; 40];
        let (offer_key, _public) = install_tribute_offer_key([0x66; 32], share.clone());
        seal_tribute_offer_and_group_sig_if_configured(Some(&cfg), &offer_key);

        let blob = std::fs::read(cfg.sealed_root_path()).unwrap();
        assert!(
            !blob.windows(share.len()).any(|w| w == share.as_slice()),
            "raw share bytes must not appear in the sealed blob"
        );
    }

    // ---- One-time on-chain offer-key onboarding ----

    /// A committee enclave deterministically seals the resident group signature
    /// to the registered recipient. The recipient verifies the installed key
    /// against the on-chain commitment.
    #[test]
    fn registry_artifact_seals_and_installs_offer_key() {
        let chain_id = B256::repeat_byte(0xC1);
        let epoch = 0u64;
        // Any bytes act as the group signature for the HKDF derivation.
        let group_sig = vec![0x9b_u8; 96];
        let (secret, public) =
            crate::crypto::derive_tribute_offer_secret_from_group_sig(&group_sig, chain_id, epoch)
                .expect("derive offer public");

        // Server: install the resident group signature into its offer-key slot.
        let mut server = Enclave::new(1);
        server
            .offer_key
            .set(DerivedTributeOfferKey::from_parts(
                secret,
                public,
                Zeroizing::new(group_sig),
            ))
            .ok()
            .expect("install server offer key");

        // Newcomer: fresh enclave, no offer key; its X25519 recipient key is the
        // one-time registry artifact target.
        let mut newcomer = Enclave::new(2);
        let newcomer_enc = newcomer.keys.tribute_offer_public();

        let sealed = match server.call(EnclaveRequest::SealOfferKeyForRegistry {
            recipient_x25519: newcomer_enc,
        }) {
            EnclaveResponse::SealedOfferKeyForRegistry { sealed } => sealed,
            other => panic!("expected SealedOfferKeyForRegistry, got {other:?}"),
        };

        match newcomer.call(EnclaveRequest::IngestSealedOfferKeyForRegistry {
            sealed,
            expected_tribute_offer_public: public,
            chain_id,
            tribute_offer_epoch: epoch,
        }) {
            EnclaveResponse::OfferKeyForRegistryIngested {
                tribute_offer_public,
            } => assert_eq!(tribute_offer_public, public),
            other => panic!("expected OfferKeyForRegistryIngested, got {other:?}"),
        }
        assert_eq!(newcomer.offer_key.get().map(|k| k.public()), Some(public));
    }

    /// The recipient rejects a registry artifact whose derived key does not match
    /// the on-chain commitment.
    #[test]
    fn registry_artifact_rejects_wrong_expected_public() {
        let chain_id = B256::repeat_byte(0xC2);
        let epoch = 0u64;
        let group_sig = vec![0x44_u8; 96];
        let (secret, public) =
            crate::crypto::derive_tribute_offer_secret_from_group_sig(&group_sig, chain_id, epoch)
                .expect("derive");

        let mut server = Enclave::new(3);
        server
            .offer_key
            .set(DerivedTributeOfferKey::from_parts(
                secret,
                public,
                Zeroizing::new(group_sig),
            ))
            .ok()
            .expect("install");

        let mut newcomer = Enclave::new(4);
        let newcomer_enc = newcomer.keys.tribute_offer_public();
        let sealed = match server.call(EnclaveRequest::SealOfferKeyForRegistry {
            recipient_x25519: newcomer_enc,
        }) {
            EnclaveResponse::SealedOfferKeyForRegistry { sealed } => sealed,
            other => panic!("got {other:?}"),
        };

        // Claim a different expected public than the one the group signature yields.
        match newcomer.call(EnclaveRequest::IngestSealedOfferKeyForRegistry {
            sealed,
            expected_tribute_offer_public: [0xEE_u8; 32],
            chain_id,
            tribute_offer_epoch: epoch,
        }) {
            EnclaveResponse::Error { .. } => {}
            other => panic!("expected Error, got {other:?}"),
        }
        // The newcomer must NOT have installed any key.
        assert!(newcomer.offer_key.get().is_none());
    }

    /// An enclave with a resident group key installed, so `DeriveAccountKeys` can
    /// exercise the ready-state capability in isolation.
    fn resident_enclave(seed: u8) -> Enclave {
        let group_sig = vec![0x5a_u8; 96];
        let e = Enclave::new(seed);
        let (secret, public) =
            crate::crypto::derive_tribute_offer_secret_from_group_sig(&group_sig, e.chain_id, 0)
                .expect("derive");
        e.offer_key
            .set(DerivedTributeOfferKey::from_parts(
                secret,
                public,
                Zeroizing::new(group_sig),
            ))
            .ok()
            .expect("install");
        e
    }

    /// Deterministic secp256k1 signer and its EVM address.
    fn evm_signer(seed: u8) -> (k256::ecdsa::SigningKey, alloy_primitives::Address) {
        let sk = k256::ecdsa::SigningKey::from_slice(&[seed; 32]).expect("key");
        let point = sk.verifying_key().to_encoded_point(false);
        let addr = alloy_primitives::Address::from_slice(
            &alloy_primitives::keccak256(&point.as_bytes()[1..])[12..],
        );
        (sk, addr)
    }

    /// EIP-191 owner signature over the shared derive-keys message — the exact
    /// preimage the enclave arm recomputes.
    fn owner_sig(
        sk: &k256::ecdsa::SigningKey,
        account: alloy_primitives::Address,
        ephemeral: [u8; 32],
    ) -> Vec<u8> {
        use k256::ecdsa::signature::hazmat::PrehashSigner;
        let prehash =
            outbe_tee::protocol::eip191_hash(&outbe_tee::protocol::derive_gratis_keys_message(
                account,
                alloy_primitives::B256::from(ephemeral),
            ));
        let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
            sk.sign_prehash(prehash.as_slice()).expect("sign");
        let mut sig65 = [0u8; 65];
        sig65[..64].copy_from_slice(sig.to_bytes().as_slice());
        sig65[64] = recid.to_byte();
        sig65.to_vec()
    }

    #[test]
    fn derive_account_keys_requires_owner_sig() {
        let mut enclave = resident_enclave(1);
        let (owner, account) = evm_signer(7);
        let ephemeral = [0x22_u8; 32];

        // Correct owner signature -> keys sealed.
        let good = owner_sig(&owner, account, ephemeral);
        match enclave.call(EnclaveRequest::DeriveAccountKeys {
            ledger: outbe_tee::protocol::Ledger::Gratis,
            account,
            requester_ephemeral_pubkey: ephemeral,
            owner_sig: good,
        }) {
            EnclaveResponse::AccountKeysSealed { account: a, .. } => assert_eq!(a, account),
            other => panic!("expected AccountKeysSealed, got {other:?}"),
        }

        // A signature by a DIFFERENT key over the same account: an attacker who does
        // not control `account` (e.g. a compromised host) cannot obtain its keys.
        let (attacker, attacker_addr) = evm_signer(9);
        assert_ne!(attacker_addr, account);
        let forged = owner_sig(&attacker, account, ephemeral);
        assert!(
            matches!(
                enclave.call(EnclaveRequest::DeriveAccountKeys {
                    ledger: outbe_tee::protocol::Ledger::Gratis,
                    account,
                    requester_ephemeral_pubkey: ephemeral,
                    owner_sig: forged,
                }),
                EnclaveResponse::Error { .. }
            ),
            "keys must not be released without a signature by `account`"
        );
    }

    #[test]
    fn derive_account_keys_ephemeral_binding() {
        let mut enclave = resident_enclave(2);
        let (owner, account) = evm_signer(7);
        let e1 = [0x11_u8; 32];
        let e2 = [0x99_u8; 32];

        // A valid signature over ephemeral E1 cannot be replayed to seal keys to a
        // different requester ephemeral E2 (the signed message binds the ephemeral).
        let sig_over_e1 = owner_sig(&owner, account, e1);
        assert!(
            matches!(
                enclave.call(EnclaveRequest::DeriveAccountKeys {
                    ledger: outbe_tee::protocol::Ledger::Gratis,
                    account,
                    requester_ephemeral_pubkey: e2,
                    owner_sig: sig_over_e1,
                }),
                EnclaveResponse::Error { .. }
            ),
            "owner_sig must bind the requester ephemeral (no cross-target replay)"
        );
    }

    #[test]
    fn production_initialization_binds_noise_initiator_before_request_decode() {
        use outbe_tee::codec::{decode_response, encode_request};

        let root = tempfile::tempdir().unwrap();
        let boot = Arc::new(EnclaveBootConfig::new(
            [0x10; 32],
            root.path().to_path_buf(),
            0,
        ));
        let keys = Arc::new(EnclaveKeys::new([7; 32], Some([1; 32])).unwrap());
        let initialization =
            Arc::new(InitializationState::production(boot.clone(), &keys).unwrap());
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());

        let challenge = match initialization.challenge_response(&keys).unwrap() {
            EnclaveResponse::InitializationChallenge { challenge, .. } => challenge,
            response => panic!("unexpected challenge response: {response:?}"),
        };
        let node_host_private = [0x51; 32];
        let node_host_public =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(node_host_private))
                .to_bytes();
        let (manifest, node_signature) = signed_initialization_manifest(
            &keys,
            challenge,
            node_host_public,
            outbe_primitives::tee_attestation_v1::EnclaveProfile::Validator,
        );

        // First connection: signed manifest plus possession of the exact embedded
        // NodeHost Noise key commits the write-once authorization.
        let (mut client, server) = spawn_production_connection(
            keys.clone(),
            boot.clone(),
            offer_key.clone(),
            initialization.clone(),
        );
        write_frame(
            &mut client,
            &encode_request(&EnclaveRequest::Initialize {
                manifest: manifest.encode_canonical().unwrap(),
                node_signature: node_signature.to_vec(),
            })
            .unwrap(),
        )
        .unwrap();
        let params = NOISE_PARAMS.parse().unwrap();
        let mut handshake = snow::Builder::new(params)
            .local_private_key(&node_host_private)
            .remote_public_key(&keys.noise_public())
            .build_initiator()
            .unwrap();
        let mut buf = [0u8; 2048];
        let len = handshake.write_message(&[], &mut buf).unwrap();
        write_frame(&mut client, &buf[..len]).unwrap();
        let msg2 = read_frame(&mut client).unwrap();
        handshake.read_message(&msg2, &mut buf).unwrap();
        let mut noise = handshake.into_transport_mode().unwrap();
        let frame = read_frame(&mut client).unwrap();
        let len = noise.read_message(&frame, &mut buf).unwrap();
        assert!(matches!(
            decode_response(&buf[..len]).unwrap(),
            EnclaveResponse::Initialized {
                sealed_loaded: false,
                ..
            }
        ));
        drop(client);
        server.join().unwrap().unwrap();
        assert_eq!(
            initialization.expected_node_host().unwrap(),
            node_host_public
        );

        // The authorized persistent key can reconnect and issue an allowed
        // command through the production channel.
        let (mut client, server) = spawn_production_connection(
            keys.clone(),
            boot.clone(),
            offer_key.clone(),
            initialization.clone(),
        );
        write_frame(
            &mut client,
            &encode_request(&EnclaveRequest::OpenSession).unwrap(),
        )
        .unwrap();
        let params = NOISE_PARAMS.parse().unwrap();
        let mut handshake = snow::Builder::new(params)
            .local_private_key(&node_host_private)
            .remote_public_key(&keys.noise_public())
            .build_initiator()
            .unwrap();
        let len = handshake.write_message(&[], &mut buf).unwrap();
        write_frame(&mut client, &buf[..len]).unwrap();
        let msg2 = read_frame(&mut client).unwrap();
        handshake.read_message(&msg2, &mut buf).unwrap();
        let mut noise = handshake.into_transport_mode().unwrap();
        let request = encode_request(&EnclaveRequest::GetPublicKeys).unwrap();
        let len = noise.write_message(&request, &mut buf).unwrap();
        write_frame(&mut client, &buf[..len]).unwrap();
        let frame = read_frame(&mut client).unwrap();
        let len = noise.read_message(&frame, &mut buf).unwrap();
        assert!(matches!(
            decode_response(&buf[..len]).unwrap(),
            EnclaveResponse::PublicKeys { .. }
        ));
        drop(client);
        server.join().unwrap().unwrap();

        // An unknown initiator is rejected after Noise message 1 even when that
        // message carries bytes that are not a valid EnclaveRequest. The server
        // never writes message 2 and therefore cannot decode or dispatch them.
        let (mut attacker, server) =
            spawn_production_connection(keys.clone(), boot, offer_key, initialization.clone());
        write_frame(
            &mut attacker,
            &encode_request(&EnclaveRequest::OpenSession).unwrap(),
        )
        .unwrap();
        let attacker_private = [0xA5; 32];
        let params = NOISE_PARAMS.parse().unwrap();
        let mut attacker_handshake = snow::Builder::new(params)
            .local_private_key(&attacker_private)
            .remote_public_key(&keys.noise_public())
            .build_initiator()
            .unwrap();
        let len = attacker_handshake
            .write_message(&[0xff; 32], &mut buf)
            .unwrap();
        write_frame(&mut attacker, &buf[..len]).unwrap();
        assert!(read_frame(&mut attacker).is_err());
        let error = server.join().unwrap().unwrap_err();
        assert!(error
            .to_string()
            .contains("initiator is not the authorized NodeHost"));
        assert_eq!(
            initialization.expected_node_host().unwrap(),
            node_host_public
        );
    }

    #[test]
    fn public_node_host_client_initializes_and_reconnects_both_profiles() {
        use outbe_primitives::tee_attestation_v1::EnclaveProfile;
        use outbe_tee::{AuthorizedEnclaveClient, NodeHostNoiseKey};

        for (profile, seed) in [
            (EnclaveProfile::Validator, 0x71),
            (EnclaveProfile::FullNode, 0x72),
        ] {
            let root = tempfile::tempdir().unwrap();
            let socket = root.path().join("enclave.sock");
            let endpoint = socket.to_str().unwrap().to_string();
            let boot = Arc::new(EnclaveBootConfig::new(
                [0x10; 32],
                root.path().to_path_buf(),
                0,
            ));
            let keys = Arc::new(EnclaveKeys::new([seed; 32], Some([seed; 32])).unwrap());
            let initialization =
                Arc::new(InitializationState::production(boot.clone(), &keys).unwrap());
            let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
            let listener = UnixListener::bind(&socket).unwrap();
            let server_keys = keys.clone();
            let server_boot = boot.clone();
            let server_initialization = initialization.clone();
            let server_offer_key = offer_key.clone();
            let server = std::thread::spawn(move || {
                for _ in 0..3 {
                    let (stream, _) = listener.accept().unwrap();
                    serve_connection_with(
                        stream,
                        &server_keys,
                        &server_offer_key,
                        Some(&server_boot),
                        &server_initialization,
                    )
                    .unwrap();
                }
            });

            let challenge = AuthorizedEnclaveClient::discover_endpoint(&endpoint).unwrap();
            assert_eq!(challenge.recipient_x25519, keys.tribute_offer_public());
            assert_eq!(challenge.attestation_ed25519, keys.attestation_pub());
            assert_eq!(challenge.noise_responder_x25519, keys.noise_public());

            let node_host_path = root.path().join("node-host-noise.key");
            let node_host = NodeHostNoiseKey::create_new(&node_host_path).unwrap();
            let (manifest, node_signature) = signed_initialization_manifest(
                &keys,
                challenge.challenge,
                node_host.public(),
                profile,
            );
            let mut initialized = AuthorizedEnclaveClient::initialize_endpoint(
                &endpoint,
                &manifest,
                &node_signature,
                &node_host,
            )
            .unwrap();
            assert!(matches!(
                initialized.request(&EnclaveRequest::GetPublicKeys).unwrap(),
                EnclaveResponse::PublicKeys { .. }
            ));
            drop(initialized);

            drop(node_host);
            let node_host = NodeHostNoiseKey::load(&node_host_path).unwrap();
            let mut reconnected =
                AuthorizedEnclaveClient::connect_endpoint(&endpoint, &manifest, &node_host)
                    .unwrap();
            assert!(matches!(
                reconnected.request(&EnclaveRequest::GetPublicKeys).unwrap(),
                EnclaveResponse::PublicKeys { .. }
            ));
            drop(reconnected);
            server.join().unwrap();
            assert_eq!(
                initialization.expected_node_host().unwrap(),
                node_host.public()
            );
        }
    }

    #[test]
    fn preauthenticated_remote_ticket_opens_one_live_noise_session_and_cannot_replay() {
        use outbe_primitives::tee_attestation_v1::{
            EnclaveProfile, NodeHostAuthorizationWitnessV1, NodeIdV1,
        };
        use outbe_tee::{
            admit_remote_session_v1, AuthorizedEnclaveClient, FinalizedRegistryBindingV1,
            FinalizedRegistryViewV1, NodeHostNoiseKey, RemoteEnclaveClient,
            RemoteSessionExpectationV1,
        };

        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("remote-enclave.sock");
        let endpoint = socket.to_str().unwrap().to_string();
        let boot = Arc::new(EnclaveBootConfig::new(
            [0x10; 32],
            root.path().to_path_buf(),
            0,
        ));
        let keys = Arc::new(EnclaveKeys::new([0xA1; 32], Some([0xA1; 32])).unwrap());
        let initialization =
            Arc::new(InitializationState::production(boot.clone(), &keys).unwrap());
        let challenge = match initialization.challenge_response(&keys).unwrap() {
            EnclaveResponse::InitializationChallenge { challenge, .. } => challenge,
            response => panic!("unexpected challenge response: {response:?}"),
        };
        let listener = UnixListener::bind(&socket).unwrap();
        let server_keys = keys.clone();
        let server_boot = boot.clone();
        let server_initialization = initialization.clone();
        let server = std::thread::spawn(move || {
            let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
            let mut connections = Vec::new();
            for _ in 0..4 {
                let (stream, _) = listener.accept().unwrap();
                let keys = server_keys.clone();
                let boot = server_boot.clone();
                let initialization = server_initialization.clone();
                let offer_key = offer_key.clone();
                connections.push(std::thread::spawn(move || {
                    serve_connection_with(stream, &keys, &offer_key, Some(&boot), &initialization)
                }));
            }
            connections
                .into_iter()
                .map(|connection| connection.join().unwrap())
                .collect::<Vec<_>>()
        });

        let owner_path = root.path().join("owner-noise.key");
        let owner = NodeHostNoiseKey::create_new(&owner_path).unwrap();
        let (manifest, node_signature) = signed_initialization_manifest(
            &keys,
            challenge,
            owner.public(),
            EnclaveProfile::Validator,
        );
        let mut owner_client = AuthorizedEnclaveClient::initialize_endpoint(
            &endpoint,
            &manifest,
            &node_signature,
            &owner,
        )
        .unwrap();

        let source_path = root.path().join("source-noise.key");
        let source = NodeHostNoiseKey::create_new(&source_path).unwrap();
        let source_node = NodeIdV1::Validator {
            address: [0xB1; 20],
            bls_minpk_public: [0xB2; 48],
        };
        let source_witness = NodeHostAuthorizationWitnessV1 {
            chain_id: manifest.chain_id,
            genesis_hash: manifest.genesis_hash,
            enclave_profile: EnclaveProfile::Validator,
            node_id: source_node.clone(),
            node_host_noise_x25519: source.public(),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(matches!(
            owner_client.request(&EnclaveRequest::AuthorizeRemoteSessionV1 {
                ticket_id: B256::repeat_byte(0xA2),
                initiator_static_x25519: source.public(),
                responder_static_x25519: manifest.noise_responder_x25519,
                deadline: now + 60,
                finalized_block_hash: B256::repeat_byte(0xA3),
            }),
            Err(outbe_tee::TransportError::Handshake(message))
                if message.contains("finalized admission capability")
        ));
        let view = FinalizedRegistryViewV1 {
            chain_id: manifest.chain_id,
            genesis_hash: manifest.genesis_hash,
            block_number: 101,
            block_hash: B256::repeat_byte(0xB3),
            state_root: B256::repeat_byte(0xB4),
            consensus_timestamp: now.saturating_sub(1),
        };
        let source_binding = FinalizedRegistryBindingV1 {
            view,
            node_id_hash: source_node.node_id_hash().unwrap(),
            profile: EnclaveProfile::Validator,
            enclave_id: B256::repeat_byte(0xB5),
            binding_id: B256::repeat_byte(0xB6),
            intent_hash: B256::repeat_byte(0xB7),
            valid_until: now + 600,
            noise_responder_x25519: [0xB8; 32],
            node_host_authorization_hash: source_witness.authorization_hash().unwrap(),
        };
        let target_hash = manifest.node_id.node_id_hash().unwrap();
        let target_binding = FinalizedRegistryBindingV1 {
            view,
            node_id_hash: target_hash,
            profile: EnclaveProfile::Validator,
            enclave_id: manifest.enclave_id().unwrap(),
            binding_id: B256::repeat_byte(0xC1),
            intent_hash: B256::repeat_byte(0xC2),
            valid_until: now + 500,
            noise_responder_x25519: manifest.noise_responder_x25519,
            node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
        };
        let admission = admit_remote_session_v1(
            RemoteSessionExpectationV1 {
                chain_id: manifest.chain_id,
                genesis_hash: manifest.genesis_hash,
                source_node_id_hash: source_binding.node_id_hash,
                source_profile: EnclaveProfile::Validator,
                target_node_id_hash: target_hash,
                target_profile: EnclaveProfile::Validator,
            },
            &source_witness,
            source_binding,
            target_binding,
        )
        .unwrap();
        let wrong_ticket = owner_client.authorize_remote_session(&admission).unwrap();
        let mut wrong_stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        wrong_stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        write_frame(
            &mut wrong_stream,
            &outbe_tee::codec::encode_request(&EnclaveRequest::OpenRemoteSessionV1 {
                ticket_id: wrong_ticket.ticket_id(),
            })
            .unwrap(),
        )
        .unwrap();
        let params = NOISE_PARAMS.parse().unwrap();
        let mut wrong_handshake = snow::Builder::new(params)
            .local_private_key(&[0xD1; 32])
            .remote_public_key(&manifest.noise_responder_x25519)
            .build_initiator()
            .unwrap();
        let mut message = [0_u8; 1024];
        let message_len = wrong_handshake.write_message(&[], &mut message).unwrap();
        write_frame(&mut wrong_stream, &message[..message_len]).unwrap();
        assert!(read_frame(&mut wrong_stream).is_err());

        let ticket = owner_client.authorize_remote_session(&admission).unwrap();
        let mut remote = RemoteEnclaveClient::connect_endpoint(&endpoint, &ticket, &source)
            .expect("exact registered source NodeHost must complete Noise IK");
        let remote_keys = remote.public_keys().unwrap();
        assert_eq!(
            remote_keys.noise_static_pub,
            manifest.noise_responder_x25519
        );
        assert_eq!(remote_keys.recipient_x25519_pub, manifest.recipient_x25519);
        assert_eq!(remote_keys.attestation_pub, manifest.attestation_ed25519);
        drop(remote);

        assert!(RemoteEnclaveClient::connect_endpoint(&endpoint, &ticket, &source).is_err());
        drop(owner_client);
        let results = server.join().unwrap();
        assert!(results[0].is_ok());
        assert!(results[1].is_err());
        assert!(results[2].is_ok());
        assert!(results[3].is_err());
    }

    #[test]
    fn validator_node_host_state_initializes_once_and_reconnects_from_datadir() {
        use alloy_primitives::{Address, U256};
        use k256::ecdsa::signature::hazmat::PrehashSigner as _;
        use outbe_tee::{connect_or_initialize_validator_enclave, ValidatorNodeHostIdentityV1};

        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("enclave.sock");
        let endpoint = socket.to_str().unwrap().to_string();
        let chain_id = 16_u64;
        let chain_id_word = U256::from(chain_id).to_be_bytes();
        let genesis_hash = B256::repeat_byte(0x11);
        let boot = Arc::new(EnclaveBootConfig::new(
            chain_id_word,
            root.path().join("enclave-state"),
            0,
        ));
        std::fs::create_dir(&boot.tee_dir).unwrap();
        let keys = Arc::new(EnclaveKeys::new([0x74; 32], Some([0x74; 32])).unwrap());
        let initialization =
            Arc::new(InitializationState::production(boot.clone(), &keys).unwrap());
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let listener = UnixListener::bind(&socket).unwrap();
        let server_keys = keys.clone();
        let server_initialization = initialization.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let (stream, _) = listener.accept().unwrap();
                serve_connection_with(
                    stream,
                    &server_keys,
                    &offer_key,
                    Some(&boot),
                    &server_initialization,
                )
                .unwrap();
            }
        });

        let signing = k256::ecdsa::SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
        let public = signing.verifying_key().to_encoded_point(false);
        let hash = alloy_primitives::keccak256(&public.as_bytes()[1..]);
        let validator = Address::from_slice(&hash[12..]);
        let identity = ValidatorNodeHostIdentityV1 {
            chain_id,
            genesis_hash,
            validator,
            consensus_bls_public: [0x32; 48],
        };
        let sign = |hash: B256| {
            let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing
                .sign_prehash(hash.as_slice())
                .map_err(|error| error.to_string())?;
            let mut bytes = [0_u8; 65];
            bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
            bytes[64] = recovery.to_byte();
            Ok(bytes)
        };
        let node_data_dir = root.path().join("node-data");
        std::fs::create_dir(&node_data_dir).unwrap();

        let mut initialized =
            connect_or_initialize_validator_enclave(&endpoint, &node_data_dir, identity, &sign)
                .unwrap();
        assert!(matches!(
            initialized.request(&EnclaveRequest::GetPublicKeys).unwrap(),
            EnclaveResponse::PublicKeys { .. }
        ));
        drop(initialized);

        let mut reconnected =
            connect_or_initialize_validator_enclave(&endpoint, &node_data_dir, identity, &sign)
                .unwrap();
        assert!(matches!(
            reconnected.request(&EnclaveRequest::GetPublicKeys).unwrap(),
            EnclaveResponse::PublicKeys { .. }
        ));
        drop(reconnected);
        server.join().unwrap();

        let state = node_data_dir.join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1);
        assert!(state
            .join(outbe_tee::node_host::NODE_HOST_NOISE_KEY_V1)
            .is_file());
        assert!(state
            .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1)
            .is_file());
    }

    #[test]
    fn validator_replacement_candidate_keeps_the_committed_enclave_active() {
        use alloy_primitives::{Address, U256};
        use k256::ecdsa::signature::hazmat::PrehashSigner as _;
        use outbe_primitives::tee_attestation_v1::{
            AttestationEvidenceV1, AttestationMode, AttestationOperationV1,
            DcapCollateralComponentV1, DcapCollateralKind, DcapEvidenceV1,
            EnclaveInitializationManifestV1, RegistrationIntentV1,
        };
        use outbe_tee::{
            connect_or_initialize_validator_enclave, persist_replacement_candidate_submission,
            prepare_validator_enclave_replacement_candidate, ValidatorNodeHostIdentityV1,
        };

        let root = tempfile::tempdir().unwrap();
        let socket_a = root.path().join("enclave-a.sock");
        let socket_b = root.path().join("enclave-b.sock");
        let endpoint_a = socket_a.to_str().unwrap().to_string();
        let endpoint_b = socket_b.to_str().unwrap().to_string();
        let chain_id = 18_u64;
        let chain_id_word = U256::from(chain_id).to_be_bytes();
        let genesis_hash = B256::repeat_byte(0x13);

        let boot_a = Arc::new(EnclaveBootConfig::new(
            chain_id_word,
            root.path().join("enclave-a-state"),
            0,
        ));
        let boot_b = Arc::new(EnclaveBootConfig::new(
            chain_id_word,
            root.path().join("enclave-b-state"),
            0,
        ));
        std::fs::create_dir(&boot_a.tee_dir).unwrap();
        std::fs::create_dir(&boot_b.tee_dir).unwrap();
        let keys_a = Arc::new(EnclaveKeys::new([0x76; 32], Some([0x76; 32])).unwrap());
        let keys_b = Arc::new(EnclaveKeys::new([0x77; 32], Some([0x77; 32])).unwrap());
        let initialization_a =
            Arc::new(InitializationState::production(boot_a.clone(), &keys_a).unwrap());
        let initialization_b =
            Arc::new(InitializationState::production(boot_b.clone(), &keys_b).unwrap());

        let listener_a = UnixListener::bind(&socket_a).unwrap();
        let server_keys_a = keys_a.clone();
        let server_a = std::thread::spawn(move || {
            let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
            for _ in 0..3 {
                let (stream, _) = listener_a.accept().unwrap();
                serve_connection_with(
                    stream,
                    &server_keys_a,
                    &offer_key,
                    Some(&boot_a),
                    &initialization_a,
                )
                .unwrap();
            }
        });
        let listener_b = UnixListener::bind(&socket_b).unwrap();
        let server_keys_b = keys_b.clone();
        let server_b = std::thread::spawn(move || {
            let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
            for _ in 0..3 {
                let (stream, _) = listener_b.accept().unwrap();
                serve_connection_with(
                    stream,
                    &server_keys_b,
                    &offer_key,
                    Some(&boot_b),
                    &initialization_b,
                )
                .unwrap();
            }
        });

        let signing = k256::ecdsa::SigningKey::from_bytes((&[0x63; 32]).into()).unwrap();
        let public = signing.verifying_key().to_encoded_point(false);
        let hash = alloy_primitives::keccak256(&public.as_bytes()[1..]);
        let validator = Address::from_slice(&hash[12..]);
        let identity = ValidatorNodeHostIdentityV1 {
            chain_id,
            genesis_hash,
            validator,
            consensus_bls_public: [0x33; 48],
        };
        let sign = |hash: B256| {
            let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing
                .sign_prehash(hash.as_slice())
                .map_err(|error| error.to_string())?;
            let mut bytes = [0_u8; 65];
            bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
            bytes[64] = recovery.to_byte();
            Ok(bytes)
        };
        let node_data_dir = root.path().join("node-data");
        std::fs::create_dir(&node_data_dir).unwrap();

        drop(
            connect_or_initialize_validator_enclave(&endpoint_a, &node_data_dir, identity, &sign)
                .unwrap(),
        );
        let candidate = prepare_validator_enclave_replacement_candidate(
            &endpoint_b,
            &node_data_dir,
            identity,
            &sign,
        )
        .unwrap();
        let active_bytes = std::fs::read(
            node_data_dir
                .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
                .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1),
        )
        .unwrap();
        let active = EnclaveInitializationManifestV1::decode_canonical(&active_bytes).unwrap();
        assert_ne!(
            active.enclave_id().unwrap(),
            candidate.manifest().enclave_id().unwrap()
        );
        assert_eq!(
            active.node_host_authorization_hash().unwrap(),
            candidate.manifest().node_host_authorization_hash().unwrap()
        );
        let candidate_manifest = candidate.manifest().clone();
        let candidate_path = node_data_dir
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .join(outbe_tee::node_host::NODE_HOST_REPLACEMENT_CANDIDATE_V1);
        let candidate_bytes = std::fs::read(&candidate_path).unwrap();
        drop(candidate);
        let resumed = prepare_validator_enclave_replacement_candidate(
            &endpoint_b,
            &node_data_dir,
            identity,
            &sign,
        )
        .unwrap();
        assert_eq!(resumed.manifest(), &candidate_manifest);
        assert_eq!(std::fs::read(&candidate_path).unwrap(), candidate_bytes);
        drop(resumed);

        let intent = RegistrationIntentV1 {
            chain_id: candidate_manifest.chain_id,
            genesis_hash: candidate_manifest.genesis_hash,
            operation: AttestationOperationV1::ReplaceEnclaveBinding,
            attestation_mode: AttestationMode::DcapRequired,
            policy_hash: B256::repeat_byte(0x21),
            enclave_profile: candidate_manifest.enclave_profile,
            node_id: candidate_manifest.node_id.clone(),
            enclave_id: candidate_manifest.enclave_id().unwrap(),
            binding_id: B256::repeat_byte(0x45),
            binding_version: 2,
            registration_version: 1,
            renewal_nonce: 0,
            transition_nonce: 0,
            requested_valid_until: 20_000,
            recipient_x25519: candidate_manifest.recipient_x25519,
            attestation_ed25519: candidate_manifest.attestation_ed25519,
            noise_responder_x25519: candidate_manifest.noise_responder_x25519,
            node_host_authorization_hash: candidate_manifest
                .node_host_authorization_hash()
                .unwrap(),
        };
        candidate_manifest.validate_intent_binding(&intent).unwrap();
        let intent_hash = intent.intent_hash().unwrap();
        let node_signature = sign(intent_hash).unwrap();
        let enclave_signature = keys_b.sign_attestation(intent_hash.as_slice());
        let components = (1_u8..=8)
            .map(|kind| DcapCollateralComponentV1 {
                kind: DcapCollateralKind::try_from(kind).unwrap(),
                bytes: vec![kind],
            })
            .collect();
        let evidence = AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
            intent,
            quote: vec![0x51],
            components,
        });
        let submission = persist_replacement_candidate_submission(
            &node_data_dir,
            &evidence,
            &node_signature,
            &enclave_signature,
        )
        .unwrap();
        assert_eq!(
            persist_replacement_candidate_submission(
                &node_data_dir,
                &evidence,
                &node_signature,
                &enclave_signature,
            )
            .unwrap(),
            submission
        );
        let mut conflicting = evidence;
        let AttestationEvidenceV1::Dcap(conflicting_dcap) = &mut conflicting else {
            unreachable!();
        };
        conflicting_dcap.quote[0] ^= 1;
        assert!(persist_replacement_candidate_submission(
            &node_data_dir,
            &conflicting,
            &node_signature,
            &enclave_signature,
        )
        .unwrap_err()
        .to_string()
        .contains("conflicts with the durable replacement submission"));

        let mut active_client =
            connect_or_initialize_validator_enclave(&endpoint_a, &node_data_dir, identity, &sign)
                .unwrap();
        assert!(matches!(
            active_client
                .request(&EnclaveRequest::GetPublicKeys)
                .unwrap(),
            EnclaveResponse::PublicKeys { .. }
        ));
        drop(active_client);
        server_a.join().unwrap();
        server_b.join().unwrap();
    }

    #[test]
    fn replacement_candidate_resumes_after_crash_between_stage_and_initialize() {
        use alloy_primitives::{Address, U256};
        use k256::ecdsa::signature::hazmat::PrehashSigner as _;
        use outbe_tee::{
            connect_or_initialize_validator_enclave,
            prepare_validator_enclave_replacement_candidate, ValidatorNodeHostIdentityV1,
        };

        let root = tempfile::tempdir().unwrap();
        let active_socket = root.path().join("active.sock");
        let candidate_socket = root.path().join("candidate.sock");
        let active_endpoint = active_socket.to_str().unwrap().to_string();
        let candidate_endpoint = candidate_socket.to_str().unwrap().to_string();
        let chain_id = 20_u64;
        let chain_id_word = U256::from(chain_id).to_be_bytes();
        let genesis_hash = B256::repeat_byte(0x15);
        let active_boot = Arc::new(EnclaveBootConfig::new(
            chain_id_word,
            root.path().join("active-state"),
            0,
        ));
        let candidate_boot = Arc::new(EnclaveBootConfig::new(
            chain_id_word,
            root.path().join("candidate-state"),
            0,
        ));
        std::fs::create_dir(&active_boot.tee_dir).unwrap();
        std::fs::create_dir(&candidate_boot.tee_dir).unwrap();
        let active_keys = Arc::new(EnclaveKeys::new([0x79; 32], Some([0x79; 32])).unwrap());
        let candidate_keys = Arc::new(EnclaveKeys::new([0x7a; 32], Some([0x7a; 32])).unwrap());
        let active_initialization =
            Arc::new(InitializationState::production(active_boot.clone(), &active_keys).unwrap());
        let candidate_initialization = Arc::new(
            InitializationState::production(candidate_boot.clone(), &candidate_keys).unwrap(),
        );

        let active_listener = UnixListener::bind(&active_socket).unwrap();
        let active_server_keys = active_keys.clone();
        let active_server = std::thread::spawn(move || {
            let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
            for _ in 0..2 {
                let (stream, _) = active_listener.accept().unwrap();
                serve_connection_with(
                    stream,
                    &active_server_keys,
                    &offer_key,
                    Some(&active_boot),
                    &active_initialization,
                )
                .unwrap();
            }
        });

        let signing = k256::ecdsa::SigningKey::from_bytes((&[0x64; 32]).into()).unwrap();
        let public = signing.verifying_key().to_encoded_point(false);
        let address_hash = alloy_primitives::keccak256(&public.as_bytes()[1..]);
        let identity = ValidatorNodeHostIdentityV1 {
            chain_id,
            genesis_hash,
            validator: Address::from_slice(&address_hash[12..]),
            consensus_bls_public: [0x34; 48],
        };
        let sign = |hash: B256| {
            let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing
                .sign_prehash(hash.as_slice())
                .map_err(|error| error.to_string())?;
            let mut bytes = [0_u8; 65];
            bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
            bytes[64] = recovery.to_byte();
            Ok(bytes)
        };
        let node_data_dir = root.path().join("node-data");
        std::fs::create_dir(&node_data_dir).unwrap();
        drop(
            connect_or_initialize_validator_enclave(
                &active_endpoint,
                &node_data_dir,
                identity,
                &sign,
            )
            .unwrap(),
        );
        active_server.join().unwrap();

        let first_listener = UnixListener::bind(&candidate_socket).unwrap();
        let first_server_keys = candidate_keys.clone();
        let first_boot = candidate_boot.clone();
        let first_initialization = candidate_initialization.clone();
        let first_server = std::thread::spawn(move || {
            let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
            let (stream, _) = first_listener.accept().unwrap();
            serve_connection_with(
                stream,
                &first_server_keys,
                &offer_key,
                Some(&first_boot),
                &first_initialization,
            )
            .unwrap();
        });
        assert!(prepare_validator_enclave_replacement_candidate(
            &candidate_endpoint,
            &node_data_dir,
            identity,
            &sign,
        )
        .is_err());
        first_server.join().unwrap();
        std::fs::remove_file(&candidate_socket).unwrap();
        drop(candidate_initialization);
        let restarted_candidate_initialization = Arc::new(
            InitializationState::production(candidate_boot.clone(), &candidate_keys).unwrap(),
        );

        let retry_listener = UnixListener::bind(&candidate_socket).unwrap();
        retry_listener.set_nonblocking(true).unwrap();
        let retry_server_keys = candidate_keys.clone();
        let retry_server = std::thread::spawn(move || {
            let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut accepted = 0;
            while accepted < 3 && std::time::Instant::now() < deadline {
                match retry_listener.accept() {
                    Ok((stream, _)) => {
                        let result = serve_connection_with(
                            stream,
                            &retry_server_keys,
                            &offer_key,
                            Some(&candidate_boot),
                            &restarted_candidate_initialization,
                        );
                        if let Err(error) = result {
                            assert!(
                                error.to_string().contains("enclave is not initialized")
                                    || error.to_string().contains("challenge mismatch"),
                                "unexpected candidate retry error: {error}"
                            );
                        }
                        accepted += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => panic!("candidate retry listener failed: {error}"),
                }
            }
            accepted
        });
        let retry = prepare_validator_enclave_replacement_candidate(
            &candidate_endpoint,
            &node_data_dir,
            identity,
            &sign,
        );
        match retry {
            Ok(candidate) => drop(candidate),
            Err(error) => {
                let accepted = retry_server.join().unwrap();
                panic!("candidate retry failed after {accepted} connections: {error}");
            }
        }
        assert_eq!(retry_server.join().unwrap(), 3);
    }

    #[test]
    fn full_node_host_state_uses_exact_reth_p2p_identity_and_reconnects() {
        use alloy_primitives::U256;
        use k256::ecdsa::signature::hazmat::PrehashSigner as _;
        use outbe_primitives::tee_attestation_v1::{
            EnclaveInitializationManifestV1, EnclaveProfile, NodeIdV1,
        };
        use outbe_tee::{
            connect_or_initialize_full_node_enclave,
            prepare_full_node_enclave_replacement_candidate, FullNodeNodeHostIdentityV1,
        };

        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("enclave.sock");
        let endpoint = socket.to_str().unwrap().to_string();
        let candidate_socket = root.path().join("candidate-enclave.sock");
        let candidate_endpoint = candidate_socket.to_str().unwrap().to_string();
        let chain_id = 17_u64;
        let chain_id_word = U256::from(chain_id).to_be_bytes();
        let genesis_hash = B256::repeat_byte(0x12);
        let boot = Arc::new(EnclaveBootConfig::new(
            chain_id_word,
            root.path().join("enclave-state"),
            0,
        ));
        std::fs::create_dir(&boot.tee_dir).unwrap();
        let keys = Arc::new(EnclaveKeys::new([0x75; 32], Some([0x75; 32])).unwrap());
        let initialization =
            Arc::new(InitializationState::production(boot.clone(), &keys).unwrap());
        let candidate_boot = Arc::new(EnclaveBootConfig::new(
            chain_id_word,
            root.path().join("candidate-enclave-state"),
            0,
        ));
        std::fs::create_dir(&candidate_boot.tee_dir).unwrap();
        let candidate_keys = Arc::new(EnclaveKeys::new([0x78; 32], Some([0x78; 32])).unwrap());
        let candidate_initialization = Arc::new(
            InitializationState::production(candidate_boot.clone(), &candidate_keys).unwrap(),
        );
        let offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let listener = UnixListener::bind(&socket).unwrap();
        let server_keys = keys.clone();
        let server_initialization = initialization.clone();
        let server = std::thread::spawn(move || {
            for _ in 0..3 {
                let (stream, _) = listener.accept().unwrap();
                serve_connection_with(
                    stream,
                    &server_keys,
                    &offer_key,
                    Some(&boot),
                    &server_initialization,
                )
                .unwrap();
            }
        });
        let candidate_offer_key: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let candidate_listener = UnixListener::bind(&candidate_socket).unwrap();
        let candidate_server_keys = candidate_keys.clone();
        let candidate_server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (stream, _) = candidate_listener.accept().unwrap();
                serve_connection_with(
                    stream,
                    &candidate_server_keys,
                    &candidate_offer_key,
                    Some(&candidate_boot),
                    &candidate_initialization,
                )
                .unwrap();
            }
        });

        let signing = k256::ecdsa::SigningKey::from_bytes((&[0x62; 32]).into()).unwrap();
        let reth_p2p_public: [u8; 33] = signing
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap();
        let identity = FullNodeNodeHostIdentityV1 {
            chain_id,
            genesis_hash,
            reth_p2p_public,
        };
        let sign = |hash: B256| {
            let (signature, recovery): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) = signing
                .sign_prehash(hash.as_slice())
                .map_err(|error| error.to_string())?;
            let mut bytes = [0_u8; 65];
            bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
            bytes[64] = recovery.to_byte();
            Ok(bytes)
        };
        let node_data_dir = root.path().join("node-data");
        std::fs::create_dir(&node_data_dir).unwrap();

        let mut initialized =
            connect_or_initialize_full_node_enclave(&endpoint, &node_data_dir, identity, &sign)
                .unwrap();
        assert!(matches!(
            initialized.request(&EnclaveRequest::GetPublicKeys).unwrap(),
            EnclaveResponse::PublicKeys { .. }
        ));
        drop(initialized);

        let mut reconnected =
            connect_or_initialize_full_node_enclave(&endpoint, &node_data_dir, identity, &sign)
                .unwrap();
        assert!(matches!(
            reconnected.request(&EnclaveRequest::GetPublicKeys).unwrap(),
            EnclaveResponse::PublicKeys { .. }
        ));
        drop(reconnected);
        let candidate = prepare_full_node_enclave_replacement_candidate(
            &candidate_endpoint,
            &node_data_dir,
            identity,
            &sign,
        )
        .unwrap();
        assert_eq!(
            candidate.manifest().enclave_profile,
            EnclaveProfile::FullNode
        );
        assert_eq!(
            candidate.manifest().node_id,
            NodeIdV1::FullNode { reth_p2p_public }
        );
        let candidate_manifest = candidate.manifest().clone();
        drop(candidate);
        server.join().unwrap();
        candidate_server.join().unwrap();

        let manifest_bytes = std::fs::read(
            node_data_dir
                .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
                .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1),
        )
        .unwrap();
        let manifest = EnclaveInitializationManifestV1::decode_canonical(&manifest_bytes).unwrap();
        assert_eq!(manifest.enclave_profile, EnclaveProfile::FullNode);
        assert_eq!(manifest.node_id, NodeIdV1::FullNode { reth_p2p_public });
        assert_ne!(
            manifest.enclave_id().unwrap(),
            candidate_manifest.enclave_id().unwrap()
        );
        assert_eq!(
            manifest.node_host_authorization_hash().unwrap(),
            candidate_manifest.node_host_authorization_hash().unwrap()
        );
    }
}
