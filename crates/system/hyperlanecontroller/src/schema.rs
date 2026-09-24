use alloy_primitives::{keccak256, Address, B256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;

/// EVM storage layout for the Hyperlane controller.
///
/// Storage slots:
///   0: ica_router - InterchainAccountRouter on Outbe (zero = not initialized)
///   1: ism_by_domain - mapping(domain => StorageMessageIdMultisigIsm on that
///      chain), the Outbe chain included under its own domain (= chain id);
///      zero = absent
///   2: domains - enumerable list of the configured domains
///   3: hook_by_domain - mapping(domain => MerkleTreeHook on that chain); the
///      hook address is part of the checkpoint digest validators sign
///   4: signer_of - mapping(validator => Hyperlane signing key); zero means
///      the validator address itself
///   5: submitted_index - mapping(validator_domain_key => latest submitted
///      checkpoint index)
///   6: submitted_block - mapping(validator_domain_key => block of that
///      submission); zero = never submitted
///   7: miss_count - mapping(validator => consecutive liveness misses)
#[storage_schema]
#[contract(addr = HYPERLANE_CONTROLLER_ADDRESS)]
pub struct HyperlaneControllerContract {
    #[attribute(order = 0)]
    pub ica_router: outbe_primitives::storage::dsl::Value<Address>,

    #[attribute(order = 1)]
    pub ism_by_domain: outbe_primitives::storage::dsl::Map<u32, Address>,

    #[attribute(order = 2)]
    pub domains: outbe_primitives::storage::dsl::List<u32>,

    #[attribute(order = 3)]
    pub hook_by_domain: outbe_primitives::storage::dsl::Map<u32, Address>,

    #[attribute(order = 4)]
    pub signer_of: outbe_primitives::storage::dsl::Map<Address, Address>,

    #[attribute(order = 5)]
    pub submitted_index: outbe_primitives::storage::dsl::Map<B256, u32>,

    #[attribute(order = 6)]
    pub submitted_block: outbe_primitives::storage::dsl::Map<B256, u64>,

    #[attribute(order = 7)]
    pub miss_count: outbe_primitives::storage::dsl::Map<Address, u32>,
}

/// Composite key for the per-(validator, domain) checkpoint maps.
pub fn validator_domain_key(validator: Address, domain: u32) -> B256 {
    let mut buf = [0u8; 24];
    buf[..20].copy_from_slice(validator.as_slice());
    buf[20..].copy_from_slice(&domain.to_be_bytes());
    keccak256(buf)
}
