use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::{
    dispatch::{dispatch_call, mutate_void, reject_value, view},
    error::Result,
    storage::StorageHandle,
};

use crate::schema::CodeRegistry;

pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ICodeRegistry.sol"
);

pub fn dispatch(
    storage: StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(
        data,
        ICodeRegistry::ICodeRegistryCalls::abi_decode,
        |call| {
            use ICodeRegistry::ICodeRegistryCalls::*;
            let mut registry = CodeRegistry::new(storage.clone());
            match call {
                registerRepository(call) => mutate_void(&storage, call, caller, |sender, call| {
                    registry
                        .register_repository(call.repoId, sender)
                        .map(|_| ())
                }),
                repositoryCount(call) => view(call, |_| registry.repository_count.read()),
                repositoryAt(call) => view(call, |call| registry.repository_at(call.index)),
                repositoryRegistrant(call) => {
                    view(call, |call| registry.registrant_of(call.repoId))
                }
                registryGeneration(call) => view(call, |_| registry.registry_generation.read()),
                maxRepositories(call) => view(call, |_| registry.config_max_repositories.read()),
                getPatchApproval(call) => view(call, |call| {
                    let approval = registry.patch_approval(call.proposalId)?;
                    Ok(match approval {
                        Some(approval) => ICodeRegistry::PatchApprovalInfo {
                            approved: true,
                            payload: approval.payload,
                            approvedHeight: approval.approved_height,
                            approvedTimestamp: approval.approved_timestamp,
                        },
                        None => ICodeRegistry::PatchApprovalInfo {
                            approved: false,
                            payload: String::new(),
                            approvedHeight: 0,
                            approvedTimestamp: 0,
                        },
                    })
                }),
                patchExecutionStage(call) => view(call, |call| {
                    if registry.patch_approval(call.proposalId)?.is_none() {
                        return Ok(0u8);
                    }
                    registry.patch_execution_stage.read(&call.proposalId)
                }),
            }
        },
    )
}
