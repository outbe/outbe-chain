use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate_void, reject_value, view};
use outbe_primitives::error::Result;

use crate::schema::L2RegistryContract;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IL2Registry.sol"
);

/// Dispatch for the L2 registry precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(data, IL2Registry::IL2RegistryCalls::abi_decode, |call| {
        use IL2Registry::IL2RegistryCalls::*;
        let mut registry = L2RegistryContract::new(storage);
        match call {
            registerNetwork(c) => mutate_void(c, caller, |_sender, c| {
                registry.register_network(c.chainId, c.l1Address, &c.publicKey)
            }),
            setZkEnabled(c) => mutate_void(c, caller, |_sender, c| {
                registry.set_zk_enabled(c.chainId, c.enabled)
            }),
            removeNetwork(c) => {
                mutate_void(c, caller, |_sender, c| registry.remove_network(c.chainId))
            }
            getNetwork(c) => view(c, |c| {
                let record = registry.load_network(c.chainId)?;
                Ok(IL2Registry::getNetworkReturn {
                    l1Address: record.l1_address,
                    publicKey: Bytes::copy_from_slice(&record.public_key_bytes()),
                    zkEnabled: record.zk_enabled,
                })
            }),
            chainIdByL1Address(c) => view(c, |c| registry.l1_to_chain.read(&c.l1Address)),
        }
    })
}
