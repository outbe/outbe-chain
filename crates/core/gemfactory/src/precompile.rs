use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, mutate, mutate_void};
use outbe_primitives::error::Result;

use crate::runtime;
use crate::schema::GemFactoryContract;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IGemFactory.sol"
);

pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IGemFactory::IGemFactoryCalls::abi_decode, |call| {
        use IGemFactory::IGemFactoryCalls::*;
        match call {
            settleGem(c) => mutate_void(c, caller, |sender, c| {
                runtime::settle_gem(&storage, sender, c.gemId)
            }),
            mineGemPromis(c) => mutate(c, caller, |sender, c| {
                runtime::mine_gem_promis(&storage, sender, c.gemId, c.nonce)
            }),
            getStatistics(_) => metadata::<IGemFactory::getStatisticsCall>(|| {
                let factory = GemFactoryContract::new(storage.clone());
                Ok(IGemFactory::getStatisticsReturn {
                    totalGemsIssued: factory.total_gems_issued.read()?,
                    totalIntexParked: factory.total_intex_parked.read()?,
                })
            }),
        }
    })
}
