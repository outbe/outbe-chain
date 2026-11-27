//! EVM precompile ABI dispatch for `IAttestersRegistry`.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, reject_value, view};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::AttestersRegistryContract;

/// No selector on `AttestersRegistry` accepts native value.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IAttestersRegistry.sol"
);

/// Dispatches ABI-encoded calldata for `AttestersRegistry`.
pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(
        data,
        IAttestersRegistry::IAttestersRegistryCalls::abi_decode,
        |call| {
            use IAttestersRegistry::IAttestersRegistryCalls::*;
            let mut registry = AttestersRegistryContract::new(storage.clone());
            match call {
                admin(c) => view(c, |_| registry.admin.read()),
                pendingAdmin(c) => view(c, |_| registry.pending_admin.read()),
                register(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.register(
                        sender,
                        c.attester,
                        c.permissionMask,
                        c.isRotationCandidate,
                        c.consensusKey,
                        c.p2pAddr,
                    )
                }),
                revoke(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.revoke(sender, c.attester)
                }),
                updateMask(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.update_mask(sender, c.attester, c.newMask)
                }),
                setRotationCandidate(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.set_rotation_candidate(sender, c.attester, c.isRotationCandidate)
                }),
                setConsensusIdentity(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.set_consensus_identity(sender, c.attester, c.consensusKey, c.p2pAddr)
                }),
                initiateAdminTransfer(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.initiate_admin_transfer(sender, c.newAdmin)
                }),
                acceptAdminTransfer(c) => mutate_void(&storage, c, caller, |sender, _| {
                    registry.accept_admin_transfer(sender)
                }),
                forceReshare(c) => mutate(&storage, c, caller, |sender, c| {
                    registry.force_reshare(sender, c.skipDealers)
                }),
                reshareGeneration(c) => view(c, |_| registry.reshare_generation.read()),
                reshareSkipDealers(c) => view(c, |_| registry.reshare_skip_dealers.read_all()),
                activatedGeneration(c) => view(c, |_| registry.activated_generation.read()),
                activatedOutputHash(c) => view(c, |_| registry.activated_output_hash.read()),
                attestGeneration(c) => mutate_void(&storage, c, caller, |sender, c| {
                    registry.attest_generation(sender, c.generation, c.outputHash)
                }),
                attestationQuorum(c) => view(c, |_| registry.attestation_quorum()),
                effectiveAttestCount(c) => view(c, |c| {
                    registry.effective_attest_count(c.generation, c.outputHash)
                }),
                attestationCount(c) => view(c, |c| {
                    registry.effective_attest_count(c.generation, c.outputHash)
                }),
                getRecord(c) => view(c, |c| {
                    let rec = registry.read_record(c.attester)?;
                    Ok(IAttestersRegistry::AttesterRecord {
                        active: rec.active,
                        permissionMask: rec.permission_mask,
                        registeredAt: rec.registered_at,
                        isRotationCandidate: rec.is_rotation_candidate,
                        consensusKey: rec.consensus_key,
                        p2pAddr: rec.p2p_addr,
                    })
                }),
                isActive(c) => view(c, |c| registry.is_active(c.attester)),
                hasPermission(c) => view(c, |c| registry.has_permission(c.attester, c.bit)),
                allActors(c) => view(c, |_| registry.all_actors()),
                actorCount(c) => view(c, |_| registry.actor_count()),
                rotationCandidatesSorted(c) => view(c, |_| registry.rotation_candidates_sorted()),
            }
        },
    )
}
