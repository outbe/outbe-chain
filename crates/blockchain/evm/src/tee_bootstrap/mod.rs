//! Block 1 (`GenesisBootstrap`) TEE / DCAP bootstrap lifecycle module.
//!
//! Consolidates all Block-1 `OST3` (`TeeBootstrapV2`) validation, proposer
//! attestation key extraction for the Block-1 execution witness, and EVM
//! `TeeRegistry` bootstrap execution behind a single module boundary.

use alloy_consensus::Transaction as _;
use alloy_primitives::Address;
use outbe_primitives::{
    block::{BlockHeightRegime, BlockRuntimeContext},
    error::Result,
    system_tx::SystemTx,
    tee_attestation_v1::TeePolicyV1,
    tee_bootstrap_v2::TeeBootstrapV2,
};
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::Recovered;

use crate::{
    errors::{BeginBlockPrecompileError, SystemTxBuildError, TeeBootstrapHostError},
    tee_attestation_activation::TeeAttestationChainSpecStateV1,
};

/// Preflights a `TeeBootstrapV2` payload and verifies its policy against the
/// `ChainSpec` activation schedule when building Block-1 system transactions.
pub fn validate_tee_bootstrap_for_build(
    host: &dyn crate::EvmRuntimeHost,
    block_number: u64,
    payload: &TeeBootstrapV2,
    state: &TeeAttestationChainSpecStateV1,
) -> std::result::Result<(), SystemTxBuildError> {
    payload
        .preflight()
        .map_err(SystemTxBuildError::invalid_block1_ost3_bootstrap_payload)?;
    let activation = state
        .activation()
        .map_err(SystemTxBuildError::invalid_mandatory_tee_chain_spec_authority)?;
    let expected_policy = activation
        .policy_at(block_number)
        .map_err(SystemTxBuildError::from)?;
    match host.validate_tee_bootstrap_for_build(payload, expected_policy) {
        Ok(()) => Ok(()),
        Err(TeeBootstrapHostError::InvalidPayload(reason)) => Err(
            SystemTxBuildError::invalid_block1_ost3_bootstrap_payload(reason),
        ),
        Err(TeeBootstrapHostError::PolicyScheduleMismatch) => {
            Err(SystemTxBuildError::Block1Ost3PolicyScheduleMismatch)
        }
        Err(TeeBootstrapHostError::HostNotInstalled) => {
            Err(SystemTxBuildError::RuntimeHostNotInstalled)
        }
    }
}

/// Extracts the Block-1 proposer's Ed25519 enclave attestation public key from
/// the `TeeBootstrapV2` payload before `TeeBootstrap` commits to `TeeRegistry`.
pub fn extract_block_one_proposer_attestation_key(
    regime: BlockHeightRegime,
    pending_tee_bootstrap: Option<&TeeBootstrapV2>,
    expected_begin_system_txs: &[Recovered<TransactionSigned>],
    proposer: Address,
) -> Option<[u8; 32]> {
    if !regime.is_genesis_bootstrap() {
        return None;
    }
    if let Some(payload) = pending_tee_bootstrap {
        if let Some(key) = attestation_for_proposer(payload, proposer) {
            return Some(key);
        }
    }
    for tx in expected_begin_system_txs {
        let Ok(SystemTx::TeeBootstrap { payload }) = SystemTx::decode(tx.input().as_ref()) else {
            continue;
        };
        if let Some(key) = attestation_for_proposer(&payload, proposer) {
            return Some(key);
        }
    }
    None
}

/// Returns the Ed25519 attestation public key for `proposer` inside `payload`.
pub fn attestation_for_proposer(payload: &TeeBootstrapV2, proposer: Address) -> Option<[u8; 32]> {
    payload
        .participants
        .iter()
        .find(|participant| Address::from(participant.validator_binding.validator) == proposer)
        .map(|participant| participant.intent.attestation_ed25519)
}

/// Installs the initial authoritative TEE policy and executes the Block-1 DCAP bootstrap.
pub fn execute_tee_bootstrap(
    host: &dyn crate::EvmRuntimeHost,
    ctx: &BlockRuntimeContext,
    payload: &TeeBootstrapV2,
    state: &TeeAttestationChainSpecStateV1,
) -> Result<()> {
    let expected_policy = resolve_expected_policy(ctx.block.block_number, payload, state)?;
    host.install_initial_tee_policy(ctx.storage.clone(), &expected_policy)?;
    host.run_tee_bootstrap(ctx, payload, &expected_policy)
}

/// Validates the `ChainSpec` TEE activation schedule and installs the initial policy.
pub fn prepare_tee_bootstrap(
    host: &dyn crate::EvmRuntimeHost,
    ctx: &BlockRuntimeContext,
    payload: &TeeBootstrapV2,
    state: &TeeAttestationChainSpecStateV1,
) -> Result<()> {
    let expected_policy = resolve_expected_policy(ctx.block.block_number, payload, state)?;
    host.install_initial_tee_policy(ctx.storage.clone(), &expected_policy)
}

fn resolve_expected_policy(
    block_number: u64,
    payload: &TeeBootstrapV2,
    state: &TeeAttestationChainSpecStateV1,
) -> Result<TeePolicyV1> {
    let activation = state
        .activation()
        .map_err(BeginBlockPrecompileError::invalid_tee_chain_spec_authority)?;
    if block_number < activation.manifest.activation_height {
        return Err(BeginBlockPrecompileError::Ost3NotActive.into());
    }
    let expected_policy = activation
        .policy_at(block_number)
        .map_err(BeginBlockPrecompileError::invalid_active_tee_policy)?;
    if &payload.authority.policy != expected_policy {
        return Err(BeginBlockPrecompileError::Ost3PolicyScheduleMismatch.into());
    }
    Ok(expected_policy.clone())
}
