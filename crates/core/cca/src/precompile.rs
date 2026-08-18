//! ABI decode, dispatch and encode for the CCA registry precompile.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_common::WorldwideDay;

use outbe_primitives::dispatch::{dispatch_call, mutate_void, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;

use crate::schema::{CcaContract, CcaRecord, CcaState};

/// Selectors on this precompile that accept native value. The bond of §8.1 will
/// make `register` payable; nothing here takes value yet.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!("../../../contracts/precompiles/src/ICca.sol");

pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, ICca::ICcaCalls::abi_decode, |call| {
        let mut contract = CcaContract::new(storage.clone());
        use ICca::ICcaCalls::*;
        match call {
            register(c) => mutate_void(c, caller, |sender, _| {
                let now = contract.storage.timestamp()?.to::<u64>();
                contract.register(sender, now)
            }),
            deregister(c) => mutate_void(c, caller, |sender, _| contract.deregister(sender)),
            getCca(c) => view(c, |c| {
                let record = contract.get_cca(c.cca)?;
                abi_cca(&record)
            }),
            canOriginate(c) => view(c, |c| contract.can_originate(c.cca)),
            multiplierOf(c) => view(c, |c| contract.multiplier_of(c.cca)),
            originationUnits(c) => view(c, |c| {
                contract.origination_units(WorldwideDay::new(c.worldwideDay), c.cca)
            }),
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID)
            }),
        }
    })
}

/// Maps the stored `u8` onto the ABI enum explicitly rather than transmuting the
/// discriminant, so a corrupt byte surfaces as a revert instead of decoding as
/// some neighbouring state.
fn abi_cca(record: &CcaRecord) -> Result<ICca::Cca> {
    let state = match record.registration_state()? {
        CcaState::Active => ICca::State::Active,
        CcaState::Suspended => ICca::State::Suspended,
        CcaState::Deregistered => ICca::State::Deregistered,
    };
    Ok(ICca::Cca {
        cca: record.cca,
        multiplier: record.multiplier,
        registeredAt: record.registered_at,
        state,
    })
}
