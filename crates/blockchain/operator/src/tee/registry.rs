//! Canonical Registry selectors and exact onboarding expectations.

use alloy_primitives::{B256, U256};
use alloy_sol_types::SolCall;
use eyre::{Result, WrapErr as _};
use outbe_primitives::{
    addresses::TEE_REGISTRY_ADDRESS,
    tee_attestation_v1::{AttestationMode, TeePolicyV1},
    tee_operator_v1::TeeRenewalScheduleV1,
    tee_registry_abi_v1::{ITeeRegistryV1, NodeEnclaveBindingV1View},
};
use serde::{Deserialize, Serialize};

use crate::rpc::RenewalRpc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeBindingSelectorV1 {
    NodeHost([u8; 33]),
}

impl NodeBindingSelectorV1 {
    pub fn binding_call(&self) -> Vec<u8> {
        match self {
            Self::NodeHost(public) => ITeeRegistryV1::nodeHostEnclaveBindingCall {
                rethP2pPrefix: public[0],
                rethP2pX: B256::from_slice(&public[1..]),
            }
            .abi_encode(),
        }
    }

    pub fn decode_binding(
        &self,
        encoded: &[u8],
    ) -> alloy_sol_types::Result<NodeEnclaveBindingV1View> {
        ITeeRegistryV1::nodeHostEnclaveBindingCall::abi_decode_returns(encoded)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenewalBindingV1 {
    pub node_id_hash: B256,
    pub enclave_id: B256,
    pub binding_id: B256,
    pub intent_hash: B256,
    pub evidence_hash: B256,
    pub policy_hash: B256,
    pub binding_version: u64,
    pub registration_version: u64,
    pub renewal_nonce: u64,
    pub transition_nonce: u64,
    pub lease_started_at: u64,
    pub valid_until: u64,
    pub collateral_valid_until: u64,
    pub recipient_x25519: B256,
    pub attestation_ed25519: B256,
    pub noise_responder_x25519: B256,
    pub mrenclave: B256,
    pub mrsigner: B256,
    pub isv_prod_id: u16,
    pub isv_svn: u16,
    pub platform_tcb_status: u8,
    pub verdict_hash: B256,
    pub node_host_authorization_hash: B256,
}

impl TryFrom<NodeEnclaveBindingV1View> for RenewalBindingV1 {
    type Error = eyre::Report;

    fn try_from(value: NodeEnclaveBindingV1View) -> Result<Self> {
        if !value.exists {
            eyre::bail!("finalized Registry has no enclave binding for this node");
        }
        Ok(Self {
            node_id_hash: value.nodeIdHash,
            enclave_id: value.enclaveId,
            binding_id: value.bindingId,
            intent_hash: value.intentHash,
            evidence_hash: value.evidenceHash,
            policy_hash: value.policyHash,
            binding_version: value.bindingVersion,
            registration_version: value.registrationVersion,
            renewal_nonce: value.renewalNonce,
            transition_nonce: value.transitionNonce,
            lease_started_at: value.leaseStartedAt,
            valid_until: value.validUntil,
            collateral_valid_until: value.collateralValidUntil,
            recipient_x25519: value.recipientX25519,
            attestation_ed25519: value.attestationEd25519,
            noise_responder_x25519: value.noiseResponderX25519,
            mrenclave: value.mrenclave,
            mrsigner: value.mrsigner,
            isv_prod_id: value.isvProdId,
            isv_svn: value.isvSvn,
            platform_tcb_status: value.platformTcbStatus,
            verdict_hash: value.verdictHash,
            node_host_authorization_hash: value.nodeHostAuthorizationHash,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedRenewalChainViewV1 {
    pub schedule: TeeRenewalScheduleV1,
    pub policy: TeePolicyV1,
    pub binding: RenewalBindingV1,
    pub tribute_offer_public: B256,
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
pub async fn read_finalized_staged_successor_policy_v1(
    rpc: &(impl RenewalRpc + Sync),
) -> Result<Option<FinalizedStagedSuccessorPolicyV1>> {
    let finalized = rpc
        .finalized_block()
        .await
        .wrap_err("read finalized block for staged TEE policy")?;
    let finalized_height = json_hex_u64(&finalized, "number")?;
    let finalized_hash = finalized
        .get("hash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("finalized block has no hash"))?
        .parse::<B256>()
        .wrap_err("parse finalized block hash")?;
    let tag = format!("0x{finalized_height:x}");
    let encoded = rpc
        .call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::stagedSuccessorPolicyV1Call {}.abi_encode(),
            &tag,
        )
        .await
        .wrap_err("read finalized staged successor TEE policy")?;
    let view = ITeeRegistryV1::stagedSuccessorPolicyV1Call::abi_decode_returns(&encoded)
        .wrap_err("decode finalized staged successor TEE policy")?;
    if !view.exists {
        if !view.proposalId.is_zero() || !view.policy.is_empty() {
            eyre::bail!("empty finalized staged policy view has non-empty anchors");
        }
        return Ok(None);
    }
    if view.proposalId.is_zero() || view.policy.is_empty() {
        eyre::bail!("finalized staged policy view is incomplete");
    }
    let policy = TeePolicyV1::decode_canonical(&view.policy)
        .map_err(|error| eyre::eyre!("finalized staged TEE policy is non-canonical: {error}"))?;
    let rpc_chain_id = rpc.chain_id().await.wrap_err("read eth_chainId")?;
    if policy.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        eyre::bail!("finalized staged TEE policy chain id does not match eth_chainId");
    }
    Ok(Some(FinalizedStagedSuccessorPolicyV1 {
        finalized_height,
        finalized_hash,
        proposal_id: view.proposalId,
        policy,
    }))
}

pub async fn read_finalized_renewal_view_v1(
    rpc: &(impl RenewalRpc + Sync),
    selector: &NodeBindingSelectorV1,
) -> Result<FinalizedRenewalChainViewV1> {
    let schedule = rpc
        .tee_renewal_schedule_v1()
        .await
        .wrap_err("read exact finalized TEE renewal schedule")?
        .validate()
        .map_err(eyre::Report::msg)?;
    let finalized = rpc
        .finalized_block()
        .await
        .wrap_err("read finalized block")?;
    let number = json_hex_u64(&finalized, "number")?;
    let timestamp = json_hex_u64(&finalized, "timestamp")?;
    let hash = finalized
        .get("hash")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("finalized block has no hash"))?
        .parse::<B256>()
        .wrap_err("parse finalized block hash")?;
    if number != schedule.finalized_height
        || timestamp != schedule.finalized_timestamp
        || hash != schedule.finalized_hash
    {
        eyre::bail!("finalized schedule and eth finalized block disagree");
    }
    let tag = format!("0x{number:x}");
    let policy_bytes = rpc
        .call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::activePolicyV1Call {}.abi_encode(),
            &tag,
        )
        .await
        .wrap_err("read finalized active TEE policy")?;
    let canonical = ITeeRegistryV1::activePolicyV1Call::abi_decode_returns(&policy_bytes)
        .wrap_err("decode finalized active TEE policy")?;
    let policy = TeePolicyV1::decode_canonical(&canonical)
        .map_err(|error| eyre::eyre!("finalized TEE policy is non-canonical: {error}"))?;
    if policy.attestation_mode != AttestationMode::DcapRequired {
        eyre::bail!("DCAP renewal is disabled for non-DcapRequired networks");
    }
    let rpc_chain_id = rpc.chain_id().await.wrap_err("read eth_chainId")?;
    if policy.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        eyre::bail!("finalized TEE policy chain id does not match eth_chainId");
    }
    let binding_bytes = rpc
        .call_at(TEE_REGISTRY_ADDRESS, &selector.binding_call(), &tag)
        .await
        .wrap_err("read finalized enclave binding")?;
    let binding = RenewalBindingV1::try_from(
        selector
            .decode_binding(&binding_bytes)
            .wrap_err("decode finalized enclave binding")?,
    )?;
    let offer_bytes = rpc
        .call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::tributeOfferPublicKeyCall {}.abi_encode(),
            &tag,
        )
        .await
        .wrap_err("read finalized tribute offer public key")?;
    let offer = ITeeRegistryV1::tributeOfferPublicKeyCall::abi_decode_returns(&offer_bytes)
        .wrap_err("decode finalized tribute offer public key")?;
    let tribute_offer_public = B256::from(offer.to_be_bytes::<32>());
    if tribute_offer_public.is_zero() {
        eyre::bail!("finalized Registry tribute offer public key is zero");
    }
    Ok(FinalizedRenewalChainViewV1 {
        schedule,
        policy,
        binding,
        tribute_offer_public,
    })
}

fn json_hex_u64(value: &serde_json::Value, field: &str) -> Result<u64> {
    let encoded = value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("finalized block has no {field}"))?;
    u64::from_str_radix(encoded.strip_prefix("0x").unwrap_or(encoded), 16)
        .wrap_err_with(|| format!("parse finalized block {field}"))
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
    pub tribute_offer_public: [u8; 32],
    pub key_epoch: u64,
    pub tribute_offer_epoch: u64,
}
