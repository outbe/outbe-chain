//! Neutral enclave-upgrade record shared by the registry and its callers.

use alloy_primitives::{B256, U256};

/// Explicit opt-in hard retirement of one enclave measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnclaveUpgradeV1 {
    pub proposal_id: U256,
    pub activation_height: u64,
    pub mrenclave: B256,
    pub successor_policy_hash: B256,
    pub predecessor_policy_hash: B256,
}
