//! ABI decode, dispatch and encode for the CCA registry.
use crate::{api, runtime};
use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::dispatch::{
    dispatch_call, mutate_void, mutate_void_payable, reject_value_unless_payable, view,
};
use outbe_primitives::{erc::ERC165_INTERFACE_ID, error::Result, storage::StorageHandle};

sol!(
    #[sol(all_derives)]
    "../../../contracts/precompiles/src/ICcaRegistry.sol"
);
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[ICcaRegistry::bondCall::SELECTOR];

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value_unless_payable(data, PAYABLE_SELECTORS, &value)?;
    dispatch_call(data, ICcaRegistry::ICcaRegistryCalls::abi_decode, |call| {
        use ICcaRegistry::ICcaRegistryCalls::*;
        match call {
            bond(c) => {
                mutate_void_payable(c, PAYABLE_SELECTORS, caller, value, |sender, c, amount| {
                    runtime::bond(storage.clone(), sender, amount, c.name)
                })
            }
            unbond(c) => mutate_void(c, caller, |sender, _| {
                runtime::unbond(storage.clone(), sender)
            }),
            claimUnbonded(c) => mutate_void(c, caller, |sender, _| {
                runtime::claim_unbonded(storage.clone(), sender)
            }),
            claimRewards(c) => mutate_void(c, caller, |sender, _| {
                runtime::claim_rewards(storage.clone(), sender)
            }),
            getCca(c) => view(c, |c| api::get_cca(&storage, c.cca)),
            getCcaState(c) => view(c, |c| api::cca_state(&storage, c.cca)),
            supportsInterface(c) => view(c, |c| Ok(c.interfaceId.0 == ERC165_INTERFACE_ID)),
        }
    })
}
