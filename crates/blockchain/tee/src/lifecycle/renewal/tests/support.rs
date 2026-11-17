//! Public-behavior fixture shared by the two resident reducer suites.
use super::super::LifecycleSignerV1;
use crate::lifecycle::*;
use crate::registry_view::{NodeBindingSelectorV1, RenewalBindingV1};
use crate::{
    in_process::{InProcessTeeContext, ProofSourceV1},
    resident::ResidentTeeKeys,
    NodeHostIdentityV1,
};
use alloy_primitives::{keccak256, Address, Signature, B256, U256};
use outbe_primitives::{
    tee_attestation_v1::*, tee_operator_v1::TeeRenewalScheduleV1, validator_dto::ValidatorRecord,
};
use std::sync::Mutex;

pub(crate) struct Signer(pub k256::ecdsa::SigningKey);
impl Default for Signer {
    fn default() -> Self {
        Self(k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap())
    }
}
impl LifecycleSignerV1 for Signer {
    type Error = k256::ecdsa::Error;
    fn address(&self) -> Address {
        Address::from_public_key(self.0.verifying_key())
    }
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65], Self::Error> {
        Ok(self.sign_transaction_hash(hash)?.as_bytes())
    }
    fn sign_transaction_hash(&self, hash: B256) -> Result<Signature, Self::Error> {
        let (signature, recovery) = self.0.sign_prehash_recoverable(hash.as_slice())?;
        Ok(Signature::from_signature_and_parity(
            signature,
            recovery.is_y_odd(),
        ))
    }
}
pub(crate) fn context(seed: u8) -> InProcessTeeContext {
    let keys = ResidentTeeKeys::from_seed([seed; 32]).unwrap();
    let signer = Signer::default();
    let m = EnclaveInitializationManifestV1 {
        chain_id: U256::from(676).to_be_bytes(),
        genesis_hash: B256::repeat_byte(2),
        attestation_mode: AttestationMode::NoAttestationDev,
        node_id: NodeIdV1 {
            reth_p2p_public: signer
                .0
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
        },
        initialization_challenge: [seed; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let identity = NodeHostIdentityV1 {
        network_binding: m.network_binding(),
        reth_p2p_public: m.node_id.reth_p2p_public,
    };
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: m.network_binding(),
        genesis_consensus_keys: vec![],
    };
    InProcessTeeContext::new(keys, identity, m, descriptor, ProofSourceV1::NoHardware).unwrap()
}
pub(crate) fn view(live: &InProcessTeeContext) -> LifecycleRegistryStateV1 {
    let m = live.manifest();
    let mut policy =
        outbe_primitives::tee_test_utils::no_attestation_dev_policy_v1(676, m.genesis_hash)
            .unwrap();
    policy.maximum_lease = 7200;
    policy.minimum_lease = 3600;
    let at = LifecycleCheckpointV1 {
        height: 5,
        hash: B256::repeat_byte(5),
        state_root: B256::repeat_byte(6),
        timestamp: 3600,
        base_fee_per_gas: U256::from(17),
        chain_id: 676,
        genesis_hash: m.genesis_hash,
    };
    let mut binding = crate::test_utils::RenewalBindingFixtureV1::new(1)
        .versions(1, 1)
        .lease(0, 7200, 30000)
        .build();
    binding.node_id_hash = m.node_id.node_id_hash().unwrap();
    binding.enclave_id = m.enclave_id().unwrap();
    binding.node_host_authorization_hash = m.node_host_authorization_hash().unwrap();
    binding.policy_hash = policy.policy_hash().unwrap();
    binding.recipient_x25519 = m.recipient_x25519.into();
    binding.attestation_ed25519 = m.attestation_ed25519.into();
    binding.noise_responder_x25519 = m.noise_responder_x25519.into();
    LifecycleRegistryStateV1 {
        checkpoint: at,
        binding: Some(binding),
        policy,
        schedule: TeeRenewalScheduleV1 {
            finalized_height: at.height,
            finalized_hash: at.hash,
            finalized_timestamp: at.timestamp,
            epoch_number: 0,
            epoch_start_height: 0,
            epoch_length_blocks: 100,
            next_freeze_height: 90,
            planned_activation_height: 100,
            dkg_prepare_window_blocks: 10,
            minimum_block_time_millis: 1000,
        },
        active_upgrade: None,
        successor_policy: None,
        candidate: None,
        network_encryption_public_key: B256::repeat_byte(9),
        key_epoch: 0,
        network_encryption_epoch: 0,
    }
}
pub(crate) type PortResult<T> = Result<T, LifecyclePortError<std::io::Error>>;
pub(crate) struct Chain {
    pub view: LifecycleRegistryStateV1,
    pub nonces: [u64; 3],
    pub upgrade_nonce: u64,
    pub sent: Mutex<Vec<Vec<u8>>>,
    pub receipt: Option<LifecycleReceiptV1>,
    pub upgrade_reply: Option<LifecycleOnboardingProofV1>,
    pub wrong_hash: bool,
    pub nonce_error: bool,
    pub journal_path: std::path::PathBuf,
}
impl Chain {
    pub fn new(view: LifecycleRegistryStateV1, root: &std::path::Path) -> Self {
        Self {
            view,
            nonces: [4; 3],
            upgrade_nonce: 0,
            sent: Mutex::new(vec![]),
            receipt: None,
            upgrade_reply: None,
            wrong_hash: false,
            nonce_error: false,
            journal_path: root.join("tee-service-tx-v1/journal.json"),
        }
    }
    pub fn timestamp(&mut self, t: u64) {
        self.view.checkpoint.timestamp = t;
        self.view.schedule.finalized_timestamp = t;
    }
    pub fn binding(&mut self) -> &mut RenewalBindingV1 {
        self.view.binding.as_mut().unwrap()
    }
}
impl LifecycleChainPort for Chain {
    type Error = std::io::Error;
    async fn finalized_checkpoint(&self) -> PortResult<LifecycleCheckpointV1> {
        Ok(self.view.checkpoint)
    }
    async fn registry_state(
        &self,
        _: LifecycleCheckpointV1,
        _: &NodeBindingSelectorV1,
    ) -> PortResult<LifecycleRegistryStateV1> {
        Ok(self.view.clone())
    }
    async fn pending_upgrade_nonce(
        &self,
        _: LifecycleCheckpointV1,
        _: &NodeBindingSelectorV1,
    ) -> PortResult<u64> {
        Ok(self.upgrade_nonce)
    }
    async fn validator_record(
        &self,
        _: LifecycleCheckpointV1,
        _: Address,
    ) -> PortResult<Option<ValidatorRecord>> {
        Err(LifecyclePortError::Source(std::io::Error::other(
            "role must not gate renewal",
        )))
    }
    async fn balance(&self, _: LifecycleCheckpointV1, _: Address) -> PortResult<U256> {
        Ok(U256::ZERO)
    }
    async fn account_nonce(&self, _: Address, tag: LifecycleNonceTagV1) -> PortResult<u64> {
        Ok(self.nonces[match tag {
            LifecycleNonceTagV1::Finalized(_) => 0,
            LifecycleNonceTagV1::Latest => 1,
            LifecycleNonceTagV1::Pending => 2,
        }])
    }
    async fn send_raw_transaction(&self, bytes: &[u8]) -> PortResult<B256> {
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&self.journal_path).unwrap()).unwrap();
        assert_eq!(
            saved["record"]["raw_signed_tx"],
            serde_json::json!(alloy_primitives::Bytes::copy_from_slice(bytes))
        );
        self.sent.lock().unwrap().push(bytes.to_vec());
        if self.nonce_error {
            return Err(LifecyclePortError::NonceTooLow {
                source: std::io::Error::other("nonce"),
            });
        }
        Ok(if self.wrong_hash {
            B256::ZERO
        } else {
            keccak256(bytes)
        })
    }
    async fn finalized_receipt(
        &self,
        _: LifecycleCheckpointV1,
        _: B256,
    ) -> PortResult<Option<LifecycleReceiptV1>> {
        Ok(self.receipt)
    }
    async fn onboarding_proof(
        &self,
        _: LifecycleCheckpointV1,
        _: B256,
    ) -> PortResult<LifecycleOnboardingProofV1> {
        Err(LifecyclePortError::Source(std::io::Error::other("unused")))
    }
    async fn upgrade_key(
        &self,
        _: LifecycleCheckpointV1,
        _: &crate::dcap_protocol::DcapOnboardingContextV1,
        _: &crate::upgrade_transfer::UpgradeKeyProofV1,
    ) -> PortResult<LifecycleOnboardingProofV1> {
        self.upgrade_reply
            .clone()
            .ok_or_else(|| LifecyclePortError::Source(std::io::Error::other("unused")))
    }
}
