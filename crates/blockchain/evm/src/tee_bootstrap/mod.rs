//! Block-1 TEE bootstrap attestation-key extraction.
//!
//! Payload construction and validation belong to protocol steps. This module
//! resolves the proposer's enclave key before the bootstrap commits to storage.

use alloy_consensus::Transaction as _;
use alloy_primitives::Address;
use outbe_primitives::{
    block::BlockHeightRegime,
    system_tx::{SystemTx, CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX},
    tee_bootstrap_v2::TeeBootstrapV2,
};
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::Recovered;

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
        let Ok(SystemTx::Consensus {
            step_index: CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX,
            payload,
        }) = SystemTx::decode(tx.input().as_ref())
        else {
            continue;
        };
        let Ok(payload) =
            outbe_primitives::tee_bootstrap_v2::codec::decode_canonical(payload.as_ref())
        else {
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
