//! Write-once enclave initialization and NodeHost command authorization.

use std::sync::{Arc, Mutex};

use outbe_primitives::tee_attestation_v1::{
    AttestationMode, EnclaveInitializationManifestV1, EnclaveProfile, RegistrationIntentV1,
};
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use rand_core::RngCore as _;

use crate::keys::EnclaveKeys;
use crate::seal::{
    seal_tribute_offer_and_group_sig, unseal_tribute_offer_and_group_sig, EnclaveBootConfig,
    SealHeader, SEAL_FORMAT,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitializationMode {
    Production,
    Development,
}

#[derive(Clone, Debug)]
pub struct PendingInitialization {
    manifest: EnclaveInitializationManifestV1,
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
    challenge: [u8; 32],
    boot: Option<Arc<EnclaveBootConfig>>,
    stored: Mutex<Option<StoredInitialization>>,
}

impl InitializationState {
    pub fn production(boot: Arc<EnclaveBootConfig>, keys: &EnclaveKeys) -> Result<Self, String> {
        let mut challenge = [0u8; 32];
        rand_core::OsRng.fill_bytes(&mut challenge);
        if crate::transport::sealing_key().is_none() {
            return Err("production initialization requires an SGX sealing key".to_string());
        }
        Self::production_with_challenge(boot, keys, challenge)
    }

    fn production_with_challenge(
        boot: Arc<EnclaveBootConfig>,
        keys: &EnclaveKeys,
        challenge: [u8; 32],
    ) -> Result<Self, String> {
        if challenge == [0; 32] {
            return Err("initialization challenge must be nonzero".to_string());
        }
        let restored = restore_manifest(&boot, keys)?;
        Ok(Self {
            mode: InitializationMode::Production,
            challenge,
            boot: Some(boot),
            stored: Mutex::new(restored.map(|manifest| StoredInitialization {
                manifest,
                loaded_from_seal: true,
            })),
        })
    }

    /// Separate dev/mock behavior. It never creates a production authorization
    /// claim and is selected only by the required-feature mock binary or tests.
    pub fn development() -> Self {
        Self {
            mode: InitializationMode::Development,
            challenge: [0xDD; 32],
            boot: None,
            stored: Mutex::new(None),
        }
    }

    pub fn mode(&self) -> InitializationMode {
        self.mode
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
                .authorization_hash()
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
    ) -> Result<(), &'static str> {
        if self.mode == InitializationMode::Development {
            return Ok(());
        }
        let profile = self
            .manifest()
            .map_err(|_| "initialization state unavailable")?
            .ok_or("enclave is not initialized")?
            .enclave_profile;
        command_allowed(command_class(request), profile, offer_key_ready)
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
    ValidatorKeyless,
    Ready,
    FinalizedAuthorizationRequired,
}

fn command_class(request: &EnclaveRequest) -> CommandClass {
    match request {
        EnclaveRequest::GetPublicKeys
        | EnclaveRequest::GenerateDcapQuote { .. }
        | EnclaveRequest::BeginDcapVerificationV1 { .. }
        | EnclaveRequest::DcapVerificationChunkV1 { .. }
        | EnclaveRequest::FinishDcapVerificationV1 { .. } => CommandClass::Initialized,
        EnclaveRequest::DkgOpen { .. }
        | EnclaveRequest::DkgStartDealer { .. }
        | EnclaveRequest::DkgPlayerIngest { .. }
        | EnclaveRequest::DkgDealerReceiveAck { .. }
        | EnclaveRequest::DkgDealerFinalize { .. }
        | EnclaveRequest::DkgPlayerFinalize { .. }
        | EnclaveRequest::DkgTributeOfferPartial { .. }
        | EnclaveRequest::DkgRecoverTributeOffer { .. } => CommandClass::ValidatorKeyless,
        EnclaveRequest::ProcessTributeOfferBatch { .. }
        | EnclaveRequest::ApplyGratisOp { .. }
        | EnclaveRequest::ApplyPromisOp { .. }
        | EnclaveRequest::DeriveAccountKeys { .. } => CommandClass::Ready,
        EnclaveRequest::SealTributeOfferHandoff { .. }
        | EnclaveRequest::SealOfferKeyForRegistry { .. }
        | EnclaveRequest::IngestTributeOfferHandoff { .. } => {
            CommandClass::FinalizedAuthorizationRequired
        }
        EnclaveRequest::GetQuote { .. }
        | EnclaveRequest::GetInitializationChallenge
        | EnclaveRequest::Initialize { .. }
        | EnclaveRequest::OpenSession
        | EnclaveRequest::SessionHandshake { .. } => CommandClass::Never,
    }
}

fn command_allowed(
    class: CommandClass,
    profile: EnclaveProfile,
    offer_key_ready: bool,
) -> Result<(), &'static str> {
    let allowed = match class {
        CommandClass::Never => false,
        CommandClass::Initialized => true,
        CommandClass::ValidatorKeyless => profile == EnclaveProfile::Validator && !offer_key_ready,
        CommandClass::Ready => offer_key_ready,
        // I6 replaces this unconditional denial with verification of the exact
        // canonical finalized proof inside the enclave, not NodeHost assertions.
        CommandClass::FinalizedAuthorizationRequired => false,
    };
    allowed
        .then_some(())
        .ok_or("command denied by enclave profile/state matrix")
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

    fn signed_manifest(
        keys: &EnclaveKeys,
        challenge: [u8; 32],
    ) -> (EnclaveInitializationManifestV1, [u8; 65]) {
        let signing = SigningKey::from_bytes((&[0x61; 32]).into()).unwrap();
        let public = signing.verifying_key().to_encoded_point(false);
        let hash = alloy_primitives::keccak256(&public.as_bytes()[1..]);
        let mut address = [0u8; 20];
        address.copy_from_slice(&hash[12..]);
        let manifest = EnclaveInitializationManifestV1 {
            chain_id: [0x10; 32],
            genesis_hash: B256::repeat_byte(0x11),
            enclave_profile: EnclaveProfile::Validator,
            node_id: NodeIdV1::Validator {
                address,
                bls_minpk_public: [0x32; 48],
            },
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
    fn quote_report_data_is_exact_for_initial_and_renewal_intents() {
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
        ] {
            let intent = RegistrationIntentV1 {
                chain_id: manifest.chain_id,
                genesis_hash: manifest.genesis_hash,
                operation,
                attestation_mode:
                    outbe_primitives::tee_attestation_v1::AttestationMode::DcapRequired,
                policy_hash: B256::repeat_byte(0x21),
                enclave_profile: manifest.enclave_profile,
                node_id: manifest.node_id.clone(),
                enclave_id: manifest.enclave_id().unwrap(),
                binding_id: B256::repeat_byte(0x42),
                binding_version: 1,
                registration_version: 0,
                renewal_nonce: u64::from(operation == AttestationOperationV1::RenewEnclave),
                transition_nonce: 0,
                requested_valid_until: 7_200,
                recipient_x25519: manifest.recipient_x25519,
                attestation_ed25519: manifest.attestation_ed25519,
                noise_responder_x25519: manifest.noise_responder_x25519,
                node_host_authorization_hash: manifest.authorization_hash().unwrap(),
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
    fn command_matrix_is_deny_by_default_for_both_profiles_and_readiness_states() {
        use CommandClass::*;
        let cases = [
            (Never, [false, false, false, false]),
            (Initialized, [true, true, true, true]),
            (ValidatorKeyless, [true, false, false, false]),
            (Ready, [false, true, false, true]),
            (FinalizedAuthorizationRequired, [false, false, false, false]),
        ];
        for (class, expected) in cases {
            let actual = [
                command_allowed(class, EnclaveProfile::Validator, false).is_ok(),
                command_allowed(class, EnclaveProfile::Validator, true).is_ok(),
                command_allowed(class, EnclaveProfile::FullNode, false).is_ok(),
                command_allowed(class, EnclaveProfile::FullNode, true).is_ok(),
            ];
            assert_eq!(actual, expected, "matrix mismatch for {class:?}");
        }
    }
}
