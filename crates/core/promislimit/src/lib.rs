pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;

use alloy_primitives::U256;
use outbe_primitives::addresses::SYSTEM_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

pub use runtime::BurnSource;
pub use schema::PromisLimitContract;

/// Settles already-burned block fees into the Promis Limit.
///
/// The caller is the block executor. The burn source is [`BurnSource::BaseFee`]
/// and the escrow is [`SYSTEM_ADDRESS`], so the credit does not debit a balance.
pub fn settle_block_burned_fees(storage: &mut StorageHandle<'_>, burned_fees: U256) -> Result<()> {
    let mut contract = PromisLimitContract::new(storage.clone());
    contract
        .burn_to_promis_limit(SYSTEM_ADDRESS, burned_fees, BurnSource::BaseFee)
        .map(|_| ())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod settle_block_burned_fees_tests {
    use super::settle_block_burned_fees;
    use alloy_primitives::U256;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;

    use crate::PromisLimitContract;

    const CHAIN_ID: u64 = 1;

    #[test]
    fn zero_burn_leaves_the_promis_limit_unchanged() {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        StorageHandle::enter(&mut provider, |mut storage| {
            settle_block_burned_fees(&mut storage, U256::ZERO).unwrap();
            let contract = PromisLimitContract::new(storage);
            assert_eq!(contract.promis_limit().unwrap(), U256::ZERO);
            assert_eq!(contract.rest_minor().unwrap(), U256::ZERO);
        });
    }

    #[test]
    fn base_fee_burn_stores_the_remainder() {
        let burned = U256::from(1_500u64);
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        StorageHandle::enter(&mut provider, |mut storage| {
            settle_block_burned_fees(&mut storage, burned).unwrap();
            let contract = PromisLimitContract::new(storage);
            assert_eq!(contract.rest_minor().unwrap(), burned);
            assert_eq!(contract.promis_limit().unwrap(), U256::ZERO);
        });
    }
}
