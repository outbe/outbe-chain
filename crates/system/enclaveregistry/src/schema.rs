//! Persistent EVM storage schema for the EnclaveRegistry precompile.

use alloy_primitives::{Address, B256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_system_common::addresses::ENCLAVE_REGISTRY_ADDRESS;

/// Stored policy for one bank-data provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[storage_record(exists_field = allowed)]
pub struct ProviderPolicyRecord {
    #[key]
    pub provider_id: B256,

    #[attribute(order = 0)]
    pub allowed: bool,

    #[attribute(order = 1)]
    pub require_release: bool,

    #[attribute(order = 2)]
    pub base_url_hash: B256,

    #[attribute(order = 3)]
    pub environment_hash: B256,
}

/// Stored scalar metadata for one registered enclave deployment.
///
/// The dynamic `measurements` and `chip_ids` vectors live in sibling maps
/// keyed by `enclave_pk`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[storage_record(exists_field = status)]
pub struct DeploymentRecord {
    #[key]
    pub enclave_pk: B256,

    #[attribute(order = 0)]
    pub attester: Address,

    #[attribute(order = 1)]
    pub rm_digest: B256,

    #[attribute(order = 2)]
    pub provider_id: B256,

    #[attribute(order = 3)]
    pub base_url_hash: B256,

    #[attribute(order = 4)]
    pub registered_at: u64,

    #[attribute(order = 5)]
    pub retiring_until: u64,

    /// 0 = None, 1 = Active, 2 = Retiring.
    #[attribute(order = 6)]
    pub status: u8,

    #[attribute(order = 7)]
    pub tcb: u64,

    #[attribute(order = 8)]
    pub product: u8,
}

/// EVM storage layout for `EnclaveRegistry` (`0x5200000000000000000000000000000000000002`).
///
/// Storage slots:
///   0: `admin` (`Address`) — genesis writes slot 0
///   1: `pending_admin` (`Address`)
///   2: `attesters_registry` (`Address`) — genesis writes slot 2
///   3: `retiring_grace` (`u64`) — genesis writes slot 3 (default 7200)
///   4: `max_schema` (`u16`) — genesis writes slot 4 (default 6)
///   5: `devnet_admin_register` (`bool`) — genesis writes slot 5
///   6: `min_registration_version` (`u8`) — genesis writes slot 6 (default 2)
///   7: `all_enclaves` (`List<B256>`)
///   8: `allowed_measurements` (`Map<B256, bool>`)
///   9: `tcb_floor` (`Map<u8, u64>`)
///   10: `revoked_chips` (`Map<B256, bool>`)
///   11..15: `providers` (`Map<B256, ProviderPolicyRecord>`, 4 slots)
///   15..24: `deployments` (`Map<B256, DeploymentRecord>`, 9 slots)
///   24: `deployment_measurements` (`Map<B256, List<B256>>`)
///   25: `deployment_chip_ids` (`Map<B256, List<B256>>`)
#[storage_schema]
#[contract(addr = ENCLAVE_REGISTRY_ADDRESS)]
pub struct EnclaveRegistryContract {
    #[attribute(order = 0)]
    pub admin: Value<Address>,

    #[attribute(order = 1)]
    pub pending_admin: Value<Address>,

    #[attribute(order = 2)]
    pub attesters_registry: Value<Address>,

    #[attribute(order = 3)]
    pub retiring_grace: Value<u64>,

    #[attribute(order = 4)]
    pub max_schema: Value<u16>,

    #[attribute(order = 5)]
    pub devnet_admin_register: Value<bool>,

    #[attribute(order = 6)]
    pub min_registration_version: Value<u8>,

    #[attribute(order = 7)]
    pub all_enclaves: List<B256>,

    #[attribute(order = 8)]
    pub allowed_measurements: Map<B256, bool>,

    #[attribute(order = 9)]
    pub tcb_floor: Map<u8, u64>,

    #[attribute(order = 10)]
    pub revoked_chips: Map<B256, bool>,

    #[attribute(order = 11)]
    pub providers: Map<B256, ProviderPolicyRecord>,

    #[attribute(order = 12)]
    pub deployments: Map<B256, DeploymentRecord>,

    #[attribute(order = 13)]
    pub deployment_measurements: Map<B256, List<B256>>,

    #[attribute(order = 14)]
    pub deployment_chip_ids: Map<B256, List<B256>>,
}
