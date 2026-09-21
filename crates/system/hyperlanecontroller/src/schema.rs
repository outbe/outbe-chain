use alloy_primitives::Address;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::HYPERLANE_CONTROLLER_ADDRESS;

/// EVM storage layout for the Hyperlane controller.
///
/// Storage slots:
///   0: router - InterchainAccountRouter on Outbe (zero = not initialized)
///   1: ism_by_domain - mapping(domain => StorageMessageIdMultisigIsm on that
///      chain), the Outbe chain included under its own domain (= chain id);
///      zero = absent
///   2: domains - enumerable list of the configured domains
#[storage_schema]
#[contract(addr = HYPERLANE_CONTROLLER_ADDRESS)]
pub struct HyperlaneControllerContract {
    #[attribute(order = 0)]
    pub router: outbe_primitives::storage::dsl::Value<Address>,

    #[attribute(order = 1)]
    pub ism_by_domain: outbe_primitives::storage::dsl::Map<u32, Address>,

    #[attribute(order = 2)]
    pub domains: outbe_primitives::storage::dsl::List<u32>,
}
