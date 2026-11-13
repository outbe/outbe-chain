//! Canonical `TeeRegistry` selectors and finalized-block state readers.

use core::{convert::Infallible, future::Future};

use alloy_primitives::{hex::FromHexError, Address, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    errors::TeeRenewalScheduleError,
    tee_attestation_v1::{AttestationMode, CodecError, TeePolicyV1},
    tee_operator_v1::TeeRenewalScheduleV1,
    tee_registry_abi_v1::{ITeeRegistryV1, NodeEnclaveBindingV1View},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::FinalizedRegistryViewV1;

#[cfg(test)]
mod abi_tests;

/// Chain identity used when validating policy anchors and signing relay bytes.
pub trait ChainRpc {
    type Error: std::error::Error + Send + Sync + 'static;

    fn chain_id(&self) -> impl Future<Output = Result<u64, Self::Error>> + Send;
}

/// Reads pinned to an exact finalized block tag.
pub trait FinalizedStateRpc: ChainRpc {
    fn finalized_block(
        &self,
    ) -> impl Future<Output = Result<serde_json::Value, Self::Error>> + Send;

    fn call_at(
        &self,
        to: Address,
        data: &[u8],
        block_tag: &str,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send;
}

/// Read-only Registry facts. No transaction preparation or relay capability.
pub trait RegistryRpc: FinalizedStateRpc {
    fn tee_renewal_schedule_v1(
        &self,
    ) -> impl Future<Output = Result<TeeRenewalScheduleV1, Self::Error>> + Send;
}

/// Typed errors from finalized Registry policy and binding reads.
#[derive(Debug, Error)]
pub enum RegistryError<E = Infallible> {
    /// Transport or RPC read failed.
    #[error("{context}: {source}")]
    Rpc {
        context: &'static str,
        #[source]
        source: E,
    },

    /// Finalized schedule validation failed.
    #[error(transparent)]
    InvalidRenewalSchedule(#[from] TeeRenewalScheduleError),

    /// ABI return decoding failed.
    #[error("{context}: {source}")]
    AbiDecode {
        context: &'static str,
        #[source]
        source: alloy_sol_types::Error,
    },

    /// Hex `u64` field in finalized block JSON failed to parse.
    #[error("parse finalized block {field}: {source}")]
    ParseFinalizedBlockU64 {
        field: &'static str,
        #[source]
        source: std::num::ParseIntError,
    },

    /// Hex `B256` field in finalized block JSON failed to parse.
    #[error("parse finalized block {field}: {source}")]
    ParseFinalizedBlockHash {
        field: &'static str,
        #[source]
        source: FromHexError,
    },

    /// The finalized Registry has no binding for this node.
    #[error("finalized Registry has no enclave binding for this node")]
    MissingEnclaveBinding,

    /// The finalized block JSON omits a required field.
    #[error("finalized block has no {0}")]
    FinalizedBlockMissingField(&'static str),

    /// An empty staged policy view contains non-empty anchors.
    #[error("empty finalized staged policy view has non-empty anchors")]
    EmptyStagedPolicyNonEmptyAnchors,

    /// The decoder or hasher rejected the active successor policy.
    #[error("invalid active successor: {0}")]
    InvalidActiveSuccessor(#[source] CodecError),

    /// The active upgrade anchors disagree with the active policy.
    #[error("activated enclave upgrade anchors disagree")]
    ActivatedUpgradeAnchorsDisagree,

    /// The staged policy view omits the proposal id or bytes.
    #[error("finalized staged policy view is incomplete")]
    IncompleteStagedPolicyView,

    /// The decoder rejected the finalized staged policy.
    #[error("finalized staged TEE policy is non-canonical: {0}")]
    NonCanonicalStagedPolicy(#[source] CodecError),

    /// The staged policy chain id disagrees with `eth_chainId`.
    #[error("finalized staged TEE policy chain id does not match eth_chainId")]
    StagedPolicyChainIdMismatch,

    /// The hasher rejected the active policy.
    #[error("invalid policy: {0}")]
    InvalidPolicy(#[source] CodecError),

    /// The binding and staged policy use different finalized heads.
    #[error("renewal binding and staged policy use different finalized heads")]
    RenewalStagedHeadMismatch,

    /// The hasher rejected the staged policy.
    #[error("invalid staged policy: {0}")]
    InvalidStagedPolicy(#[source] CodecError),

    /// The network does not use DCAP mode.
    #[error("DCAP renewal is disabled for non-DcapRequired networks")]
    DcapRenewalDisabled,

    /// The renewal schedule disagrees with the finalized block.
    #[error("finalized schedule and eth finalized block disagree")]
    FinalizedScheduleDisagreesWithBlock,

    /// The decoder rejected the finalized active policy.
    #[error("finalized TEE policy is non-canonical: {0}")]
    NonCanonicalPolicy(#[source] CodecError),

    /// The active policy chain id disagrees with `eth_chainId`.
    #[error("finalized TEE policy chain id does not match eth_chainId")]
    PolicyChainIdMismatch,

    /// The finalized Registry returned a zero threshold network encryption key.
    #[error("finalized Registry network encryption public key is zero")]
    ZeroNetworkEncryptionPublicKey,

    /// An unexpected Registry selector was called in tests.
    #[cfg(test)]
    #[error("unexpected Registry call")]
    UnexpectedRegistryCall,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeBindingSelectorV1 {
    NodeHost([u8; 33]),
    Validator(Address),
}

impl NodeBindingSelectorV1 {
    #[must_use]
    pub fn binding_call(&self) -> Vec<u8> {
        match self {
            Self::NodeHost(public) => ITeeRegistryV1::nodeHostEnclaveBindingCall {
                rethP2pPrefix: public[0],
                rethP2pX: B256::from_slice(&public[1..]),
            }
            .abi_encode(),
            Self::Validator(validator) => ITeeRegistryV1::validatorEnclaveBindingCall {
                validator: *validator,
            }
            .abi_encode(),
        }
    }

    pub fn decode_binding(
        &self,
        encoded: &[u8],
    ) -> alloy_sol_types::Result<NodeEnclaveBindingV1View> {
        match self {
            Self::NodeHost(_) => {
                ITeeRegistryV1::nodeHostEnclaveBindingCall::abi_decode_returns(encoded)
            }
            Self::Validator(_) => {
                ITeeRegistryV1::validatorEnclaveBindingCall::abi_decode_returns(encoded)
            }
        }
    }
}

outbe_primitives::define_tee_registry_binding_v1! {
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenewalBindingV1
}

outbe_primitives::impl_tee_registry_binding_v1_mapping!(RenewalBindingV1, decode_fields);

impl TryFrom<NodeEnclaveBindingV1View> for RenewalBindingV1 {
    type Error = RegistryError;

    fn try_from(value: NodeEnclaveBindingV1View) -> Result<Self, Self::Error> {
        if !value.exists {
            return Err(RegistryError::MissingEnclaveBinding);
        }
        Ok(Self::from_registry_binding_fields_v1(&value))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedRenewalChainViewV1 {
    pub schedule: TeeRenewalScheduleV1,
    pub policy: TeePolicyV1,
    pub binding: RenewalBindingV1,
    pub network_encryption_public_key: B256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedRegistryChainViewV1 {
    pub schedule: TeeRenewalScheduleV1,
    pub view: FinalizedRegistryViewV1,
    pub policy: TeePolicyV1,
    pub binding: Option<RenewalBindingV1>,
    pub network_encryption_public_key: B256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedStagedSuccessorPolicyV1 {
    pub finalized_height: u64,
    pub finalized_hash: B256,
    pub proposal_id: U256,
    pub policy: TeePolicyV1,
}

/// Read the staged successor only at the RPC node's exact finalized block.
/// Latest/pending state is deliberately never used for an upgrade decision.
pub async fn read_finalized_staged_successor_policy_v1<R: FinalizedStateRpc + Sync>(
    rpc: &R,
) -> Result<Option<FinalizedStagedSuccessorPolicyV1>, RegistryError<R::Error>> {
    read_finalized_upgrade_policy_inner_v1(rpc, false).await
}

/// Includes the activated strict successor, allowing a missed deadline to be repaired.
pub async fn read_finalized_upgrade_policy_v1<R: FinalizedStateRpc + Sync>(
    rpc: &R,
) -> Result<Option<FinalizedStagedSuccessorPolicyV1>, RegistryError<R::Error>> {
    read_finalized_upgrade_policy_inner_v1(rpc, true).await
}

async fn read_finalized_upgrade_policy_inner_v1<R: FinalizedStateRpc + Sync>(
    rpc: &R,
    allow_activated: bool,
) -> Result<Option<FinalizedStagedSuccessorPolicyV1>, RegistryError<R::Error>> {
    let finalized = rpc
        .finalized_block()
        .await
        .map_err(|source| RegistryError::Rpc {
            context: "read finalized block for staged TEE policy",
            source,
        })?;
    let finalized_height = json_hex_u64(&finalized, "number")?;
    let finalized_hash = json_b256(&finalized, "hash")?;
    let tag = format!("0x{finalized_height:x}");
    let encoded = rpc
        .call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::stagedSuccessorPolicyV1Call {}.abi_encode(),
            &tag,
        )
        .await
        .map_err(|source| RegistryError::Rpc {
            context: "read finalized staged successor TEE policy",
            source,
        })?;
    let mut view = ITeeRegistryV1::stagedSuccessorPolicyV1Call::abi_decode_returns(&encoded)
        .map_err(|source| RegistryError::AbiDecode {
            context: "decode finalized staged successor TEE policy",
            source,
        })?;
    if !view.exists {
        if !view.proposalId.is_zero() || !view.policy.is_empty() {
            return Err(RegistryError::EmptyStagedPolicyNonEmptyAnchors);
        }
        let Some((proposal_id, policy_bytes)) =
            read_activated_upgrade_policy_v1(rpc, &tag, finalized_height, allow_activated).await?
        else {
            return Ok(None);
        };
        view.exists = true;
        view.proposalId = proposal_id;
        view.policy = policy_bytes;
    }
    if view.proposalId.is_zero() || view.policy.is_empty() {
        return Err(RegistryError::IncompleteStagedPolicyView);
    }
    let policy = TeePolicyV1::decode_canonical(&view.policy)
        .map_err(RegistryError::NonCanonicalStagedPolicy)?;
    let rpc_chain_id = rpc.chain_id().await.map_err(|source| RegistryError::Rpc {
        context: "read eth_chainId",
        source,
    })?;
    if policy.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        return Err(RegistryError::StagedPolicyChainIdMismatch);
    }
    Ok(Some(FinalizedStagedSuccessorPolicyV1 {
        finalized_height,
        finalized_hash,
        proposal_id: view.proposalId,
        policy,
    }))
}

async fn read_activated_upgrade_policy_v1<R: FinalizedStateRpc + Sync>(
    rpc: &R,
    tag: &str,
    finalized_height: u64,
    allow_activated: bool,
) -> Result<Option<(U256, alloy_primitives::Bytes)>, RegistryError<R::Error>> {
    if !allow_activated {
        return Ok(None);
    }
    let encoded = rpc
        .call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::enclaveUpgradeV1Call {}.abi_encode(),
            tag,
        )
        .await
        .map_err(|source| RegistryError::Rpc {
            context: "read finalized enclave upgrade state",
            source,
        })?;
    let upgrade =
        ITeeRegistryV1::enclaveUpgradeV1Call::abi_decode_returns(&encoded).map_err(|source| {
            RegistryError::AbiDecode {
                context: "decode finalized enclave upgrade state",
                source,
            }
        })?;
    if upgrade.proposalId.is_zero() || finalized_height < upgrade.activationHeight {
        return Ok(None);
    }
    let encoded = rpc
        .call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::activePolicyV1Call {}.abi_encode(),
            tag,
        )
        .await
        .map_err(|source| RegistryError::Rpc {
            context: "read finalized active policy for activated upgrade",
            source,
        })?;
    let bytes =
        ITeeRegistryV1::activePolicyV1Call::abi_decode_returns(&encoded).map_err(|source| {
            RegistryError::AbiDecode {
                context: "decode finalized active policy for activated upgrade",
                source,
            }
        })?;
    let active =
        TeePolicyV1::decode_canonical(&bytes).map_err(RegistryError::InvalidActiveSuccessor)?;
    if active
        .policy_hash()
        .map_err(RegistryError::InvalidActiveSuccessor)?
        != upgrade.successorPolicyHash
        || active.activation_height != upgrade.activationHeight
    {
        return Err(RegistryError::ActivatedUpgradeAnchorsDisagree);
    }
    Ok(Some((upgrade.proposalId, bytes)))
}

pub async fn read_finalized_renewal_view_v1<R: RegistryRpc + Sync>(
    rpc: &R,
    selector: &NodeBindingSelectorV1,
) -> Result<FinalizedRenewalChainViewV1, RegistryError<R::Error>> {
    let view = read_finalized_registry_view_v1(rpc, selector).await?;
    require_dcap_renewal_mode_v1(view.policy.attestation_mode)?;
    resolve_bound_renewal_policy_v1(rpc, view).await
}

/// Read the exact finalized policy and required binding for the shared manual
/// renewal lifecycle, independent of the genesis-selected evidence mode.
pub async fn read_finalized_bound_renewal_view_v1<R: RegistryRpc + Sync>(
    rpc: &R,
    selector: &NodeBindingSelectorV1,
) -> Result<FinalizedRenewalChainViewV1, RegistryError<R::Error>> {
    let view = read_finalized_registry_view_v1(rpc, selector).await?;
    resolve_bound_renewal_policy_v1(rpc, view).await
}

async fn resolve_bound_renewal_policy_v1<R: RegistryRpc + Sync>(
    rpc: &R,
    mut view: FinalizedRegistryChainViewV1,
) -> Result<FinalizedRenewalChainViewV1, RegistryError<R::Error>> {
    let active_hash = view
        .policy
        .policy_hash()
        .map_err(RegistryError::InvalidPolicy)?;
    if view
        .binding
        .as_ref()
        .is_some_and(|binding| binding.policy_hash != active_hash)
    {
        if let Some(staged) = read_finalized_staged_successor_policy_v1(rpc).await? {
            if staged.finalized_hash != view.schedule.finalized_hash {
                return Err(RegistryError::RenewalStagedHeadMismatch);
            }
            let hash = staged
                .policy
                .policy_hash()
                .map_err(RegistryError::InvalidStagedPolicy)?;
            if view
                .binding
                .as_ref()
                .is_some_and(|binding| binding.policy_hash == hash)
            {
                view.policy = staged.policy;
            }
        }
    }
    require_bound_renewal_view_v1(view)
}

fn require_bound_renewal_view_v1<E>(
    view: FinalizedRegistryChainViewV1,
) -> Result<FinalizedRenewalChainViewV1, RegistryError<E>> {
    let binding = view.binding.ok_or(RegistryError::MissingEnclaveBinding)?;
    Ok(FinalizedRenewalChainViewV1 {
        schedule: view.schedule,
        policy: view.policy,
        binding,
        network_encryption_public_key: view.network_encryption_public_key,
    })
}

fn require_dcap_renewal_mode_v1<E>(mode: AttestationMode) -> Result<(), RegistryError<E>> {
    if mode != AttestationMode::DcapRequired {
        return Err(RegistryError::DcapRenewalDisabled);
    }
    Ok(())
}

/// Read the exact finalized policy and optional node binding used by initial
/// join, live-lease rejection, and expired rejoin recovery.
pub async fn read_finalized_registry_view_v1<R: RegistryRpc + Sync>(
    rpc: &R,
    selector: &NodeBindingSelectorV1,
) -> Result<FinalizedRegistryChainViewV1, RegistryError<R::Error>> {
    let schedule = rpc
        .tee_renewal_schedule_v1()
        .await
        .map_err(|source| RegistryError::Rpc {
            context: "read exact finalized TEE renewal schedule",
            source,
        })?
        .validate()?;
    let finalized = rpc
        .finalized_block()
        .await
        .map_err(|source| RegistryError::Rpc {
            context: "read finalized block",
            source,
        })?;
    let number = json_hex_u64(&finalized, "number")?;
    let timestamp = json_hex_u64(&finalized, "timestamp")?;
    let hash = json_b256(&finalized, "hash")?;
    let state_root = json_b256(&finalized, "stateRoot")?;
    if number != schedule.finalized_height
        || timestamp != schedule.finalized_timestamp
        || hash != schedule.finalized_hash
    {
        return Err(RegistryError::FinalizedScheduleDisagreesWithBlock);
    }
    let tag = format!("0x{number:x}");
    let policy_bytes = rpc
        .call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::activePolicyV1Call {}.abi_encode(),
            &tag,
        )
        .await
        .map_err(|source| RegistryError::Rpc {
            context: "read finalized active TEE policy",
            source,
        })?;
    let canonical = ITeeRegistryV1::activePolicyV1Call::abi_decode_returns(&policy_bytes).map_err(
        |source| RegistryError::AbiDecode {
            context: "decode finalized active TEE policy",
            source,
        },
    )?;
    let policy =
        TeePolicyV1::decode_canonical(&canonical).map_err(RegistryError::NonCanonicalPolicy)?;
    let rpc_chain_id = rpc.chain_id().await.map_err(|source| RegistryError::Rpc {
        context: "read eth_chainId",
        source,
    })?;
    if policy.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        return Err(RegistryError::PolicyChainIdMismatch);
    }
    let binding_bytes = rpc
        .call_at(TEE_REGISTRY_ADDRESS, &selector.binding_call(), &tag)
        .await
        .map_err(|source| RegistryError::Rpc {
            context: "read finalized enclave binding",
            source,
        })?;
    let binding_view =
        selector
            .decode_binding(&binding_bytes)
            .map_err(|source| RegistryError::AbiDecode {
                context: "decode finalized enclave binding",
                source,
            })?;
    let binding = if binding_view.exists {
        Some(RenewalBindingV1::from_registry_binding_fields_v1(
            &binding_view,
        ))
    } else {
        None
    };
    // `ITeeRegistryV1.tributeOfferPublicKey()` (selector `0x1b640a92`) is the
    // canonical V1 Solidity ABI getter for the threshold network encryption key.
    let key_bytes = rpc
        .call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::tributeOfferPublicKeyCall {}.abi_encode(),
            &tag,
        )
        .await
        .map_err(|source| RegistryError::Rpc {
            context: "read finalized network encryption public key",
            source,
        })?;
    let key_word = ITeeRegistryV1::tributeOfferPublicKeyCall::abi_decode_returns(&key_bytes)
        .map_err(|source| RegistryError::AbiDecode {
            context: "decode finalized network encryption public key",
            source,
        })?;
    let network_encryption_public_key = B256::from(key_word.to_be_bytes::<32>());
    if network_encryption_public_key.is_zero() {
        return Err(RegistryError::ZeroNetworkEncryptionPublicKey);
    }
    Ok(FinalizedRegistryChainViewV1 {
        schedule,
        view: FinalizedRegistryViewV1 {
            chain_id: policy.chain_id,
            genesis_hash: policy.genesis_hash,
            block_number: number,
            block_hash: hash,
            state_root,
            consensus_timestamp: timestamp,
        },
        policy,
        binding,
        network_encryption_public_key,
    })
}

fn json_hex_u64<E>(
    value: &serde_json::Value,
    field: &'static str,
) -> Result<u64, RegistryError<E>> {
    let encoded = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(RegistryError::FinalizedBlockMissingField(field))?;
    u64::from_str_radix(encoded.strip_prefix("0x").unwrap_or(encoded), 16)
        .map_err(|source| RegistryError::ParseFinalizedBlockU64 { field, source })
}

fn json_b256<E>(value: &serde_json::Value, field: &'static str) -> Result<B256, RegistryError<E>> {
    let encoded = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or(RegistryError::FinalizedBlockMissingField(field))?;
    encoded
        .parse::<B256>()
        .map_err(|source| RegistryError::ParseFinalizedBlockHash { field, source })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedOnboardingBindingV1 {
    pub selector: NodeBindingSelectorV1,
    pub chain_id: [u8; 32],
    pub genesis_hash: B256,
    pub node_id_hash: B256,
    pub enclave_id: B256,
    pub intent_hash: B256,
    pub recipient_x25519: [u8; 32],
    pub network_encryption_public_key: [u8; 32],
    pub key_epoch: u64,
    pub tribute_offer_epoch: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloy_primitives::Bytes;
    use outbe_primitives::{
        chain::DEVNET_CHAIN_ID,
        tee_genesis_v1::{initial_tee_policy_v1, InitialTeeProfileV1, ProductionSgxMeasurementV1},
        tee_operator_v1::TeeRenewalScheduleV1,
    };

    #[derive(Clone)]
    struct MockRegistryRpc {
        policy: TeePolicyV1,
        staged: Option<TeePolicyV1>,
        binding: NodeEnclaveBindingV1View,
        schedule: TeeRenewalScheduleV1,
    }

    impl ChainRpc for MockRegistryRpc {
        type Error = RegistryError;

        async fn chain_id(&self) -> Result<u64, Self::Error> {
            Ok(DEVNET_CHAIN_ID)
        }
    }

    impl FinalizedStateRpc for MockRegistryRpc {
        async fn finalized_block(&self) -> Result<serde_json::Value, Self::Error> {
            Ok(serde_json::json!({
                "number": format!("0x{:x}", self.schedule.finalized_height),
                "timestamp": format!("0x{:x}", self.schedule.finalized_timestamp),
                "hash": format!("{:#x}", self.schedule.finalized_hash),
                "stateRoot": format!("{:#x}", B256::repeat_byte(0x71)),
            }))
        }

        async fn call_at(
            &self,
            _to: Address,
            data: &[u8],
            _block_tag: &str,
        ) -> Result<Vec<u8>, Self::Error> {
            if data.starts_with(&ITeeRegistryV1::activePolicyV1Call::SELECTOR) {
                let policy = Bytes::from(self.policy.encode_canonical().unwrap());
                return Ok(ITeeRegistryV1::activePolicyV1Call::abi_encode_returns(
                    &policy,
                ));
            }
            if data.starts_with(&ITeeRegistryV1::stagedSuccessorPolicyV1Call::SELECTOR) {
                use alloy_sol_types::SolValue;
                let value = self
                    .staged
                    .as_ref()
                    .map(|p| {
                        (
                            true,
                            U256::from(7),
                            Bytes::from(p.encode_canonical().unwrap()),
                        )
                    })
                    .unwrap_or((false, U256::ZERO, Bytes::new()));
                return Ok(value.abi_encode_params());
            }
            if data.starts_with(&ITeeRegistryV1::nodeHostEnclaveBindingCall::SELECTOR) {
                return Ok(
                    ITeeRegistryV1::nodeHostEnclaveBindingCall::abi_encode_returns(&self.binding),
                );
            }
            if data.starts_with(&ITeeRegistryV1::tributeOfferPublicKeyCall::SELECTOR) {
                return Ok(
                    ITeeRegistryV1::tributeOfferPublicKeyCall::abi_encode_returns(&U256::from(7)),
                );
            }
            Err(RegistryError::UnexpectedRegistryCall)
        }
    }

    impl RegistryRpc for MockRegistryRpc {
        async fn tee_renewal_schedule_v1(&self) -> Result<TeeRenewalScheduleV1, Self::Error> {
            Ok(self.schedule)
        }
    }

    fn registry_rpc(binding_exists: bool, mode: AttestationMode) -> MockRegistryRpc {
        let finalized_hash = B256::repeat_byte(0x70);
        let profile = match mode {
            AttestationMode::DcapRequired => {
                InitialTeeProfileV1::DcapRequired(ProductionSgxMeasurementV1 {
                    mrenclave: B256::repeat_byte(0x61),
                    mrsigner: B256::repeat_byte(0x62),
                    isv_prod_id: 1,
                    minimum_isv_svn: 1,
                    minimum_tcb_evaluation_data_number: 1,
                })
            }
            AttestationMode::GramineDirectDev => InitialTeeProfileV1::GramineDirectDev,
        };
        let policy =
            initial_tee_policy_v1(profile, DEVNET_CHAIN_ID, B256::repeat_byte(0x72)).unwrap();
        MockRegistryRpc {
            policy: policy.clone(),
            staged: None,
            binding: NodeEnclaveBindingV1View {
                exists: binding_exists,
                nodeIdHash: B256::repeat_byte(0x73),
                enclaveId: B256::repeat_byte(0x74),
                bindingId: B256::repeat_byte(0x75),
                intentHash: B256::repeat_byte(0x76),
                evidenceHash: B256::repeat_byte(0x77),
                policyHash: policy.policy_hash().unwrap(),
                bindingVersion: 1,
                registrationVersion: 1,
                renewalNonce: 1,
                transitionNonce: 0,
                leaseStartedAt: 100,
                validUntil: 200,
                collateralValidUntil: u64::MAX,
                recipientX25519: B256::repeat_byte(0x79),
                attestationEd25519: B256::repeat_byte(0x7a),
                noiseResponderX25519: B256::repeat_byte(0x7b),
                mrenclave: B256::ZERO,
                mrsigner: B256::ZERO,
                isvProdId: 0,
                isvSvn: 0,
                platformTcbStatus: 0,
                verdictHash: B256::ZERO,
                nodeHostAuthorizationHash: B256::repeat_byte(0x7c),
            },
            schedule: TeeRenewalScheduleV1 {
                finalized_height: 120,
                finalized_hash,
                finalized_timestamp: 1_000,
                epoch_number: 2,
                epoch_start_height: 100,
                epoch_length_blocks: 100,
                next_freeze_height: 180,
                planned_activation_height: 200,
                dkg_prepare_window_blocks: 20,
                minimum_block_time_millis: 2_000,
            },
        }
    }

    #[tokio::test]
    async fn renewal_of_early_upgraded_node_selects_its_exact_staged_policy() {
        let mut rpc = registry_rpc(true, AttestationMode::GramineDirectDev);
        let mut successor = rpc.policy.clone();
        successor.policy_version += 1;
        successor.predecessor_policy_hash = rpc.policy.policy_hash().unwrap();
        successor.activation_height = 500;
        successor.measurement_rules[0].mrenclave = B256::repeat_byte(0xe1);
        rpc.binding.policyHash = successor.policy_hash().unwrap();
        rpc.staged = Some(successor.clone());
        let view =
            read_finalized_bound_renewal_view_v1(&rpc, &NodeBindingSelectorV1::NodeHost([2; 33]))
                .await
                .unwrap();
        assert_eq!(view.policy, successor);
        assert_eq!(view.schedule.finalized_height, 120);
    }

    #[test]
    fn finalized_join_selectors_cover_node_host_and_evm_association() {
        let node = NodeBindingSelectorV1::NodeHost([2; 33]).binding_call();
        let validator = NodeBindingSelectorV1::Validator(Address::repeat_byte(3)).binding_call();
        assert_eq!(
            &node[..4],
            ITeeRegistryV1::nodeHostEnclaveBindingCall::SELECTOR
        );
        assert_eq!(
            &validator[..4],
            ITeeRegistryV1::validatorEnclaveBindingCall::SELECTOR
        );
        assert_ne!(node, validator);
    }

    #[tokio::test]
    async fn public_readers_keep_mode_neutral_binding_and_legacy_dcap_error_order() {
        let selector = NodeBindingSelectorV1::NodeHost([2; 33]);
        let bound = read_finalized_bound_renewal_view_v1(
            &registry_rpc(true, AttestationMode::GramineDirectDev),
            &selector,
        )
        .await
        .unwrap();
        assert_eq!(
            bound.policy.attestation_mode,
            AttestationMode::GramineDirectDev
        );
        assert_eq!(bound.binding.binding_id, B256::repeat_byte(0x75));

        let legacy_error = read_finalized_renewal_view_v1(
            &registry_rpc(false, AttestationMode::GramineDirectDev),
            &selector,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(legacy_error.contains("DCAP renewal is disabled"));

        let neutral_error = read_finalized_bound_renewal_view_v1(
            &registry_rpc(false, AttestationMode::GramineDirectDev),
            &selector,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(neutral_error.contains("no enclave binding"));

        let dcap_rpc = registry_rpc(true, AttestationMode::DcapRequired);
        assert_eq!(
            read_finalized_bound_renewal_view_v1(&dcap_rpc, &selector)
                .await
                .unwrap()
                .policy
                .attestation_mode,
            AttestationMode::DcapRequired
        );
        assert_eq!(
            read_finalized_renewal_view_v1(&dcap_rpc, &selector)
                .await
                .unwrap()
                .policy
                .attestation_mode,
            AttestationMode::DcapRequired
        );
    }
}
