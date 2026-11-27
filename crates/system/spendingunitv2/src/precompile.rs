//! EVM precompile ABI dispatch for `ISpendingUnitV2`.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;
use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, reject_value, view};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::SpendingUnitV2Contract;

/// No selector on `SpendingUnitV2` accepts native value.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

#[allow(clippy::too_many_arguments)]
mod abi {
    use alloy_sol_types::sol;

    sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/ISpendingUnitV2.sol"
    );
}

pub use abi::ISpendingUnitV2;

/// Dispatches ABI-encoded calldata for `SpendingUnitV2`.
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(
        data,
        ISpendingUnitV2::ISpendingUnitV2Calls::abi_decode,
        |call| {
            use ISpendingUnitV2::ISpendingUnitV2Calls::*;
            let mut contract = SpendingUnitV2Contract::new(storage.clone());
            match call {
                submitSU(c) => mutate_void(&storage, c, caller, |sender, c| {
                    contract.submit_su(sender, &c.mint, &c.batch, &c.seals)
                }),
                multicall(c) => mutate(&storage, c, caller, |sender, c| {
                    contract.multicall(sender, &c.data)
                }),
                getData(c) => view(c, |c| contract.get_data(c.suId)),
                settlements(c) => view(c, |c| contract.settlements.read(&c.ctx)),
                enclaveRegistry(c) => view(c, |_| contract.enclave_registry.read()),
                fsEpochRegistry(c) => view(c, |_| contract.fs_epoch_registry.read()),
                enclaveRegistryContract(c) => view(c, |_| contract.enclave_registry.read()),
                fsEpochRegistryContract(c) => view(c, |_| contract.fs_epoch_registry.read()),
                VERSION(c) => view(c, |_| Ok(crate::runtime::VERSION.to_owned())),
                MAX_SEALS(c) => view(c, |_| Ok(U256::from(crate::runtime::MAX_SEALS))),
                POOL_RETENTION_DAYS(c) => {
                    view(c, |_| Ok(U256::from(crate::runtime::POOL_RETENTION_DAYS)))
                }
                exists(c) => view(c, |c| contract.committed.read(&c.id)),
                total(c) => view(c, |_| contract.total_committed.read()),
                supportsInterface(c) => view(c, |c| {
                    Ok(SpendingUnitV2Contract::supports_interface(c.interfaceId))
                }),
                paused(c) => view(c, |_| contract.paused.read()),
                pause(c) => mutate_void(&storage, c, caller, |sender, _| contract.pause(sender)),
                unpause(c) => {
                    mutate_void(&storage, c, caller, |sender, _| contract.unpause(sender))
                }
                owner(c) => view(c, |_| contract.owner.read()),
                transferOwnership(c) => mutate_void(&storage, c, caller, |sender, c| {
                    contract.transfer_ownership(sender, c.newOwner)
                }),
                renounceOwnership(c) => mutate_void(&storage, c, caller, |sender, _| {
                    contract.renounce_ownership(sender)
                }),
                attesterRegistry(c) => view(c, |_| contract.attester_registry.read()),
                setAttestersRegistry(c) => mutate_void(&storage, c, caller, |sender, c| {
                    contract.set_attesters_registry(sender, c._attesterRegistry)
                }),
                getAttestersRegistry(c) => view(c, |_| contract.attester_registry.read()),
            }
        },
    )
}
