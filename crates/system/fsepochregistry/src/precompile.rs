//! EVM precompile ABI dispatch for `IFsEpochRegistry`.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate_void, reject_value, view};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime::digest_of;
use crate::schema::FsEpochRegistryContract;

/// No selector on `FsEpochRegistry` accepts native value.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IFsEpochRegistry.sol"
);

/// Dispatches ABI-encoded calldata for `FsEpochRegistry`.
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(
        data,
        IFsEpochRegistry::IFsEpochRegistryCalls::abi_decode,
        |call| {
            use IFsEpochRegistry::IFsEpochRegistryCalls::*;
            let mut registry = FsEpochRegistryContract::new(storage.clone());
            match call {
                admin(c) => view(c, |_| registry.admin.read()),
                pendingAdmin(c) => view(c, |_| registry.pending_admin.read()),
                anchor(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.anchor(sender, &c.epoch0)
                }),
                truncate(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.truncate(sender, c.from)
                }),
                initiateAdminTransfer(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.initiate_admin_transfer(sender, c.newAdmin)
                }),
                acceptAdminTransfer(c) => mutate_void(&storage, c, caller, |sender, _| {
                    registry.accept_admin_transfer(sender)
                }),
                advance(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.advance(sender, &c.chain)
                }),
                head(c) => view(c, |_| registry.head.read()),
                anchored(c) => view(c, |_| registry.anchored.read()),
                adminNext(c) => view(c, |_| registry.admin_next.read()),
                keyFor(c) => view(c, |c| {
                    let key = registry.key_for(c.e)?;
                    Ok(IFsEpochRegistry::keyForReturn {
                        pkAtt: Bytes::from(key.pk_att),
                        dst: Bytes::from(key.dst),
                        startWwd: key.start_wwd,
                        endWwdExclusive: key.end_wwd_exclusive,
                    })
                }),
                digestAt(c) => view(c, |c| registry.digest_at(c.e)),
                digestOf(c) => view(c, |c| {
                    digest_of(
                        c.e,
                        c.entry.pkAtt.as_ref(),
                        c.entry.dst.as_ref(),
                        c.entry.startWwd,
                        c.entry.membersHash,
                        c.entry.prevHash,
                    )
                }),
            }
        },
    )
}
