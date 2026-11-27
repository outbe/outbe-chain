//! EVM precompile ABI dispatch for `IEnclaveRegistry`.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate_void, reject_value, view};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::EnclaveRegistryContract;

/// No selector on `EnclaveRegistry` accepts native value.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IEnclaveRegistry.sol"
);

/// Dispatches ABI-encoded calldata for `EnclaveRegistry`.
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(
        data,
        IEnclaveRegistry::IEnclaveRegistryCalls::abi_decode,
        |call| {
            use IEnclaveRegistry::IEnclaveRegistryCalls::*;
            let mut registry = EnclaveRegistryContract::new(storage.clone());
            match call {
                register(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.register(
                        sender,
                        c.registration.as_ref(),
                        c.report.as_ref(),
                        c.vcek.as_ref(),
                        c.product,
                        c.manifest.as_ref(),
                    )
                }),
                addMeasurement(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.add_measurement(
                        sender,
                        c.enclavePk,
                        c.report.as_ref(),
                        c.vcek.as_ref(),
                        c.product,
                    )
                }),
                reattest(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.reattest(
                        sender,
                        c.enclavePk,
                        c.report.as_ref(),
                        c.vcek.as_ref(),
                        c.product,
                    )
                }),
                retire(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.retire(sender, c.enclavePk)
                }),
                revoke(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.revoke(sender, c.enclavePk)
                }),
                adminRegister(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.admin_register(
                        sender,
                        c.enclavePk,
                        c.attester,
                        c.rmDigest,
                        c.providerId,
                        c.baseUrlHash,
                        c.measurementHash,
                    )
                }),
                admin(c) => view(c, |_| registry.admin.read()),
                pendingAdmin(c) => view(c, |_| registry.pending_admin.read()),
                attestersRegistry(c) => view(c, |_| registry.attesters_registry.read()),
                initialize(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.initialize(sender, c._admin, c._attestersRegistry)
                }),
                setMeasurement(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.set_measurement(sender, c.measurementHash, c.allowed)
                }),
                setProvider(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.set_provider(sender, c.providerId, &c.policy)
                }),
                setTcbFloor(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.set_tcb_floor(sender, c.product, c.floor)
                }),
                revokeChip(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.revoke_chip(sender, c.chipIdHash)
                }),
                setRetiringGrace(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.set_retiring_grace(sender, c.blocks)
                }),
                setMaxSchema(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.set_max_schema(sender, c.schema)
                }),
                setMinRegistrationVersion(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.set_min_registration_version(sender, c.version)
                }),
                initiateAdminTransfer(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.initiate_admin_transfer(sender, c.newAdmin)
                }),
                acceptAdminTransfer(c) => mutate_void(&storage, c, caller, |sender, _| {
                    registry.accept_admin_transfer(sender)
                }),
                allowedMeasurements(c) => view(c, |c| {
                    registry.allowed_measurements.read(&c.measurementHash)
                }),
                providers(c) => view(c, |c| {
                    let p = registry.read_provider(c.providerId)?;
                    Ok(IEnclaveRegistry::providersReturn {
                        allowed: p.allowed,
                        requireRelease: p.require_release,
                        baseUrlHash: p.base_url_hash,
                        environmentHash: p.environment_hash,
                    })
                }),
                tcbFloor(c) => view(c, |c| registry.tcb_floor.read(&c.product)),
                revokedChips(c) => view(c, |c| registry.revoked_chips.read(&c.chipIdHash)),
                retiringGrace(c) => view(c, |_| registry.retiring_grace.read()),
                maxSchema(c) => view(c, |_| registry.max_schema.read()),
                minRegistrationVersion(c) => view(c, |_| registry.min_registration_version.read()),
                devnetAdminRegister(c) => view(c, |_| registry.devnet_admin_register.read()),
                getDeployment(c) => view(c, |c| registry.get_deployment(c.enclavePk)),
                isActive(c) => view(c, |c| registry.is_active(c.enclavePk)),
                isActiveFor(c) => view(c, |c| {
                    registry.is_active_for(c.enclavePk, c.attester, c.providerId)
                }),
                measurementsOf(c) => view(c, |c| registry.measurements_of(c.enclavePk)),
                chipIdsOf(c) => view(c, |c| registry.chip_ids_of(c.enclavePk)),
                allEnclaves(c) => view(c, |c| registry.all_enclaves(c.offset, c.limit)),
                enclaveCount(c) => view(c, |_| registry.enclave_count()),
            }
        },
    )
}
