use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;
use outbe_primitives::dispatch::{dispatch_call, mutate, mutate_void, view};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;

pub use crate::abi::IStablecoin;
use crate::errors::StablecoinStateError;
use crate::schema::StablecoinContract;

/// Dispatches the shared stablecoin ABI against the actual dynamic token address.
pub fn dispatch(
    storage: StorageHandle,
    token_address: Address,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    if value != U256::ZERO {
        return Err(StablecoinStateError::UnexpectedValue { value }.into());
    }
    dispatch_call(
        data,
        |input| {
            let call = IStablecoin::IStablecoinCalls::abi_decode(input)
                .map_err(|error| error.to_string())?;
            if call.abi_encode().as_slice() != input {
                return Err("non-canonical ABI calldata or trailing bytes".to_owned());
            }
            Ok(call)
        },
        |call| {
            use IStablecoin::IStablecoinCalls::*;

            let mut token = StablecoinContract::new(storage, token_address);
            match call {
                name(call) => view(call, |_| token.name_value()),
                symbol(call) => view(call, |_| token.symbol_value()),
                decimals(call) => view(call, |_| token.decimals_value()),
                totalSupply(call) => view(call, |_| token.total_supply()),
                balanceOf(call) => view(call, |call| token.balance_of(call.account)),
                allowance(call) => view(call, |call| token.allowance_of(call.owner, call.spender)),
                approve(call) => mutate(call, caller, |owner, call| {
                    token
                        .approve(owner, call.spender, call.value)
                        .map(|()| true)
                }),
                transfer(call) => mutate(call, caller, |from, call| {
                    token.transfer(from, call.to, call.value).map(|()| true)
                }),
                transferFrom(call) => mutate(call, caller, |spender, call| {
                    token
                        .transfer_from(spender, call.from, call.to, call.value)
                        .map(|()| true)
                }),
                nonces(call) => view(call, |call| {
                    token.validated_schema_version()?;
                    token.nonces.read(&call.owner)
                }),
                currency(call) => view(call, |_| {
                    token.validated_schema_version()?;
                    token.currency.read()
                }),
                supplyCap(call) => view(call, |_| {
                    token.validated_schema_version()?;
                    token.supply_cap.read()
                }),
                policyId(call) => view(call, |_| {
                    token.validated_schema_version()?;
                    token.policy_id.read()
                }),
                issuer(call) => view(call, |_| {
                    token.validated_schema_version()?;
                    token.issuer.read()
                }),
                paused(call) => view(call, |_| {
                    token.validated_schema_version()?;
                    token.paused.read()
                }),
                hasRole(call) => view(call, |call| token.has_role(call.role, call.account)),
                pendingAdmin(call) => view(call, |_| {
                    token.validated_schema_version()?;
                    token.pending_admin.read()
                }),
                creationProtocolVersion(call) => view(call, |_| {
                    token.validated_schema_version()?;
                    token.creation_protocol_version.read()
                }),
                mint(call) => mutate(call, caller, |actor, call| {
                    token.mint(actor, call.to, call.amount).map(|()| true)
                }),
                burn(call) => mutate(call, caller, |actor, call| {
                    token.burn(actor, call.amount).map(|()| true)
                }),
                burnFrom(call) => mutate(call, caller, |spender, call| {
                    token
                        .burn_from(spender, call.from, call.amount)
                        .map(|()| true)
                }),
                grantRole(call) => mutate_void(call, caller, |actor, call| {
                    token.grant_role(actor, call.role, call.account)
                }),
                revokeRole(call) => mutate_void(call, caller, |actor, call| {
                    token.revoke_role(actor, call.role, call.account)
                }),
                setSupplyCap(call) => mutate_void(call, caller, |actor, call| {
                    token.set_supply_cap(actor, call.newCap)
                }),
                pause(call) => mutate_void(call, caller, |actor, _| token.pause(actor)),
                unpause(call) => mutate_void(call, caller, |actor, _| token.unpause(actor)),
                beginAdminTransfer(call) => mutate_void(call, caller, |actor, call| {
                    token.begin_admin_transfer(actor, call.candidate)
                }),
                cancelAdminTransfer(call) => {
                    mutate_void(call, caller, |actor, _| token.cancel_admin_transfer(actor))
                }
                acceptAdminTransfer(call) => {
                    mutate_void(call, caller, |actor, _| token.accept_admin_transfer(actor))
                }
                _ => Err(PrecompileError::Revert(
                    "stablecoin selector is not implemented in the current build phase".into(),
                )),
            }
        },
    )
}
