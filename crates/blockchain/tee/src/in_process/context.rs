//! Shared resident ownership for execution, canary and account-query lanes.
use super::{
    admission::VerifiedAdmissionAnchorV1,
    dkg::{build_ceremony_info, CeremonyKey, DkgSession, DkgSessionStore, PubKey},
    InProcessError, ProofSourceV1, Result,
};
use crate::{
    node_host::ResidentStateStore,
    resident::{
        derive_account_keys_handle, AccountKeysHandle, OfferKeyHandle, ResidentOfferMaterial,
        ResidentTeeKeys, StateKeyHandle,
    },
    NodeHostIdentityV1,
};
use alloy_primitives::{Address, B256};
use commonware_cryptography::Signer as _;
use outbe_primitives::tee_attestation_v1::{
    EnclaveInitializationManifestV1, NodeHostAuthorizationWitnessV1, TrustedNetworkDescriptorV1,
};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, RwLock},
    time::Instant,
};

pub(super) struct ActiveMaterial {
    pub material: Arc<ResidentOfferMaterial>,
    pub anchor: Option<VerifiedAdmissionAnchorV1>,
}
/// Thread-safe RAM owner for one bound identity, ceremony store and admitted network key.
pub struct InProcessTeeContext {
    pub(super) keys: ResidentTeeKeys,
    pub(super) active: RwLock<Option<ActiveMaterial>>,
    sessions: Mutex<DkgSessionStore>,
    started: Instant,
    pub(super) identity: NodeHostIdentityV1,
    pub(super) manifest: EnclaveInitializationManifestV1,
    pub(super) descriptor: TrustedNetworkDescriptorV1,
    pub(super) proof_source: ProofSourceV1,
    resident_state_store: Option<ResidentStateStore>,
}
impl std::fmt::Debug for InProcessTeeContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("InProcessTeeContext([REDACTED])")
    }
}
impl InProcessTeeContext {
    /// The caller supplies the trusted genesis descriptor, never one from a proof sender.
    pub fn new(
        keys: ResidentTeeKeys,
        identity: NodeHostIdentityV1,
        manifest: EnclaveInitializationManifestV1,
        descriptor: TrustedNetworkDescriptorV1,
        proof_source: ProofSourceV1,
    ) -> Result<Self> {
        proof_source.ensure_available(manifest.attestation_mode)?;
        manifest.encode_canonical()?;
        let host_matches = identity.network_binding == manifest.network_binding()
            && identity.reth_p2p_public == manifest.node_id.reth_p2p_public;
        let keys_match = (
            keys.noise_public(),
            keys.tribute_offer_public(),
            keys.attestation_pub(),
        ) == (
            manifest.noise_responder_x25519,
            manifest.recipient_x25519,
            manifest.attestation_ed25519,
        );
        if !host_matches || !keys_match || descriptor.network_binding != manifest.network_binding()
        {
            return Err(InProcessError::IdentityMismatch);
        }
        Ok(Self {
            keys,
            active: RwLock::new(None),
            sessions: Mutex::new(DkgSessionStore::new()),
            started: Instant::now(),
            identity,
            manifest,
            descriptor,
            proof_source,
            resident_state_store: None,
        })
    }
    /// Attach the local state file and restore material and anchor that it contains.
    pub fn with_restored_state(
        mut self,
        store: ResidentStateStore,
        material: Option<ResidentOfferMaterial>,
        anchor: Option<VerifiedAdmissionAnchorV1>,
    ) -> Self {
        self.resident_state_store = Some(store);
        if let Some(material) = material {
            self.active = RwLock::new(Some(ActiveMaterial {
                material: Arc::new(material),
                anchor,
            }));
        }
        self
    }

    /// Save a finalized successor before the runtime switches to it.
    pub fn persist_for_promotion(&self, node_data_dir: &Path) -> Result<()> {
        let material = self.material()?;
        let anchor = self.admission_anchor()?;
        let manifest_authorization_hash = self.manifest.authorization_hash()?;
        ResidentStateStore::replace_promoted(
            node_data_dir,
            &self.keys,
            self.manifest.initialization_challenge,
            manifest_authorization_hash,
            &material,
            anchor,
        )?;
        Ok(())
    }
    /// Return the immutable NodeHost identity bound at construction.
    pub fn identity(&self) -> &NodeHostIdentityV1 {
        &self.identity
    }
    /// Return the exact local initialization manifest, not an admission-sender assertion.
    pub fn manifest(&self) -> &EnclaveInitializationManifestV1 {
        &self.manifest
    }
    /// Construct the stable NodeHost authorization witness from the bound manifest.
    pub fn authorization_witness(&self) -> Result<NodeHostAuthorizationWitnessV1> {
        Ok(NodeHostAuthorizationWitnessV1::from_manifest(
            &self.manifest,
        )?)
    }
    /// Return elapsed process uptime using a monotonic clock.
    pub fn uptime_s(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
    /// Return the public Noise responder key.
    pub fn noise_public(&self) -> [u8; 32] {
        self.keys.noise_public()
    }
    /// Return the public onboarding-artifact recipient key.
    pub fn recipient_public(&self) -> [u8; 32] {
        self.keys.tribute_offer_public()
    }
    /// Return the Ed25519 public key authenticating domain attestations.
    pub fn attestation_public(&self) -> [u8; 32] {
        self.keys.attestation_pub()
    }
    /// Return the public key used to seal this participant's DKG shares.
    pub fn dkg_enc_public(&self) -> [u8; 32] {
        self.keys.dkg_enc_public()
    }
    /// Return the canonical public BLS ceremony identity.
    pub fn tee_bls_public_bytes(&self) -> Vec<u8> {
        self.keys.tee_bls_public_bytes()
    }
    /// Sign a caller-prepared domain-separated preimage without exposing the signing key.
    pub fn sign_domain_attestation(&self, preimage: &[u8]) -> [u8; 64] {
        self.keys.sign_attestation(preimage)
    }
    /// Report whether resident offer material has been activated.
    pub fn offer_key_ready(&self) -> Result<bool> {
        Ok(self
            .active
            .read()
            .map_err(|_| InProcessError::LockPoisoned)?
            .is_some())
    }
    /// Return the active network offer public key, or NotReady.
    pub fn network_encryption_public(&self) -> Result<[u8; 32]> {
        Ok(self.material()?.public())
    }
    /// Return the admitted key epoch, or NotReady.
    pub fn key_epoch(&self) -> Result<u64> {
        Ok(self.material()?.key_epoch())
    }
    /// Return the admitted network encryption epoch, or NotReady.
    pub fn network_encryption_epoch(&self) -> Result<u64> {
        Ok(self.material()?.tribute_offer_epoch())
    }
    /// Return the verified admission anchor; founding DKG has no admission anchor.
    pub fn admission_anchor(&self) -> Result<Option<VerifiedAdmissionAnchorV1>> {
        Ok(self
            .active
            .read()
            .map_err(|_| InProcessError::LockPoisoned)?
            .as_ref()
            .and_then(|a| a.anchor))
    }
    pub(super) fn material(&self) -> Result<Arc<ResidentOfferMaterial>> {
        self.active
            .read()
            .map_err(|_| InProcessError::LockPoisoned)?
            .as_ref()
            .map(|a| Arc::clone(&a.material))
            .ok_or(InProcessError::NotReady)
    }
    pub(super) fn activate(
        &self,
        material: ResidentOfferMaterial,
        anchor: Option<VerifiedAdmissionAnchorV1>,
    ) -> Result<()> {
        let mut active = self
            .active
            .write()
            .map_err(|_| InProcessError::LockPoisoned)?;
        if let Some(old) = active.as_ref() {
            if old.material.public() != material.public()
                || old.material.key_epoch() != material.key_epoch()
                || old.material.tribute_offer_epoch() != material.tribute_offer_epoch()
                || old.anchor.is_some_and(|old| {
                    anchor.is_none_or(|new| {
                        new.block_number < old.block_number
                            || (new.block_number == old.block_number && new != old)
                    })
                })
            {
                return Err(InProcessError::ActivationConflict);
            }
        }
        if let Some(store) = &self.resident_state_store {
            let manifest_authorization_hash = self.manifest.authorization_hash()?;
            store.persist_active(
                &self.keys,
                self.manifest.initialization_challenge,
                manifest_authorization_hash,
                &material,
                anchor,
            )?;
        }
        *active = Some(ActiveMaterial {
            material: Arc::new(material),
            anchor,
        });
        Ok(())
    }
    /// Derive an opaque state key using the domain adapter's label and epoch.
    pub fn state_key_handle(&self, label: &[u8], epoch: u64) -> Result<StateKeyHandle> {
        Ok(self.material()?.derive_state_key_handle(
            label,
            B256::from(self.manifest.chain_id),
            epoch,
        )?)
    }
    /// Derive opaque account view/modify keys from a domain-bound state key.
    pub fn account_keys_handle(
        &self,
        state_label: &[u8],
        epoch: u64,
        account: Address,
        view_label: &[u8],
        modify_label: &[u8],
    ) -> Result<AccountKeysHandle> {
        Ok(derive_account_keys_handle(
            &self.state_key_handle(state_label, epoch)?,
            account,
            view_label,
            modify_label,
        )?)
    }
    /// Borrow opaque resident material without exposing secret bytes or holding a lane lock.
    pub fn with_offer_material<T>(
        &self,
        operation: impl FnOnce(&ResidentOfferMaterial) -> T,
    ) -> Result<T> {
        Ok(operation(self.material()?.as_ref()))
    }
    /// Activate deterministic material only in isolated test fixtures.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn activate_for_test(&self, material: ResidentOfferMaterial) -> Result<()> {
        self.activate(material, None)
    }
    /// Activate deterministic material and a verified admission anchor in tests.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn activate_for_test_with_anchor(
        &self,
        material: ResidentOfferMaterial,
        anchor: crate::in_process::admission::VerifiedAdmissionAnchorV1,
    ) -> Result<()> {
        self.activate(material, Some(anchor))
    }
    /// Borrow the opaque offer handle while keeping its resident owner alive.
    pub fn with_offer_key_handle<T>(
        &self,
        operation: impl FnOnce(&OfferKeyHandle) -> T,
    ) -> Result<T> {
        Ok(operation(self.material()?.offer_key_handle()))
    }
    /// Bind a fresh ceremony to this identity and an exact participant encryption map.
    pub fn open_dkg_session(
        &self,
        id: CeremonyKey,
        round: u64,
        participants: &[Vec<u8>],
        recipient_enc_keys: BTreeMap<PubKey, [u8; 32]>,
    ) -> Result<()> {
        self.proof_source
            .ensure_available(self.manifest.attestation_mode)?;
        let (info, players) = build_ceremony_info(round, participants)?;
        if players.len() != recipient_enc_keys.len()
            || !players.iter().eq(recipient_enc_keys.keys())
            || recipient_enc_keys.get(&self.keys.tee_bls_key.public_key())
                != Some(&self.dkg_enc_public())
        {
            return Err(InProcessError::IdentityMismatch);
        }
        self.sessions
            .lock()
            .map_err(|_| InProcessError::LockPoisoned)?
            .open(
                id,
                info,
                self.keys.tee_bls_key.clone(),
                &self.keys.dkg_enc_secret,
                recipient_enc_keys,
            )
    }
    /// Run one ceremony seam under the session lock; callbacks must not re-enter this context.
    pub fn with_dkg_session<T>(
        &self,
        id: &CeremonyKey,
        operation: impl FnOnce(&mut DkgSession) -> Result<T>,
    ) -> Result<T> {
        operation(
            self.sessions
                .lock()
                .map_err(|_| InProcessError::LockPoisoned)?
                .get_mut(id)?,
        )
    }
    /// Drop an abandoned ceremony and zeroize its owned secret state.
    pub fn abort_dkg_session(&self, id: &CeremonyKey) -> Result<()> {
        self.sessions
            .lock()
            .map_err(|_| InProcessError::LockPoisoned)?
            .remove(id);
        Ok(())
    }
    /// Verify/recover sealed offer partials, activate in RAM and drop the ceremony atomically.
    pub fn complete_founding_dkg(
        &self,
        id: &CeremonyKey,
        sealed_partials: &[Vec<u8>],
        network_encryption_epoch: u64,
    ) -> Result<[u8; 32]> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| InProcessError::LockPoisoned)?;
        let material = sessions.get_mut(id)?.recover_tribute_offer_material(
            sealed_partials,
            B256::from(self.manifest.chain_id),
            network_encryption_epoch,
        )?;
        let public = material.public();
        self.activate(material, None)?;
        sessions.remove(id);
        Ok(public)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_process::test_support;
    use outbe_primitives::tee_attestation_v1::AttestationMode;
    use std::{fs, os::unix::fs::PermissionsExt as _};
    #[test]
    fn in_process_activation_rejection_preserves_active_material_and_anchor() {
        let context =
            test_support::context(AttestationMode::NoAttestationDev, ProofSourceV1::NoHardware);
        let anchor = VerifiedAdmissionAnchorV1 {
            block_number: 10,
            block_hash: B256::repeat_byte(1),
            state_root: B256::repeat_byte(2),
            consensus_timestamp: 1000,
        };
        let original = test_support::material(&context, 5);
        let public = original.public();
        context.activate(original, Some(anchor)).unwrap();
        for (byte, candidate) in [
            (6, Some(anchor)),
            (5, None),
            (
                5,
                Some(VerifiedAdmissionAnchorV1 {
                    block_number: 9,
                    ..anchor
                }),
            ),
            (
                5,
                Some(VerifiedAdmissionAnchorV1 {
                    block_hash: B256::repeat_byte(3),
                    ..anchor
                }),
            ),
        ] {
            assert!(matches!(
                context.activate(test_support::material(&context, byte), candidate),
                Err(InProcessError::ActivationConflict)
            ));
            assert_eq!(context.network_encryption_public().unwrap(), public);
            assert_eq!(context.admission_anchor().unwrap(), Some(anchor));
        }
    }

    #[test]
    fn active_material_is_not_published_when_resident_file_cannot_be_written() {
        let directory = tempfile::tempdir().unwrap();
        let fixture =
            test_support::context(AttestationMode::NoAttestationDev, ProofSourceV1::NoHardware);
        let authorization = fixture.authorization_witness().unwrap();
        let (store, loaded) =
            ResidentStateStore::load_or_create(directory.path(), &authorization).unwrap();
        let mut manifest = fixture.manifest.clone();
        manifest.recipient_x25519 = loaded.keys.tribute_offer_public();
        manifest.attestation_ed25519 = loaded.keys.attestation_pub();
        manifest.noise_responder_x25519 = loaded.keys.noise_public();
        manifest.initialization_challenge = loaded.initialization_challenge;
        let context = InProcessTeeContext::new(
            loaded.keys,
            fixture.identity,
            manifest,
            fixture.descriptor,
            ProofSourceV1::NoHardware,
        )
        .unwrap()
        .with_restored_state(store, None, None);
        let path = directory
            .path()
            .join("tee-node-host-v1")
            .join("resident-state.v1");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(matches!(
            context.activate(test_support::material(&context, 5), None),
            Err(InProcessError::ResidentState(
                crate::node_host::ResidentStateError::UnsafePath
            ))
        ));
        assert!(!context.offer_key_ready().unwrap());
    }

    #[test]
    fn context_persists_and_restores_tag1_and_tag2_anchor_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let fixture =
            test_support::context(AttestationMode::NoAttestationDev, ProofSourceV1::NoHardware);
        let authorization = fixture.authorization_witness().unwrap();
        let (store, loaded) =
            ResidentStateStore::load_or_create(directory.path(), &authorization).unwrap();
        let mut manifest = fixture.manifest.clone();
        manifest.recipient_x25519 = loaded.keys.tribute_offer_public();
        manifest.attestation_ed25519 = loaded.keys.attestation_pub();
        manifest.noise_responder_x25519 = loaded.keys.noise_public();
        manifest.initialization_challenge = loaded.initialization_challenge;
        let context = InProcessTeeContext::new(
            loaded.keys,
            fixture.identity,
            manifest.clone(),
            fixture.descriptor.clone(),
            ProofSourceV1::NoHardware,
        )
        .unwrap()
        .with_restored_state(store, loaded.material, loaded.anchor);

        let anchor = VerifiedAdmissionAnchorV1 {
            block_number: 15,
            block_hash: B256::repeat_byte(0x15),
            state_root: B256::repeat_byte(0x25),
            consensus_timestamp: 1_700_000_015,
        };
        let material = test_support::material(&context, 5);
        let public = material.public();
        context.activate(material, Some(anchor)).unwrap();

        let (store_reloaded, reloaded) =
            ResidentStateStore::load_or_create(directory.path(), &authorization).unwrap();
        let restored = InProcessTeeContext::new(
            reloaded.keys,
            fixture.identity,
            manifest,
            fixture.descriptor,
            ProofSourceV1::NoHardware,
        )
        .unwrap()
        .with_restored_state(store_reloaded, reloaded.material, reloaded.anchor);
        assert!(restored.offer_key_ready().unwrap());
        assert_eq!(restored.network_encryption_public().unwrap(), public);
        assert_eq!(restored.admission_anchor().unwrap(), Some(anchor));
    }
}
