//! Write-once enclave initialization and NodeHost command authorization.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use alloy_primitives::B256;

use outbe_primitives::tee_attestation_v1::{
    AttestationMode, EnclaveInitializationManifestV1, RegistrationIntentV1,
};
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use rand_core::RngCore as _;

use crate::keys::EnclaveKeys;
use crate::seal::{
    seal_tribute_offer_and_group_sig, unseal_tribute_offer_and_group_sig, EnclaveBootConfig,
    SealHeader, SEAL_FORMAT,
};

const MAX_PENDING_REMOTE_SESSIONS_V1: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitializationMode {
    Production,
    Development,
}

#[derive(Clone, Debug)]
pub struct PendingInitialization {
    manifest: EnclaveInitializationManifestV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingRemoteSessionV1 {
    initiator_static_x25519: [u8; 32],
    deadline: u64,
}

impl PendingRemoteSessionV1 {
    pub const fn initiator_static_x25519(self) -> [u8; 32] {
        self.initiator_static_x25519
    }

    pub const fn deadline(self) -> u64 {
        self.deadline
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAuthorityV1 {
    LocalNodeHost,
    RemoteActiveNode { deadline: u64 },
}

impl SessionAuthorityV1 {
    pub fn ensure_live(self) -> Result<(), &'static str> {
        self.ensure_live_at(unix_time_seconds()?)
    }

    fn ensure_live_at(self, now: u64) -> Result<(), &'static str> {
        match self {
            Self::LocalNodeHost => Ok(()),
            Self::RemoteActiveNode { deadline } if now < deadline => Ok(()),
            Self::RemoteActiveNode { .. } => Err("remote session lease expired"),
        }
    }
}

impl PendingInitialization {
    pub fn node_host_noise_x25519(&self) -> [u8; 32] {
        self.manifest.node_host_noise_x25519
    }
}

#[derive(Debug)]
struct StoredInitialization {
    manifest: EnclaveInitializationManifestV1,
    loaded_from_seal: bool,
}

/// Shared process-wide initialization state. Production requires a boot config
/// because accepting an authorization that cannot survive restart would silently
/// rotate the enclave/NodeHost trust boundary.
pub struct InitializationState {
    mode: InitializationMode,
    gramine_direct_dev_evidence_allowed: bool,
    challenge: [u8; 32],
    boot: Option<Arc<EnclaveBootConfig>>,
    stored: Mutex<Option<StoredInitialization>>,
    remote_sessions: Mutex<BTreeMap<B256, PendingRemoteSessionV1>>,
}

impl InitializationState {
    pub fn production(boot: Arc<EnclaveBootConfig>, keys: &EnclaveKeys) -> Result<Self, String> {
        let mut challenge = [0u8; 32];
        rand_core::OsRng.fill_bytes(&mut challenge);
        if crate::transport::sealing_key().is_none() {
            return Err("production initialization requires an SGX sealing key".to_string());
        }
        Self::production_with_challenge_and_attestation_inner(
            boot,
            keys,
            challenge,
            crate::gramine::attestation_type(),
        )
    }

    #[cfg(test)]
    fn production_with_challenge(
        boot: Arc<EnclaveBootConfig>,
        keys: &EnclaveKeys,
        challenge: [u8; 32],
    ) -> Result<Self, String> {
        Self::production_with_challenge_and_attestation_inner(
            boot,
            keys,
            challenge,
            crate::gramine::attestation_type(),
        )
    }

    fn production_with_challenge_and_attestation_inner(
        boot: Arc<EnclaveBootConfig>,
        keys: &EnclaveKeys,
        challenge: [u8; 32],
        attestation: crate::gramine::AttestationType,
    ) -> Result<Self, String> {
        if challenge == [0; 32] {
            return Err("initialization challenge must be nonzero".to_string());
        }
        let restored = restore_manifest(&boot, keys)?;
        Ok(Self {
            mode: InitializationMode::Production,
            gramine_direct_dev_evidence_allowed: matches!(
                attestation,
                crate::gramine::AttestationType::SgxNoAttest
            ),
            challenge,
            boot: Some(boot),
            stored: Mutex::new(restored.map(|manifest| StoredInitialization {
                manifest,
                loaded_from_seal: true,
            })),
            remote_sessions: Mutex::new(BTreeMap::new()),
        })
    }

    #[cfg(test)]
    pub(crate) fn production_with_challenge_and_attestation(
        boot: Arc<EnclaveBootConfig>,
        keys: &EnclaveKeys,
        challenge: [u8; 32],
        attestation: crate::gramine::AttestationType,
    ) -> Result<Self, String> {
        Self::production_with_challenge_and_attestation_inner(boot, keys, challenge, attestation)
    }

    /// Separate dev/mock behavior. It never creates a production authorization
    /// claim and is selected only by the required-feature mock binary or tests.
    pub fn development() -> Self {
        Self {
            mode: InitializationMode::Development,
            gramine_direct_dev_evidence_allowed: true,
            challenge: [0xDD; 32],
            boot: None,
            stored: Mutex::new(None),
            remote_sessions: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn mode(&self) -> InitializationMode {
        self.mode
    }

    pub(crate) fn gramine_direct_dev_evidence_allowed(&self) -> bool {
        self.gramine_direct_dev_evidence_allowed
    }

    pub fn challenge_response(&self, keys: &EnclaveKeys) -> Result<EnclaveResponse, String> {
        if self.mode != InitializationMode::Production {
            return Err("initialization challenge is unavailable in development mode".to_string());
        }
        if self.manifest()?.is_some() {
            return Err("enclave is already initialized".to_string());
        }
        Ok(EnclaveResponse::InitializationChallenge {
            challenge: self.challenge,
            recipient_x25519_pub: keys.tribute_offer_public(),
            attestation_pub: keys.attestation_pub(),
            noise_static_pub: keys.noise_public(),
        })
    }

    /// Validate the signed manifest without mutating state. The connection must
    /// still prove the embedded NodeHost static key in Noise message 1 before
    /// `commit` can seal it.
    pub fn prepare(
        &self,
        manifest_bytes: &[u8],
        node_signature: &[u8],
        keys: &EnclaveKeys,
    ) -> Result<PendingInitialization, String> {
        if self.mode != InitializationMode::Production {
            return Err("production initialization is unavailable in development mode".to_string());
        }
        if self.manifest()?.is_some() {
            return Err("enclave is already initialized".to_string());
        }
        let manifest = EnclaveInitializationManifestV1::decode_canonical(manifest_bytes)
            .map_err(|error| format!("initialization manifest is not canonical: {error}"))?;
        let signature: [u8; 65] = node_signature
            .try_into()
            .map_err(|_| "initialization node signature must be 65 bytes".to_string())?;
        self.validate_manifest(&manifest, keys)?;
        if !manifest.verify_node_signature(&signature) {
            return Err("initialization node signature is invalid".to_string());
        }
        Ok(PendingInitialization { manifest })
    }

    /// Persist before publishing the authorization in memory. A replay or a
    /// conflicting second manifest always rejects, including an exact replay.
    pub fn commit(&self, pending: PendingInitialization, keys: &EnclaveKeys) -> Result<(), String> {
        let mut stored = self
            .stored
            .lock()
            .map_err(|_| "initialization state lock is poisoned".to_string())?;
        if stored.is_some() {
            return Err("enclave is already initialized".to_string());
        }
        self.validate_manifest(&pending.manifest, keys)?;
        let boot = self
            .boot
            .as_deref()
            .ok_or_else(|| "production initialization requires sealed storage".to_string())?;
        persist_manifest(boot, &pending.manifest)?;
        *stored = Some(StoredInitialization {
            manifest: pending.manifest,
            loaded_from_seal: false,
        });
        Ok(())
    }

    pub fn initialized_response(&self) -> Result<EnclaveResponse, String> {
        let stored = self
            .stored
            .lock()
            .map_err(|_| "initialization state lock is poisoned".to_string())?;
        let stored = stored
            .as_ref()
            .ok_or_else(|| "enclave is not initialized".to_string())?;
        Ok(EnclaveResponse::Initialized {
            enclave_id: stored
                .manifest
                .enclave_id()
                .map_err(|error| error.to_string())?,
            node_host_authorization_hash: stored
                .manifest
                .node_host_authorization_hash()
                .map_err(|error| error.to_string())?,
            sealed_loaded: stored.loaded_from_seal,
        })
    }

    pub fn manifest(&self) -> Result<Option<EnclaveInitializationManifestV1>, String> {
        self.stored
            .lock()
            .map(|stored| stored.as_ref().map(|value| value.manifest.clone()))
            .map_err(|_| "initialization state lock is poisoned".to_string())
    }

    pub fn expected_node_host(&self) -> Result<[u8; 32], String> {
        self.manifest()?
            .map(|manifest| manifest.node_host_noise_x25519)
            .ok_or_else(|| "enclave is not initialized".to_string())
    }

    pub fn quote_report_data(&self, intent_bytes: &[u8]) -> Result<[u8; 64], String> {
        let manifest = self
            .manifest()?
            .ok_or_else(|| "enclave is not initialized".to_string())?;
        let intent = RegistrationIntentV1::decode_canonical(intent_bytes)
            .map_err(|error| format!("registration intent is not canonical: {error}"))?;
        if intent.attestation_mode != AttestationMode::DcapRequired {
            return Err(
                "production quote generation requires DcapRequired attestation mode".to_string(),
            );
        }
        manifest
            .validate_intent_binding(&intent)
            .map_err(|error| error.to_string())?;
        intent.report_data().map_err(|error| error.to_string())
    }

    pub fn authorize_command(
        &self,
        request: &EnclaveRequest,
        offer_key_ready: bool,
        authority: SessionAuthorityV1,
    ) -> Result<(), &'static str> {
        if self.mode == InitializationMode::Development {
            return Ok(());
        }
        authority.ensure_live()?;
        if matches!(authority, SessionAuthorityV1::RemoteActiveNode { .. }) {
            return matches!(request, EnclaveRequest::GetPublicKeys)
                .then_some(())
                .ok_or("remote session command denied by enclave capability matrix");
        }
        self.manifest()
            .map_err(|_| "initialization state unavailable")?
            .ok_or("enclave is not initialized")?;
        command_allowed_for_environment(
            command_class(request),
            offer_key_ready,
            self.gramine_direct_dev_evidence_allowed,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn authorize_remote_session(
        &self,
        ticket_id: B256,
        initiator_static_x25519: [u8; 32],
        responder_static_x25519: [u8; 32],
        deadline: u64,
        finalized_block_hash: B256,
        keys: &EnclaveKeys,
    ) -> Result<(), String> {
        if self.mode != InitializationMode::Production || self.manifest()?.is_none() {
            return Err(
                "remote session authorization requires initialized production state".into(),
            );
        }
        if ticket_id.is_zero()
            || initiator_static_x25519 == [0; 32]
            || responder_static_x25519 == [0; 32]
            || finalized_block_hash.is_zero()
        {
            return Err("remote session authorization is malformed".into());
        }
        if responder_static_x25519 != keys.noise_public() {
            return Err("remote session targets another Noise responder".into());
        }
        let now = unix_time_seconds().map_err(str::to_owned)?;
        if deadline <= now {
            return Err("remote session authorization is expired".into());
        }
        let mut sessions = self
            .remote_sessions
            .lock()
            .map_err(|_| "remote session authorization lock is poisoned".to_string())?;
        sessions.retain(|_, session| session.deadline > now);
        if sessions.len() >= MAX_PENDING_REMOTE_SESSIONS_V1 {
            return Err("pending remote session capacity reached".into());
        }
        if sessions.contains_key(&ticket_id) {
            return Err("remote session ticket already exists".into());
        }
        sessions.insert(
            ticket_id,
            PendingRemoteSessionV1 {
                initiator_static_x25519,
                deadline,
            },
        );
        Ok(())
    }

    pub fn take_remote_session(&self, ticket_id: B256) -> Result<PendingRemoteSessionV1, String> {
        if self.mode != InitializationMode::Production || ticket_id.is_zero() {
            return Err("remote session ticket is invalid".into());
        }
        let now = unix_time_seconds().map_err(str::to_owned)?;
        let mut sessions = self
            .remote_sessions
            .lock()
            .map_err(|_| "remote session authorization lock is poisoned".to_string())?;
        sessions.retain(|_, session| session.deadline > now);
        sessions
            .remove(&ticket_id)
            .ok_or_else(|| "remote session ticket is unavailable, expired, or already used".into())
    }

    fn validate_manifest(
        &self,
        manifest: &EnclaveInitializationManifestV1,
        keys: &EnclaveKeys,
    ) -> Result<(), String> {
        let boot = self
            .boot
            .as_deref()
            .ok_or_else(|| "production initialization requires sealed storage".to_string())?;
        if manifest.chain_id != boot.chain_id.0 {
            return Err("initialization chain id does not match enclave boot config".to_string());
        }
        if manifest.initialization_challenge != self.challenge {
            return Err("initialization challenge mismatch".to_string());
        }
        if manifest.recipient_x25519 != keys.tribute_offer_public()
            || manifest.attestation_ed25519 != keys.attestation_pub()
            || manifest.noise_responder_x25519 != keys.noise_public()
        {
            return Err(
                "initialization manifest does not bind this enclave's persistent keys".to_string(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandClass {
    Never,
    Initialized,
    FoundingKeyless,
    KeylessOnboardingArtifact,
    Ready,
    DevSourceSeal,
    DevRecipientIngest,
}

fn command_class(request: &EnclaveRequest) -> CommandClass {
    match request {
        EnclaveRequest::GetPublicKeys
        | EnclaveRequest::AuthorizeRemoteSessionV1 { .. }
        | EnclaveRequest::GenerateDcapQuote { .. }
        | EnclaveRequest::SignRegistrationIntentDevV1 { .. }
        | EnclaveRequest::BeginDcapVerificationV1 { .. }
        | EnclaveRequest::BeginDcapOnboardingVerificationV1 { .. }
        | EnclaveRequest::DcapVerificationChunkV1 { .. }
        | EnclaveRequest::FinishDcapVerificationV1 { .. }
        | EnclaveRequest::Health => CommandClass::Initialized,
        EnclaveRequest::DkgOpen { .. }
        | EnclaveRequest::DkgStartDealer { .. }
        | EnclaveRequest::DkgPlayerIngest { .. }
        | EnclaveRequest::DkgDealerReceiveAck { .. }
        | EnclaveRequest::DkgDealerFinalize { .. }
        | EnclaveRequest::DkgPlayerFinalize { .. }
        | EnclaveRequest::DkgTributeOfferPartial { .. }
        | EnclaveRequest::DkgFinalizeTributeOffer { .. } => CommandClass::FoundingKeyless,
        EnclaveRequest::IngestDcapOnboardingArtifactV1 { .. } => {
            CommandClass::KeylessOnboardingArtifact
        }
        EnclaveRequest::ProcessTributeOfferBatch { .. }
        | EnclaveRequest::ApplyGratisOp { .. }
        | EnclaveRequest::ApplyPromisOp { .. }
        | EnclaveRequest::ApplyFidelityCohortOp { .. }
        | EnclaveRequest::SnapshotFidelityLeagues { .. }
        | EnclaveRequest::QueryFidelityIndex { .. }
        | EnclaveRequest::DeriveAccountKeys { .. } => CommandClass::Ready,
        EnclaveRequest::SealOfferKeyForRegistry { .. } => CommandClass::DevSourceSeal,
        EnclaveRequest::IngestSealedOfferKeyForRegistry { .. } => CommandClass::DevRecipientIngest,
        EnclaveRequest::GetQuote { .. }
        | EnclaveRequest::GetInitializationChallenge
        | EnclaveRequest::Initialize { .. }
        | EnclaveRequest::OpenSession
        | EnclaveRequest::OpenRemoteSessionV1 { .. }
        | EnclaveRequest::SessionHandshake { .. } => CommandClass::Never,
    }
}

/// Health-counter bucket for `request` - the telemetry-facing name of the
/// private capability matrix above. Authorization stays with
/// [`InitializationState::authorize_command`]; this only labels counters.
pub(crate) fn request_class_label(request: &EnclaveRequest) -> crate::telemetry::RequestClassLabel {
    use crate::telemetry::RequestClassLabel;
    match command_class(request) {
        CommandClass::Never => RequestClassLabel::Never,
        CommandClass::Initialized => RequestClassLabel::Initialized,
        CommandClass::FoundingKeyless => RequestClassLabel::FoundingKeyless,
        CommandClass::KeylessOnboardingArtifact => RequestClassLabel::KeylessOnboarding,
        CommandClass::Ready => RequestClassLabel::Ready,
        CommandClass::DevSourceSeal => RequestClassLabel::DevSourceSeal,
        CommandClass::DevRecipientIngest => RequestClassLabel::DevRecipientIngest,
    }
}

fn unix_time_seconds() -> Result<u64, &'static str> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| "system time precedes Unix epoch")
}

fn command_allowed_for_environment(
    class: CommandClass,
    offer_key_ready: bool,
    gramine_direct_dev_evidence_allowed: bool,
) -> Result<(), &'static str> {
    let allowed = match class {
        CommandClass::Never => false,
        CommandClass::Initialized => true,
        CommandClass::FoundingKeyless => !offer_key_ready,
        CommandClass::KeylessOnboardingArtifact => !offer_key_ready,
        CommandClass::Ready => offer_key_ready,
        // The legacy raw delivery is available only to the local authenticated
        // NodeHost session of an SGX enclave running without remote attestation.
        // Any resident-key holder must be able to replay the deterministic seal
        // performed by registry execution; profile does not change the artifact.
        // DCAP production remains purpose-bound and denied here.
        CommandClass::DevSourceSeal => gramine_direct_dev_evidence_allowed && offer_key_ready,
        CommandClass::DevRecipientIngest => gramine_direct_dev_evidence_allowed && !offer_key_ready,
    };
    allowed
        .then_some(())
        .ok_or("command denied by enclave state matrix")
}

fn persist_manifest(
    boot: &EnclaveBootConfig,
    manifest: &EnclaveInitializationManifestV1,
) -> Result<(), String> {
    let (sealing_key, policy) = crate::transport::sealing_key()
        .ok_or_else(|| "SGX sealing key is unavailable".to_string())?;
    let mut nonce = [0u8; 12];
    rand_core::OsRng.fill_bytes(&mut nonce);
    let header = SealHeader {
        format_version: SEAL_FORMAT,
        key_policy: policy,
        isv_svn: boot.isv_svn,
        key_epoch: 0,
        tribute_offer_epoch: 0,
        nonce,
    };
    let encoded = manifest
        .encode_canonical()
        .map_err(|error| error.to_string())?;
    let authorization_hash = manifest
        .authorization_hash()
        .map_err(|error| error.to_string())?;
    let blob = seal_tribute_offer_and_group_sig(
        &authorization_hash.0,
        &encoded,
        &sealing_key,
        boot.chain_id,
        &header,
    )
    .map_err(|error| error.to_string())?;
    let path = boot.sealed_node_authorization_path();
    crate::transport::write_once_0600(&path, &blob)
        .map_err(|error| format!("persist sealed node authorization: {error}"))
}

fn restore_manifest(
    boot: &EnclaveBootConfig,
    keys: &EnclaveKeys,
) -> Result<Option<EnclaveInitializationManifestV1>, String> {
    let path = boot.sealed_node_authorization_path();
    let blob = match std::fs::read(&path) {
        Ok(blob) => blob,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read sealed node authorization: {error}")),
    };
    let (sealed_hash, encoded, _) = unseal_tribute_offer_and_group_sig(
        &blob,
        &crate::transport::sealing_key()
            .ok_or_else(|| "SGX sealing key is unavailable".to_string())?
            .0,
        boot.chain_id,
        boot.isv_svn,
    )
    .map_err(|error| format!("unseal node authorization: {error}"))?;
    let manifest = EnclaveInitializationManifestV1::decode_canonical(&encoded)
        .map_err(|error| format!("sealed node authorization is non-canonical: {error}"))?;
    if *sealed_hash
        != manifest
            .authorization_hash()
            .map_err(|error| error.to_string())?
            .0
    {
        return Err("sealed node authorization hash mismatch".to_string());
    }
    if manifest.chain_id != boot.chain_id.0
        || manifest.recipient_x25519 != keys.tribute_offer_public()
        || manifest.attestation_ed25519 != keys.attestation_pub()
        || manifest.noise_responder_x25519 != keys.noise_public()
    {
        return Err("sealed node authorization does not match this enclave identity".to_string());
    }
    Ok(Some(manifest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use k256::ecdsa::{signature::hazmat::PrehashSigner as _, SigningKey};
    use outbe_primitives::tee_attestation_v1::{AttestationOperationV1, NodeIdV1};

    /// `Health` is an `Initialized`-class probe: allowed in every
    /// post-handshake state (keyless included), denied only pre-handshake.
    #[test]
    fn health_is_initialized_class_and_allowed_keyless() {
        assert_eq!(
            command_class(&EnclaveRequest::Health),
            CommandClass::Initialized
        );
        assert!(command_allowed_for_environment(CommandClass::Initialized, false, false).is_ok());
        assert!(command_allowed_for_environment(CommandClass::Initialized, true, false).is_ok());
        assert_eq!(
            crate::telemetry::RequestClassLabel::Initialized,
            request_class_label(&EnclaveRequest::Health)
        );
    }

    fn signed_manifest(
        keys: &EnclaveKeys,
        challenge: [u8; 32],
    ) -> (EnclaveInitializationManifestV1, [u8; 65]) {
        let signing = SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
        let reth_p2p_public = signing
            .verifying_key()
            .to_encoded_point(true)
            .as_bytes()
            .try_into()
            .unwrap();
        let manifest = EnclaveInitializationManifestV1 {
            chain_id: [0x10; 32],
            genesis_hash: B256::repeat_byte(0x11),
            node_id: NodeIdV1 { reth_p2p_public },
            initialization_challenge: challenge,
            node_host_noise_x25519: [0x42; 32],
            recipient_x25519: keys.tribute_offer_public(),
            attestation_ed25519: keys.attestation_pub(),
            noise_responder_x25519: keys.noise_public(),
        };
        let (signature, recovery) = signing
            .sign_prehash(manifest.authorization_hash().unwrap().as_slice())
            .unwrap();
        let mut bytes = [0u8; 65];
        bytes[..64].copy_from_slice(signature.to_bytes().as_slice());
        bytes[64] = recovery.to_byte();
        (manifest, bytes)
    }

    #[test]
    fn initialization_is_write_once_and_restores_the_same_node_bound_identity() {
        let root = tempfile::tempdir().unwrap();
        let boot = Arc::new(EnclaveBootConfig::new(
            [0x10; 32],
            root.path().to_path_buf(),
            0,
        ));
        let keys = EnclaveKeys::new([7; 32], Some([1; 32])).unwrap();
        let challenge = [0x41; 32];
        let state =
            InitializationState::production_with_challenge(boot.clone(), &keys, challenge).unwrap();
        let (manifest, signature) = signed_manifest(&keys, challenge);
        let pending = state
            .prepare(&manifest.encode_canonical().unwrap(), &signature, &keys)
            .unwrap();
        state.commit(pending, &keys).unwrap();
        assert_eq!(state.expected_node_host().unwrap(), [0x42; 32]);
        assert!(state
            .prepare(&manifest.encode_canonical().unwrap(), &signature, &keys)
            .unwrap_err()
            .contains("already initialized"));

        let mut conflicting = manifest.clone();
        conflicting.node_host_noise_x25519 = [0x43; 32];
        let signing = SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
        let (conflicting_signature, recovery) = signing
            .sign_prehash(conflicting.authorization_hash().unwrap().as_slice())
            .unwrap();
        let mut conflicting_signature_bytes = [0u8; 65];
        conflicting_signature_bytes[..64]
            .copy_from_slice(conflicting_signature.to_bytes().as_slice());
        conflicting_signature_bytes[64] = recovery.to_byte();
        assert!(conflicting.verify_node_signature(&conflicting_signature_bytes));
        assert!(state
            .prepare(
                &conflicting.encode_canonical().unwrap(),
                &conflicting_signature_bytes,
                &keys,
            )
            .unwrap_err()
            .contains("already initialized"));

        let restored =
            InitializationState::production_with_challenge(boot, &keys, [0x99; 32]).unwrap();
        assert_eq!(restored.manifest().unwrap(), Some(manifest));
        assert!(matches!(
            restored.initialized_response().unwrap(),
            EnclaveResponse::Initialized {
                sealed_loaded: true,
                ..
            }
        ));
    }

    #[test]
    fn initialization_rejects_challenge_key_and_chain_substitution() {
        let root = tempfile::tempdir().unwrap();
        let boot = Arc::new(EnclaveBootConfig::new(
            [0x10; 32],
            root.path().to_path_buf(),
            0,
        ));
        let keys = EnclaveKeys::new([7; 32], Some([1; 32])).unwrap();
        let state =
            InitializationState::production_with_challenge(boot, &keys, [0x41; 32]).unwrap();
        let (mut manifest, _) = signed_manifest(&keys, [0x41; 32]);
        manifest.initialization_challenge[0] ^= 1;
        let (_, signature) = signed_manifest(&keys, [0x41; 32]);
        assert!(state
            .prepare(&manifest.encode_canonical().unwrap(), &signature, &keys)
            .unwrap_err()
            .contains("challenge mismatch"));

        let (mut manifest, _) = signed_manifest(&keys, [0x41; 32]);
        manifest.recipient_x25519[0] ^= 1;
        let (_, signature) = signed_manifest(&keys, [0x41; 32]);
        assert!(state
            .prepare(&manifest.encode_canonical().unwrap(), &signature, &keys)
            .unwrap_err()
            .contains("persistent keys"));

        let (mut manifest, _) = signed_manifest(&keys, [0x41; 32]);
        manifest.chain_id[0] ^= 1;
        let (_, signature) = signed_manifest(&keys, [0x41; 32]);
        assert!(state
            .prepare(&manifest.encode_canonical().unwrap(), &signature, &keys)
            .unwrap_err()
            .contains("chain id"));
    }

    #[test]
    fn remote_authority_has_an_exclusive_deadline_and_no_owner_command_surface() {
        assert!(SessionAuthorityV1::RemoteActiveNode { deadline: 1_000 }
            .ensure_live_at(999)
            .is_ok());
        assert!(SessionAuthorityV1::RemoteActiveNode { deadline: 1_000 }
            .ensure_live_at(1_000)
            .unwrap_err()
            .contains("expired"));

        let root = tempfile::tempdir().unwrap();
        let boot = Arc::new(EnclaveBootConfig::new(
            [0x10; 32],
            root.path().to_path_buf(),
            0,
        ));
        let keys = EnclaveKeys::new([7; 32], Some([1; 32])).unwrap();
        let state =
            InitializationState::production_with_challenge(boot, &keys, [0x41; 32]).unwrap();
        let (manifest, signature) = signed_manifest(&keys, [0x41; 32]);
        let pending = state
            .prepare(&manifest.encode_canonical().unwrap(), &signature, &keys)
            .unwrap();
        state.commit(pending, &keys).unwrap();
        let remote = SessionAuthorityV1::RemoteActiveNode { deadline: u64::MAX };

        assert!(state
            .authorize_command(&EnclaveRequest::GetPublicKeys, false, remote)
            .is_ok());
        for owner_request in [
            EnclaveRequest::GenerateDcapQuote { intent: Vec::new() },
            EnclaveRequest::AuthorizeRemoteSessionV1 {
                ticket_id: B256::repeat_byte(0x71),
                initiator_static_x25519: [0x72; 32],
                responder_static_x25519: [0x73; 32],
                deadline: u64::MAX,
                finalized_block_hash: B256::repeat_byte(0x74),
            },
            EnclaveRequest::SealOfferKeyForRegistry {
                recipient_x25519: [0x75; 32],
            },
        ] {
            assert!(state
                .authorize_command(&owner_request, true, remote)
                .unwrap_err()
                .contains("remote session command denied"));
        }

        let deadline = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        assert!(state
            .authorize_remote_session(
                B256::repeat_byte(0x76),
                [0x77; 32],
                [0x78; 32],
                deadline,
                B256::repeat_byte(0x79),
                &keys,
            )
            .unwrap_err()
            .contains("targets another Noise responder"));

        for index in 0..MAX_PENDING_REMOTE_SESSIONS_V1 {
            state
                .authorize_remote_session(
                    B256::from([u8::try_from(index + 1).unwrap(); 32]),
                    [0x77; 32],
                    keys.noise_public(),
                    deadline,
                    B256::repeat_byte(0x79),
                    &keys,
                )
                .unwrap();
        }
        assert!(state
            .authorize_remote_session(
                B256::repeat_byte(0xF0),
                [0x77; 32],
                keys.noise_public(),
                deadline,
                B256::repeat_byte(0x79),
                &keys,
            )
            .unwrap_err()
            .contains("capacity reached"));
    }

    #[test]
    fn quote_report_data_is_exact_for_initial_renewal_and_replacement_intents() {
        let root = tempfile::tempdir().unwrap();
        let boot = Arc::new(EnclaveBootConfig::new(
            [0x10; 32],
            root.path().to_path_buf(),
            0,
        ));
        let keys = EnclaveKeys::new([7; 32], Some([1; 32])).unwrap();
        let state =
            InitializationState::production_with_challenge(boot, &keys, [0x41; 32]).unwrap();
        let (manifest, signature) = signed_manifest(&keys, [0x41; 32]);
        let pending = state
            .prepare(&manifest.encode_canonical().unwrap(), &signature, &keys)
            .unwrap();
        state.commit(pending, &keys).unwrap();

        for operation in [
            AttestationOperationV1::RegisterEnclave,
            AttestationOperationV1::RenewEnclave,
            AttestationOperationV1::ReplaceEnclaveBinding,
        ] {
            let intent = RegistrationIntentV1 {
                chain_id: manifest.chain_id,
                genesis_hash: manifest.genesis_hash,
                operation,
                attestation_mode:
                    outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired,
                policy_hash: B256::repeat_byte(0x21),
                node_id: manifest.node_id.clone(),
                enclave_id: manifest.enclave_id().unwrap(),
                binding_id: B256::repeat_byte(0x42),
                binding_version: 1 + u64::from(
                    operation == AttestationOperationV1::ReplaceEnclaveBinding,
                ),
                registration_version: u64::from(
                    operation != AttestationOperationV1::RegisterEnclave,
                ),
                renewal_nonce: u64::from(operation == AttestationOperationV1::RenewEnclave),
                transition_nonce: 0,
                requested_valid_until: 7_200,
                recipient_x25519: manifest.recipient_x25519,
                attestation_ed25519: manifest.attestation_ed25519,
                noise_responder_x25519: manifest.noise_responder_x25519,
                node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
            };
            assert_eq!(
                state
                    .quote_report_data(&intent.encode_canonical().unwrap())
                    .unwrap(),
                intent.report_data().unwrap()
            );

            let mut development_intent = intent;
            development_intent.attestation_mode = AttestationMode::GramineDirectDev;
            assert!(state
                .quote_report_data(&development_intent.encode_canonical().unwrap())
                .unwrap_err()
                .contains("requires DcapRequired"));
        }
    }

    #[test]
    fn fresh_enclave_quotes_replacement_under_the_same_node_host_authority() {
        let first_root = tempfile::tempdir().unwrap();
        let replacement_root = tempfile::tempdir().unwrap();
        let first_keys = EnclaveKeys::new([7; 32], Some([1; 32])).unwrap();
        let replacement_keys = EnclaveKeys::new([8; 32], Some([2; 32])).unwrap();
        let first_state = InitializationState::production_with_challenge(
            Arc::new(EnclaveBootConfig::new(
                [0x10; 32],
                first_root.path().to_path_buf(),
                0,
            )),
            &first_keys,
            [0x41; 32],
        )
        .unwrap();
        let replacement_state = InitializationState::production_with_challenge(
            Arc::new(EnclaveBootConfig::new(
                [0x10; 32],
                replacement_root.path().to_path_buf(),
                0,
            )),
            &replacement_keys,
            [0x43; 32],
        )
        .unwrap();
        let (first_manifest, first_signature) = signed_manifest(&first_keys, [0x41; 32]);
        let (replacement_manifest, replacement_signature) =
            signed_manifest(&replacement_keys, [0x43; 32]);

        let first_pending = first_state
            .prepare(
                &first_manifest.encode_canonical().unwrap(),
                &first_signature,
                &first_keys,
            )
            .unwrap();
        first_state.commit(first_pending, &first_keys).unwrap();
        let replacement_pending = replacement_state
            .prepare(
                &replacement_manifest.encode_canonical().unwrap(),
                &replacement_signature,
                &replacement_keys,
            )
            .unwrap();
        replacement_state
            .commit(replacement_pending, &replacement_keys)
            .unwrap();

        assert_ne!(
            first_manifest.authorization_hash().unwrap(),
            replacement_manifest.authorization_hash().unwrap()
        );
        assert_ne!(
            first_manifest.enclave_id().unwrap(),
            replacement_manifest.enclave_id().unwrap()
        );
        assert_eq!(
            first_manifest.node_host_authorization_hash().unwrap(),
            replacement_manifest.node_host_authorization_hash().unwrap()
        );
        assert!(matches!(
            replacement_state.initialized_response().unwrap(),
            EnclaveResponse::Initialized {
                enclave_id,
                node_host_authorization_hash,
                sealed_loaded: false,
            } if enclave_id == replacement_manifest.enclave_id().unwrap()
                && node_host_authorization_hash
                    == first_manifest.node_host_authorization_hash().unwrap()
        ));

        let intent = RegistrationIntentV1 {
            chain_id: replacement_manifest.chain_id,
            genesis_hash: replacement_manifest.genesis_hash,
            operation: AttestationOperationV1::ReplaceEnclaveBinding,
            attestation_mode: AttestationMode::DcapRequired,
            policy_hash: B256::repeat_byte(0x21),
            node_id: replacement_manifest.node_id.clone(),
            enclave_id: replacement_manifest.enclave_id().unwrap(),
            binding_id: B256::repeat_byte(0x44),
            binding_version: 2,
            registration_version: 1,
            renewal_nonce: 0,
            transition_nonce: 0,
            requested_valid_until: 7_200,
            recipient_x25519: replacement_manifest.recipient_x25519,
            attestation_ed25519: replacement_manifest.attestation_ed25519,
            noise_responder_x25519: replacement_manifest.noise_responder_x25519,
            node_host_authorization_hash: first_manifest.node_host_authorization_hash().unwrap(),
        };
        assert_eq!(
            replacement_state
                .quote_report_data(&intent.encode_canonical().unwrap())
                .unwrap(),
            intent.report_data().unwrap()
        );

        let mut wrong_node_host = intent;
        wrong_node_host.node_host_authorization_hash = B256::repeat_byte(0x99);
        assert!(replacement_state
            .quote_report_data(&wrong_node_host.encode_canonical().unwrap())
            .unwrap_err()
            .contains("does not match initialized enclave"));
    }

    #[test]
    fn command_matrix_depends_on_key_state_and_not_on_node_role() {
        use CommandClass::*;
        let cases = [
            (Never, [false, false]),
            (Initialized, [true, true]),
            (FoundingKeyless, [true, false]),
            (KeylessOnboardingArtifact, [true, false]),
            (Ready, [false, true]),
            (DevSourceSeal, [false, false]),
            (DevRecipientIngest, [false, false]),
        ];
        for (class, expected) in cases {
            let actual = [
                command_allowed_for_environment(class, false, false).is_ok(),
                command_allowed_for_environment(class, true, false).is_ok(),
            ];
            assert_eq!(actual, expected, "matrix mismatch for {class:?}");
        }
    }

    #[test]
    fn founding_offer_finalization_is_keyless_only() {
        let request = EnclaveRequest::DkgFinalizeTributeOffer {
            ceremony_id: B256::repeat_byte(0x91),
            sealed_partials: Vec::new(),
            chain_id: B256::repeat_byte(0x92),
            tribute_offer_epoch: 0,
        };
        let class = command_class(&request);
        assert_eq!(class, CommandClass::FoundingKeyless);
        assert!(command_allowed_for_environment(class, false, false).is_ok());
        assert!(command_allowed_for_environment(class, true, false).is_err());
    }

    #[test]
    fn purpose_bound_ingest_is_keyless_only_while_raw_key_commands_remain_denied() {
        let purpose_bound = command_class(&EnclaveRequest::IngestDcapOnboardingArtifactV1 {
            artifact: vec![0x11; 300],
            expected_intent_hash: B256::repeat_byte(0x12),
            expected_tribute_offer_public: [0x13; 32],
            expected_key_epoch: 0,
            expected_tribute_offer_epoch: 0,
        });
        assert_eq!(purpose_bound, CommandClass::KeylessOnboardingArtifact);
        assert!(command_allowed_for_environment(purpose_bound, false, false).is_ok());
        assert!(command_allowed_for_environment(purpose_bound, true, false).is_err());

        for (raw, expected_class) in [
            (
                EnclaveRequest::SealOfferKeyForRegistry {
                    recipient_x25519: [0x21; 32],
                },
                CommandClass::DevSourceSeal,
            ),
            (
                EnclaveRequest::IngestSealedOfferKeyForRegistry {
                    sealed: vec![0x22; 60],
                    expected_tribute_offer_public: [0x23; 32],
                    chain_id: B256::repeat_byte(0x24),
                    tribute_offer_epoch: 0,
                },
                CommandClass::DevRecipientIngest,
            ),
        ] {
            let class = command_class(&raw);
            assert_eq!(class, expected_class);
            assert!(command_allowed_for_environment(class, false, false).is_err());
            assert!(command_allowed_for_environment(class, true, false).is_err());
        }
    }

    #[test]
    fn sgx_without_dcap_allows_only_local_node_host_onboarding_roles() {
        let seal = command_class(&EnclaveRequest::SealOfferKeyForRegistry {
            recipient_x25519: [0x31; 32],
        });
        let ingest = command_class(&EnclaveRequest::IngestSealedOfferKeyForRegistry {
            sealed: vec![0x32; 60],
            expected_tribute_offer_public: [0x33; 32],
            chain_id: B256::repeat_byte(0x34),
            tribute_offer_epoch: 0,
        });

        assert!(command_allowed_for_environment(seal, true, true).is_ok());
        assert!(command_allowed_for_environment(ingest, false, true).is_ok());
        assert!(command_allowed_for_environment(ingest, true, true).is_err());
        assert!(command_allowed_for_environment(ingest, false, false).is_err());
    }
}
