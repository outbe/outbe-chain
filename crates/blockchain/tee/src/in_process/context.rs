//! Shared resident ownership for execution, canary and account-query lanes.
use super::{
    admission::VerifiedAdmissionAnchorV1,
    dkg::{build_ceremony_info, CeremonyKey, DkgSession, DkgSessionStore, PubKey},
    InProcessError, ProofSourceV1, Result,
};
use crate::{
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
    sync::{Arc, Mutex, RwLock},
    time::Instant,
};

pub(super) struct ActiveMaterial {
    pub material: Arc<ResidentOfferMaterial>,
    pub anchor: Option<VerifiedAdmissionAnchorV1>,
}
pub struct InProcessTeeContext {
    pub(super) keys: ResidentTeeKeys,
    pub(super) active: RwLock<Option<ActiveMaterial>>,
    sessions: Mutex<DkgSessionStore>,
    started: Instant,
    pub(super) identity: NodeHostIdentityV1,
    pub(super) manifest: EnclaveInitializationManifestV1,
    pub(super) descriptor: TrustedNetworkDescriptorV1,
    pub(super) proof_source: ProofSourceV1,
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
        })
    }
    pub fn identity(&self) -> &NodeHostIdentityV1 {
        &self.identity
    }
    pub fn manifest(&self) -> &EnclaveInitializationManifestV1 {
        &self.manifest
    }
    pub fn authorization_witness(&self) -> Result<NodeHostAuthorizationWitnessV1> {
        Ok(NodeHostAuthorizationWitnessV1::from_manifest(
            &self.manifest,
        )?)
    }
    pub fn uptime_s(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
    pub fn noise_public(&self) -> [u8; 32] {
        self.keys.noise_public()
    }
    pub fn recipient_public(&self) -> [u8; 32] {
        self.keys.tribute_offer_public()
    }
    pub fn attestation_public(&self) -> [u8; 32] {
        self.keys.attestation_pub()
    }
    pub fn dkg_enc_public(&self) -> [u8; 32] {
        self.keys.dkg_enc_public()
    }
    pub fn tee_bls_public_bytes(&self) -> Vec<u8> {
        self.keys.tee_bls_public_bytes()
    }
    pub fn sign_domain_attestation(&self, preimage: &[u8]) -> [u8; 64] {
        self.keys.sign_domain_attestation(preimage)
    }
    pub fn offer_key_ready(&self) -> Result<bool> {
        Ok(self
            .active
            .read()
            .map_err(|_| InProcessError::LockPoisoned)?
            .is_some())
    }
    pub fn network_encryption_public(&self) -> Result<[u8; 32]> {
        Ok(self.material()?.public())
    }
    pub fn key_epoch(&self) -> Result<u64> {
        Ok(self.material()?.key_epoch())
    }
    pub fn network_encryption_epoch(&self) -> Result<u64> {
        Ok(self.material()?.tribute_offer_epoch())
    }
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
        *active = Some(ActiveMaterial {
            material: Arc::new(material),
            anchor,
        });
        Ok(())
    }
    pub fn state_key_handle(&self, label: &[u8], epoch: u64) -> Result<StateKeyHandle> {
        Ok(self.material()?.derive_state_key_handle(
            label,
            B256::from(self.manifest.chain_id),
            epoch,
        )?)
    }
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
    pub fn with_offer_key_handle<T>(
        &self,
        operation: impl FnOnce(&OfferKeyHandle) -> T,
    ) -> Result<T> {
        Ok(operation(self.material()?.offer_key_handle()))
    }
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
    pub fn abort_dkg_session(&self, id: &CeremonyKey) -> Result<()> {
        self.sessions
            .lock()
            .map_err(|_| InProcessError::LockPoisoned)?
            .remove(id);
        Ok(())
    }
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
