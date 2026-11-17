//! Domain-neutral chain I/O. Implementations pin reads to the supplied finalized hash.
use std::future::Future;

use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_primitives::{
    enclave_upgrade::EnclaveUpgradeV1, tee_attestation_v1::TeePolicyV1,
    tee_operator_v1::TeeRenewalScheduleV1, validator_dto::ValidatorRecord,
};
use serde::{Deserialize, Serialize};

use crate::{
    dcap_protocol::DcapOnboardingContextV1,
    registry_view::{FinalizedStagedSuccessorPolicyV1, NodeBindingSelectorV1, RenewalBindingV1},
    upgrade_transfer::UpgradeKeyProofV1,
};

/// Public ValidatorSet ABI status, including validators jailed after registration.
pub const VALIDATOR_STATUS_JAILED: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleCheckpointV1 {
    pub height: u64,
    pub hash: B256,
    pub state_root: B256,
    pub timestamp: u64,
    pub base_fee_per_gas: U256,
    pub chain_id: u64,
    pub genesis_hash: B256,
}

/// Exact pendingEnclaveUpgrade ABI projection; None means no live candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LifecycleUpgradeCandidateV1 {
    pub context_hash: B256,
    pub valid_until: u64,
    pub source_binding_id: B256,
    pub target_hash: B256,
    pub nonce: u64,
}

/// Every field is read from the same checkpoint, never a moving latest tag.
#[derive(Clone, Debug)]
pub struct LifecycleRegistryStateV1 {
    pub checkpoint: LifecycleCheckpointV1,
    pub binding: Option<RenewalBindingV1>,
    pub policy: TeePolicyV1,
    pub schedule: TeeRenewalScheduleV1,
    pub active_upgrade: Option<EnclaveUpgradeV1>,
    pub successor_policy: Option<FinalizedStagedSuccessorPolicyV1>,
    pub candidate: Option<LifecycleUpgradeCandidateV1>,
    pub network_encryption_public_key: B256,
    pub key_epoch: u64,
    pub network_encryption_epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleNonceTagV1 {
    Finalized(LifecycleCheckpointV1),
    Latest,
    Pending,
}

/// The adapter verifies canonical ancestry to the supplied finalized checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleReceiptV1 {
    pub tx_hash: B256,
    pub block_height: u64,
    pub block_hash: B256,
    pub success: bool,
}

/// Ciphertext and genesis-rooted BLS/MPT proof only; never decrypted key material.
#[derive(Clone, Debug)]
pub struct LifecycleOnboardingProofV1 {
    pub artifact: Bytes,
    pub proof: UpgradeKeyProofV1,
}

/// Preserve the adapter's structured source, including classified nonce failures.
#[derive(Debug, thiserror::Error)]
pub enum LifecyclePortError<E: std::error::Error + 'static> {
    #[error("chain I/O failed")]
    Source(#[source] E),
    #[error("account nonce is already consumed")]
    NonceTooLow {
        #[source]
        source: E,
    },
    #[error("transaction was displaced at account nonce {nonce}")]
    TransactionDisplaced {
        nonce: u64,
        #[source]
        source: E,
    },
}

/// Local provider and upstream RPC adapters implement this same finalized contract.
/// No reducer depends on runtime, system contracts, JSON, URLs or an HTTP client.
pub trait LifecycleChainPort: Sync {
    type Error: std::error::Error + Send + Sync + 'static;

    fn finalized_checkpoint(
        &self,
    ) -> impl Future<Output = Result<LifecycleCheckpointV1, LifecyclePortError<Self::Error>>> + Send;

    fn registry_state(
        &self,
        at: LifecycleCheckpointV1,
        selector: &NodeBindingSelectorV1,
    ) -> impl Future<Output = Result<LifecycleRegistryStateV1, LifecyclePortError<Self::Error>>> + Send;

    fn validator_record(
        &self,
        at: LifecycleCheckpointV1,
        validator: Address,
    ) -> impl Future<Output = Result<Option<ValidatorRecord>, LifecyclePortError<Self::Error>>> + Send;

    fn balance(
        &self,
        at: LifecycleCheckpointV1,
        account: Address,
    ) -> impl Future<Output = Result<U256, LifecyclePortError<Self::Error>>> + Send;

    fn account_nonce(
        &self,
        account: Address,
        tag: LifecycleNonceTagV1,
    ) -> impl Future<Output = Result<u64, LifecyclePortError<Self::Error>>> + Send;

    fn send_raw_transaction(
        &self,
        raw_signed_tx: &[u8],
    ) -> impl Future<Output = Result<B256, LifecyclePortError<Self::Error>>> + Send;

    fn finalized_receipt(
        &self,
        at: LifecycleCheckpointV1,
        tx_hash: B256,
    ) -> impl Future<Output = Result<Option<LifecycleReceiptV1>, LifecyclePortError<Self::Error>>> + Send;

    fn onboarding_proof(
        &self,
        at: LifecycleCheckpointV1,
        intent_hash: B256,
    ) -> impl Future<Output = Result<LifecycleOnboardingProofV1, LifecyclePortError<Self::Error>>> + Send;

    fn upgrade_key(
        &self,
        at: LifecycleCheckpointV1,
        context: &DcapOnboardingContextV1,
        proof: &UpgradeKeyProofV1,
    ) -> impl Future<Output = Result<LifecycleOnboardingProofV1, LifecyclePortError<Self::Error>>> + Send;
}
