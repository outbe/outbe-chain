//! Process-global enclave session: the reconnect-capable wrapper around the
//! pinned [`RuntimeEnclaveClient`].
//!
//! The node installs one session at startup. When a request fails with a
//! connection-class fault ([`TransportError::is_connection_fault`]), the session
//! does ONE bounded reconnect with identity re-validation. Then it re-sends the
//! request once, but only when [`EnclaveRequest::is_idempotent`] allows it. A
//! reconnected peer must present the byte-identical identity that the session
//! pinned at install. Any mismatch permanently revokes the session (fail-closed).
//! Without this revocation, every later attestation-tag check would validate
//! against the impostor's own key.
//!
//! Locking stays with the caller ([`crate::client_global::try_with_enclave`]):
//! the session is single-threaded under that mutex, so a reconnect can never
//! interleave with another caller's request.

use std::path::PathBuf;
use std::time::Instant;

use alloy_primitives::B256;

use crate::client::NodeHostNoiseKey;
use crate::client::{AuthorizedEnclaveClient, EnclaveClient, GeneratedDcapQuoteV1};
use crate::client_global::RuntimeEnclaveClient;
use crate::dcap_protocol::{
    DcapOnboardingArtifactV1, DcapOnboardingContextV1, DcapOnboardingVerificationResultV1,
    DcapVerificationOutcomeV1, RegistrationVerificationRequest,
};
use crate::errors::TransportError;
use crate::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_primitives::tee_attestation_v1::{EnclaveInitializationManifestV1, RegistrationIntentV1};

/// Identity pinned from a development client's structurally validated quote at
/// install time. A reconnected peer must match every field byte-for-byte.
#[derive(Clone, Debug)]
struct DevPinnedIdentity {
    mrenclave: B256,
    mrsigner: B256,
    isv_svn: u16,
    attestation_pub: [u8; 32],
    attestation_label: String,
    noise_static_pub: [u8; 32],
}

impl DevPinnedIdentity {
    fn pin(client: &EnclaveClient) -> Self {
        let (mrenclave, mrsigner, isv_svn) = client.measurements();
        Self {
            mrenclave,
            mrsigner,
            isv_svn,
            attestation_pub: client.attestation_pub(),
            attestation_label: client.attestation_label().to_string(),
            noise_static_pub: client.noise_static_pub(),
        }
    }

    /// Exact-equality identity check (including the attestation label, so a
    /// `dcap` peer can never be replaced by a `none` peer).
    fn matches(&self, client: &EnclaveClient) -> Result<(), String> {
        let (mrenclave, mrsigner, isv_svn) = client.measurements();
        if (mrenclave, mrsigner, isv_svn) != (self.mrenclave, self.mrsigner, self.isv_svn) {
            return Err("enclave measurements changed across reconnect".into());
        }
        if client.attestation_pub() != self.attestation_pub {
            return Err("enclave attestation key changed across reconnect".into());
        }
        if client.noise_static_pub() != self.noise_static_pub {
            return Err("enclave Noise static key changed across reconnect".into());
        }
        if client.attestation_label() != self.attestation_label {
            return Err("enclave attestation environment changed across reconnect".into());
        }
        Ok(())
    }
}

#[derive(Clone)]
enum SessionContext {
    Development {
        endpoint: String,
        pinned: DevPinnedIdentity,
    },
    Production {
        endpoint: String,
        /// Retained for operator diagnostics. Reconnect deliberately does NOT
        /// re-read NodeHost state (see `committed_node_host_session_material`).
        #[allow(dead_code)]
        node_data_dir: PathBuf,
        manifest: EnclaveInitializationManifestV1,
        node_host: NodeHostNoiseKey,
    },
}

/// The process-global enclave session (see module docs).
pub struct EnclaveSession {
    /// `None` = disconnected. The next request forces a clean reconnect.
    client: Option<RuntimeEnclaveClient>,
    context: SessionContext,
    /// The resident permanent offer public key observed most recently. `None`
    /// until the enclave reports `offer_key_ready` (legal pre-DKG state).
    pinned_offer_public: Option<B256>,
    /// Every successful reconnect bumps this value. Generation 1 is the installed
    /// connection.
    generation: u64,
    /// A permanently revoked session refuses every request (fail-closed).
    revoked: Option<&'static str>,
    /// First-poison latch: recovery from a poisoned mutex drops the client
    /// exactly once (the interrupted request left the Noise state unknown).
    poison_recovered: bool,
}

impl EnclaveSession {
    /// Open a separate connection on first use, retaining the installed identity
    /// and credentials. Never clone live Noise state or reconnect under the
    /// execution session's mutex.
    pub(crate) fn fork_connection(&self) -> Self {
        Self {
            client: None,
            context: self.context.clone(),
            pinned_offer_public: self.pinned_offer_public,
            generation: 0,
            revoked: self.revoked,
            poison_recovered: false,
        }
    }

    /// Wrap an installed development client. Probes `GetPublicKeys` once to pin
    /// the resident offer key state.
    pub fn development(client: EnclaveClient, endpoint: String) -> Result<Self, TransportError> {
        let pinned = DevPinnedIdentity::pin(&client);
        let mut client = RuntimeEnclaveClient::Development(Box::new(client));
        let pinned_offer_public = probe_offer_key(&mut client)?;
        Ok(Self {
            client: Some(client),
            context: SessionContext::Development { endpoint, pinned },
            pinned_offer_public,
            generation: 1,
            revoked: None,
            poison_recovered: false,
        })
    }

    /// Wrap an installed production client together with the committed session
    /// material loaded once at startup (`committed_node_host_session_material`).
    pub fn production(
        client: AuthorizedEnclaveClient,
        endpoint: String,
        node_data_dir: PathBuf,
        manifest: EnclaveInitializationManifestV1,
        node_host: NodeHostNoiseKey,
    ) -> Result<Self, TransportError> {
        let mut client = RuntimeEnclaveClient::Production(client);
        let pinned_offer_public = probe_offer_key(&mut client)?;
        Ok(Self {
            client: Some(client),
            context: SessionContext::Production {
                endpoint,
                node_data_dir,
                manifest,
                node_host,
            },
            pinned_offer_public,
            generation: 1,
            revoked: None,
            poison_recovered: false,
        })
    }

    /// The pinned enclave attestation key. The session takes it from install-time
    /// identity (dev: quote pin, production: committed manifest), NOT from the live
    /// connection. Post-reconnect attestation-tag checks therefore verify against
    /// the identity the operator installed, not whatever peer answered last.
    pub fn attestation_pub(&self) -> [u8; 32] {
        match &self.context {
            SessionContext::Development { pinned, .. } => pinned.attestation_pub,
            SessionContext::Production { manifest, .. } => manifest.attestation_ed25519,
        }
    }

    /// Current session generation (1 = the installed connection).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// True once poison recovery ran (see [`Self::recover_from_poison`]).
    pub fn poison_recovered(&self) -> bool {
        self.poison_recovered
    }

    /// Recover after a caller panicked while holding the session mutex. The
    /// interrupted request left the Noise cipher state unknown. Thus, drop the
    /// client and force a clean reconnect on next use. Latched: later lock
    /// acquisitions must not drop a healthy reconnected client.
    pub fn recover_from_poison(&mut self) {
        if !self.poison_recovered {
            self.poison_recovered = true;
            self.client = None;
        }
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
        let _call_context =
            crate::call_context::ContextScope::enter(crate::call_context::resolve()?);
        ensure_single_frame_request(req)?;
        if let Some(reason) = self.revoked {
            return Err(TransportError::SessionRevoked(reason));
        }
        if self.client.is_none() {
            self.reconnect()?;
        }
        let first_err = match self.request_connected(req) {
            Ok(response) => return Ok(response),
            Err(error) if !error.is_connection_fault() => return Err(error),
            Err(error) => error,
        };
        self.client = None;
        self.reconnect()?;
        if !req.is_idempotent() {
            // The reconnect healed the session for the next caller. But this
            // request may already have executed inside the enclave. Surface the
            // original fault instead of risking a double apply.
            return Err(first_err);
        }
        self.request_connected(req)
    }

    fn request_connected(
        &mut self,
        req: &EnclaveRequest,
    ) -> Result<EnclaveResponse, TransportError> {
        match self.client.as_mut() {
            Some(client) => client.request(req),
            // Reconnect either installs a client or errors. Keep the impossible
            // missing-client path structured instead of panicking.
            None => Err(TransportError::Unavailable(
                "enclave session has no connection after reconnect".into(),
            )),
        }
    }

    /// Whole-operation wrapper for the production-only DCAP flows. It runs the
    /// operation once. On a connection fault, it reconnects once and reruns the
    /// WHOLE operation. A multi-frame upload restarts from `Begin` on the fresh
    /// session, because the enclave keys upload state per connection.
    fn with_production_retry<T>(
        &mut self,
        op_label: &'static str,
        dev_rejection: impl Fn() -> TransportError,
        op: impl Fn(&mut AuthorizedEnclaveClient) -> Result<T, TransportError>,
    ) -> Result<T, TransportError> {
        let _call_context =
            crate::call_context::ContextScope::enter(crate::call_context::resolve()?);
        if let Some(reason) = self.revoked {
            return Err(TransportError::SessionRevoked(reason));
        }
        if self.client.is_none() {
            self.reconnect()?;
        }
        let started = Instant::now();
        let first = match self.client.as_mut() {
            Some(RuntimeEnclaveClient::Production(client)) => op(client),
            Some(RuntimeEnclaveClient::Development(_)) => return Err(dev_rejection()),
            None => {
                return Err(TransportError::EnclaveError(
                    "enclave session has no connection after reconnect".into(),
                ));
            }
        };
        let result = match first {
            Err(error) if error.is_connection_fault() => {
                self.client = None;
                self.reconnect()?;
                match self.client.as_mut() {
                    Some(RuntimeEnclaveClient::Production(client)) => op(client),
                    _ => Err(error),
                }
            }
            other => other,
        };
        crate::metrics::record_request_duration(op_label, started.elapsed());
        if let Err(error) = &result {
            crate::metrics::record_request_error(op_label, error.metric_class());
        }
        result
    }

    pub(crate) fn export_upgrade_key_v1(
        &mut self,
        proof: &crate::upgrade_transfer::UpgradeKeyProofV1,
        artifact: &[u8],
    ) -> Result<EnclaveResponse, TransportError> {
        self.with_production_retry(
            "export_upgrade_key_v1",
            || {
                TransportError::EnclaveError(
                    "upgrade export requires an authenticated owner session".into(),
                )
            },
            |client| client.transfer_upgrade_key_v1(proof, artifact, true),
        )
    }

    /// Production-only: verify DCAP evidence (whole-operation retry). The
    /// development rejection mirrors the pre-session behavior verbatim.
    pub fn verify_dcap_evidence_v1(
        &mut self,
        evidence: &[u8],
        policy: &[u8],
        block_timestamp: u64,
    ) -> Result<DcapVerificationOutcomeV1, TransportError> {
        self.with_production_retry(
            "verify_dcap_evidence_v1",
            || {
                TransportError::DcapVerification(
                    "development enclave client cannot verify consensus DCAP evidence".into(),
                )
            },
            |client| client.verify_dcap_evidence_v1(evidence, policy, block_timestamp),
        )
    }

    /// Production-only: `RegisterEnclave` verify-and-seal (whole-operation retry).
    pub fn verify_dcap_registration_and_seal_v1(
        &mut self,
        request: RegistrationVerificationRequest<'_>,
    ) -> Result<DcapOnboardingVerificationResultV1, TransportError> {
        self.with_production_retry(
            "verify_dcap_registration_and_seal_v1",
            || {
                TransportError::DcapVerification(
                    "development enclave client cannot issue DCAP onboarding artifacts".into(),
                )
            },
            |client| client.verify_dcap_registration_and_seal_v1(request),
        )
    }

    pub fn prepare_gramine_direct_dev_onboarding_artifact_v1(
        &mut self,
        context: DcapOnboardingContextV1,
    ) -> Result<DcapOnboardingArtifactV1, TransportError> {
        self.with_production_retry(
            "prepare_gramine_direct_dev_onboarding_artifact_v1",
            || {
                TransportError::EnclaveError(
                    "development enclave client cannot issue DirectDev onboarding artifacts".into(),
                )
            },
            |client| client.prepare_gramine_direct_dev_onboarding_artifact_v1(context),
        )
    }

    /// Production-only: intent-bound quote generation (whole-operation retry).
    pub fn generate_dcap_quote(
        &mut self,
        intent: &RegistrationIntentV1,
    ) -> Result<GeneratedDcapQuoteV1, TransportError> {
        self.with_production_retry(
            "generate_dcap_quote",
            || {
                TransportError::Attestation(
                    "development enclave client cannot generate production renewal quotes".into(),
                )
            },
            |client| client.generate_dcap_quote(intent),
        )
    }

    /// One bounded reconnect: fresh transport, identity re-validation, offer-key
    /// pin check. Success installs the client and bumps the generation. An
    /// identity or offer-key mismatch revokes the session permanently.
    fn reconnect(&mut self) -> Result<(), TransportError> {
        if let Some(reason) = self.revoked {
            return Err(TransportError::SessionRevoked(reason));
        }
        // Drop any broken half-session first so its socket closes.
        self.client = None;
        let mut client = self.connect_pinned_client()?;
        // Offer-key pin: the resident permanent key must never change or
        // disappear across a reconnect. Appearing (pre-DKG -> post-DKG) is the
        // one legal upgrade.
        let fresh = match probe_offer_key(&mut client) {
            Ok(fresh) => fresh,
            Err(error) => {
                crate::metrics::record_reconnect("connect_failed");
                return Err(error);
            }
        };
        self.accept_offer_key(fresh)?;
        self.client = Some(client);
        self.generation += 1;
        crate::metrics::record_reconnect("ok");
        crate::metrics::record_session_generation(self.generation);
        Ok(())
    }

    /// Create a new transport and verify the pinned install-time identity.
    fn connect_pinned_client(&mut self) -> Result<RuntimeEnclaveClient, TransportError> {
        match &self.context {
            SessionContext::Development { endpoint, pinned } => {
                let client = match EnclaveClient::connect_endpoint(endpoint) {
                    Ok(client) => client,
                    Err(error) => {
                        crate::metrics::record_reconnect("connect_failed");
                        return Err(error);
                    }
                };
                if let Err(mismatch) = pinned.matches(&client) {
                    crate::metrics::record_reconnect("identity_mismatch");
                    return Err(self.revoke("enclave identity changed across reconnect", mismatch));
                }
                Ok(RuntimeEnclaveClient::Development(Box::new(client)))
            }
            SessionContext::Production {
                endpoint,
                manifest,
                node_host,
                ..
            } => {
                // Noise-IK against `manifest.noise_responder_x25519` IS the
                // identity check: an impostor can never complete the handshake.
                // Handshake failure therefore stays retryable (connect_failed),
                // never a revocation. A legitimately replaced enclave keeps
                // failing here until the operator restarts the node. The
                // replacement flow already requires that restart.
                match AuthorizedEnclaveClient::connect_endpoint(endpoint, manifest, node_host) {
                    Ok(client) => Ok(RuntimeEnclaveClient::Production(client)),
                    Err(error) => {
                        crate::metrics::record_reconnect("connect_failed");
                        Err(error)
                    }
                }
            }
        }
    }

    /// Accept the one legal key transition (absent to present) and reject a
    /// changed or missing permanent key with permanent session revocation.
    fn accept_offer_key(&mut self, fresh: Option<B256>) -> Result<(), TransportError> {
        match (self.pinned_offer_public, fresh) {
            (Some(pinned), Some(observed)) if pinned == observed => Ok(()),
            (None, observed) => {
                self.pinned_offer_public = observed;
                Ok(())
            }
            (Some(pinned), observed) => {
                crate::metrics::record_reconnect("identity_mismatch");
                Err(self.revoke(
                    "resident offer key changed across reconnect",
                    format!("pinned {pinned}, reconnected enclave reports {observed:?}"),
                ))
            }
        }
    }

    fn revoke(&mut self, reason: &'static str, detail: String) -> TransportError {
        self.revoked = Some(reason);
        self.client = None;
        TransportError::IdentityMismatch(format!("{reason}: {detail}"))
    }
}

fn ensure_single_frame_request(req: &EnclaveRequest) -> Result<(), TransportError> {
    if matches!(
        req,
        EnclaveRequest::BeginDcapVerificationV1 { .. }
            | EnclaveRequest::BeginDcapOnboardingVerificationV1 { .. }
            | EnclaveRequest::DcapVerificationChunkV1 { .. }
            | EnclaveRequest::FinishDcapVerificationV1 { .. }
            | EnclaveRequest::BeginDcapOnboardingArtifactIngestV1 { .. }
            | EnclaveRequest::BeginUpgradeKeyTransferV1 { .. }
            | EnclaveRequest::DcapOnboardingArtifactChunkV1 { .. }
            | EnclaveRequest::CommitDcapOnboardingArtifactRecordV1 { .. }
            | EnclaveRequest::FinishDcapOnboardingArtifactIngestV1 { .. }
    ) {
        return Err(TransportError::DcapVerification(
            "multi-frame enclave requests must use a whole-operation session API".into(),
        ));
    }
    Ok(())
}

/// `GetPublicKeys` probe shared by install and reconnect. `Ok(None)` is the
/// legal keyless (pre-DKG) state. A ready-but-zero key is an enclave fault.
fn probe_offer_key(client: &mut RuntimeEnclaveClient) -> Result<Option<B256>, TransportError> {
    match client.request(&EnclaveRequest::GetPublicKeys)? {
        EnclaveResponse::PublicKeys {
            offer_key_ready,
            recipient_x25519_pub,
            ..
        } => {
            if !offer_key_ready {
                return Ok(None);
            }
            let public = B256::from(recipient_x25519_pub);
            if public.is_zero() {
                return Err(TransportError::EnclaveError(
                    "local enclave reports a ready but zero permanent offer key".into(),
                ));
            }
            Ok(Some(public))
        }
        EnclaveResponse::Error { message } => Err(TransportError::EnclaveError(message)),
        _ => Err(TransportError::UnexpectedResponse),
    }
}

#[cfg(test)]
mod tests;
