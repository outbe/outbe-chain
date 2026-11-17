use super::*;
use outbe_primitives::{tee_operator_v1::TeeRenewalScheduleV1, validator_dto::ValidatorRecord};
use outbe_tee::registry_view::{NodeBindingSelectorV1, RenewalBindingV1};
use outbe_tee::{dcap_protocol::DcapOnboardingContextV1, upgrade_transfer::UpgradeKeyProofV1};
use std::sync::Mutex;

pub type PortResult<T> = Result<T, LifecyclePortError<std::io::Error>>;
pub struct Chain {
    pub at: LifecycleCheckpointV1,
    pub binding: Option<RenewalBindingV1>,
    pub policy: TeePolicyV1,
    pub jailed: bool,
    pub strict: bool,
    pub balance: U256,
    pub nonces: [u64; 3],
    pub sent: Mutex<Vec<Vec<u8>>>,
    pub proof: Option<LifecycleOnboardingProofV1>,
    pub public: B256,
    pub fail_nonce: bool,
    pub journal_dir: std::path::PathBuf,
}
impl LifecycleChainPort for Chain {
    type Error = std::io::Error;
    async fn finalized_checkpoint(&self) -> PortResult<LifecycleCheckpointV1> {
        Ok(self.at)
    }
    async fn registry_state(
        &self,
        at: LifecycleCheckpointV1,
        _: &NodeBindingSelectorV1,
    ) -> PortResult<LifecycleRegistryStateV1> {
        Ok(LifecycleRegistryStateV1 {
            checkpoint: at,
            binding: self.binding.clone(),
            policy: self.policy.clone(),
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
            active_upgrade: self.strict.then_some(Default::default()),
            successor_policy: None,
            candidate: None,
            network_encryption_public_key: self.public,
            key_epoch: 0,
            network_encryption_epoch: 0,
        })
    }
    async fn validator_record(
        &self,
        _: LifecycleCheckpointV1,
        _: Address,
    ) -> PortResult<Option<ValidatorRecord>> {
        Ok(self.jailed.then_some(ValidatorRecord {
            validator_address: Address::ZERO,
            consensus_pubkey: [0; 48],
            stake: U256::ZERO,
            status: VALIDATOR_STATUS_JAILED,
            slash_count: 0,
            missed_blocks: 0,
            missed_votes: 0,
            blocks_proposed: 0,
            joined_at_height: 0,
            deactivated_at_height: 0,
            unbonding_end: 0,
            has_bls_share: false,
        }))
    }
    async fn balance(&self, _: LifecycleCheckpointV1, _: Address) -> PortResult<U256> {
        Ok(self.balance)
    }
    async fn account_nonce(&self, _: Address, tag: LifecycleNonceTagV1) -> PortResult<u64> {
        Ok(self.nonces[match tag {
            LifecycleNonceTagV1::Finalized(_) => 0,
            LifecycleNonceTagV1::Latest => 1,
            LifecycleNonceTagV1::Pending => 2,
        }])
    }
    async fn send_raw_transaction(&self, raw: &[u8]) -> PortResult<B256> {
        let path = self.journal_dir.join("tee-service-tx-v1/journal.json");
        let durable: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            durable["record"]["raw_signed_tx"],
            serde_json::to_value(alloy_primitives::Bytes::copy_from_slice(raw)).unwrap()
        );
        self.sent.lock().unwrap().push(raw.to_vec());
        if self.fail_nonce {
            return Err(LifecyclePortError::NonceTooLow {
                source: std::io::Error::other("operator nonce"),
            });
        }
        Ok(alloy_primitives::keccak256(raw))
    }
    async fn finalized_receipt(
        &self,
        _: LifecycleCheckpointV1,
        _: B256,
    ) -> PortResult<Option<LifecycleReceiptV1>> {
        Ok(None)
    }
    async fn onboarding_proof(
        &self,
        _: LifecycleCheckpointV1,
        _: B256,
    ) -> PortResult<LifecycleOnboardingProofV1> {
        self.proof
            .clone()
            .ok_or_else(|| LifecyclePortError::Source(std::io::Error::other("proof unavailable")))
    }
    async fn upgrade_key(
        &self,
        _: LifecycleCheckpointV1,
        _: &DcapOnboardingContextV1,
        _: &UpgradeKeyProofV1,
    ) -> PortResult<LifecycleOnboardingProofV1> {
        Err(LifecyclePortError::Source(std::io::Error::other("unused")))
    }
}

pub struct Signer;
impl Signer {
    fn operator() -> k256::ecdsa::SigningKey {
        k256::ecdsa::SigningKey::from_slice(&[7; 32]).unwrap()
    }
    pub fn address() -> Address {
        Address::from_public_key(Self::operator().verifying_key())
    }
}
impl EnrollmentSignerV1 for Signer {
    type Error = std::convert::Infallible;
    fn sign_operator_hash(&self, hash: B256) -> Result<alloy_primitives::Signature, Self::Error> {
        let (sig, rec) = Self::operator()
            .sign_prehash_recoverable(hash.as_slice())
            .unwrap();
        Ok(alloy_primitives::Signature::from_signature_and_parity(
            sig,
            rec.is_y_odd(),
        ))
    }
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65], Self::Error> {
        Ok(
            outbe_primitives::tee_test_utils::sign_node_host_hash_for_test(
                &k256::ecdsa::SigningKey::from_slice(&[3; 32]).unwrap(),
                hash,
            ),
        )
    }
}
