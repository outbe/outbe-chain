use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, mutate_void, reject_value, view};
use outbe_primitives::error::{PrecompileError, Result};

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time. If you add a selector
/// here and do not flip the route, the build fails.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IValidatorSet.sol"
);

/// Dispatches an ABI-encoded call to the ValidatorSet precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(
        data,
        IValidatorSet::IValidatorSetCalls::abi_decode,
        |call| {
            route(
                &storage,
                &mut crate::schema::ValidatorSet::new(storage.clone()),
                caller,
                call,
            )
        },
    )
}

/// Routes one decoded call to its ValidatorSet operation. Every selector has
/// exactly one arm.
fn route(
    storage: &outbe_primitives::storage::StorageHandle<'_>,
    vs: &mut crate::schema::ValidatorSet<'_>,
    caller: Address,
    call: IValidatorSet::IValidatorSetCalls,
) -> Result<Bytes> {
    use crate::delegation::ValidatorDelegateRole as Role;
    use IValidatorSet as I;
    use IValidatorSet::IValidatorSetCalls::*;
    match call {
        getValidators(_) => address_list::<I::getValidatorsCall>(|| vs.get_all_validators()),
        getActiveValidators(_) => {
            address_list::<I::getActiveValidatorsCall>(|| vs.get_active_validators())
        }
        getActiveConsensusSet(_) => {
            address_list::<I::getActiveConsensusSetCall>(|| vs.get_active_consensus_set())
        }
        validatorByAddress(c) => view(c, |c| Ok(validator_view(vs, c.addr)?.into())),
        validatorByIndex(c) => view(c, |c| Ok(validator_view_at(vs, c.index)?.into())),
        validatorCount(_) => metadata::<I::validatorCountCall>(|| vs.validator_count()),
        activeValidatorCount(_) => {
            metadata::<I::activeValidatorCountCall>(|| vs.active_validator_count())
        }
        activeConsensusCount(_) => {
            metadata::<I::activeConsensusCountCall>(|| vs.active_consensus_count())
        }
        isValidator(c) => view(c, |c| vs.is_validator(c.addr)),
        isConsensusParticipant(c) => view(c, |c| vs.is_consensus_participant(c.addr)),
        hasPendingSetChange(_) => {
            metadata::<I::hasPendingSetChangeCall>(|| vs.has_pending_set_change())
        }
        getEpochNumber(_) => metadata::<I::getEpochNumberCall>(|| vs.epoch_number.read()),
        getEpochStartTimestamp(_) => {
            metadata::<I::getEpochStartTimestampCall>(|| vs.epoch_start_timestamp.read())
        }
        getEpochStartBlock(_) => {
            metadata::<I::getEpochStartBlockCall>(|| vs.epoch_start_block.read())
        }
        setDelegate(c) => mutate_void(storage, c, caller, |sender, c| {
            vs.set_delegate(sender, Role::try_from(c.role)?, c.delegate)
        }),
        revokeDelegate(c) => mutate_void(storage, c, caller, |sender, c| {
            vs.revoke_delegate(sender, Role::try_from(c.role)?)
        }),
        getDelegate(c) => view(c, |c| vs.get_delegate(c.validator, Role::try_from(c.role)?)),
        resolveValidator(c) => view(c, |c| resolved_signer(vs, c.signer, c.role)),
        getRadicleNodeId(c) => view(c, |c| vs.get_radicle_node_id(c.validator)),
        validatorByRadicleNodeId(c) => view(c, |c| vs.validator_by_radicle_node_id(c.nodeId)),
        registerValidator(c) => mutate_void(storage, c, caller, |sender, c| {
            register_from_call(vs, sender, &c)
        }),
        setP2pAddress(c) => mutate_void(storage, c, caller, |sender, c| {
            vs.set_p2p_address(sender, c.validatorAddress, c.version, &c.encoded)
        }),
        getP2pAddress(c) => view(c, |c| Ok(p2p_view(vs, c.validatorAddress)?.into())),
        deactivateValidator(c) => mutate_void(storage, c, caller, |sender, c| {
            vs.deactivate_validator(sender, c.validatorAddress)
        }),
        confirmValidatorReady(c) => mutate_void(storage, c, caller, |sender, c| {
            vs.confirm_validator_ready(sender, &c.registration)
        }),
    }
}

/// The ABI tuple of `validatorByAddress` and `validatorByIndex`.
type ValidatorView = (
    Address,
    Bytes,
    U256,
    u8,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    u64,
    bool,
);

/// The ABI view of the registered validator `addr`.
fn validator_view(vs: &crate::schema::ValidatorSet<'_>, addr: Address) -> Result<ValidatorView> {
    let v = vs
        .get_validator(addr)?
        .ok_or_else(|| PrecompileError::Revert("validator not found".into()))?;
    Ok((
        v.validator_address,
        Bytes::copy_from_slice(&v.consensus_pubkey),
        v.stake,
        v.status,
        v.slash_count,
        v.missed_blocks,
        v.missed_votes,
        v.blocks_proposed,
        v.joined_at_height,
        v.deactivated_at_height,
        v.unbonding_end,
        v.has_bls_share,
    ))
}

/// The ABI view of the validator at the 1-based registry `index`.
fn validator_view_at(vs: &crate::schema::ValidatorSet<'_>, index: u64) -> Result<ValidatorView> {
    let addr = vs
        .validator_address_at(index)?
        .ok_or_else(|| PrecompileError::Revert("validator not found at index".into()))?;
    validator_view(vs, addr)
}

/// Encodes the addresses of the validators that `validators` returns.
fn address_list<T: SolCall<Return = Vec<Address>>>(
    validators: impl FnOnce() -> Result<Vec<crate::runtime::ValidatorRecord>>,
) -> Result<Bytes> {
    metadata::<T>(|| Ok(validators()?.iter().map(|v| v.validator_address).collect()))
}

/// The validator that `signer` signs for in `role`, or the zero address.
fn resolved_signer(
    vs: &crate::schema::ValidatorSet<'_>,
    signer: Address,
    role: u8,
) -> Result<Address> {
    let role = crate::delegation::ValidatorDelegateRole::try_from(role)?;
    Ok(vs
        .resolve_validator_for_role(signer, role)?
        .unwrap_or(Address::ZERO))
}

/// The stored versioned P2P address of `validator`, or version 0 with no
/// bytes.
fn p2p_view(vs: &crate::schema::ValidatorSet<'_>, validator: Address) -> Result<(u8, Bytes)> {
    let (version, encoded) = vs.get_p2p_address(validator)?.unwrap_or((0, Vec::new()));
    Ok((version, Bytes::from(encoded)))
}

/// Registers the validator of a `registerValidator` call from `sender`.
fn register_from_call(
    vs: &mut crate::schema::ValidatorSet<'_>,
    sender: Address,
    call: &IValidatorSet::registerValidatorCall,
) -> Result<()> {
    let (pubkey, sig) =
        registration_key_and_proof(&call.consensusPubkey, &call.blsRegistrationSignature)?;
    vs.register_validator_with_sig(
        sender,
        call.validatorAddress,
        &pubkey,
        call.radicleNodeId,
        Some(sig),
    )
}

/// The 48-byte consensus key and the 96-byte BLS proof of possession of a
/// registration call. The key length is checked first.
fn registration_key_and_proof<'a>(
    consensus_pubkey: &[u8],
    signature: &'a [u8],
) -> Result<([u8; 48], &'a [u8; 96])> {
    if consensus_pubkey.len() != 48 {
        return Err(PrecompileError::Revert(
            "consensus pubkey must be 48 bytes".into(),
        ));
    }
    let pubkey: [u8; 48] = consensus_pubkey[..48]
        .try_into()
        .map_err(|_| PrecompileError::Revert("consensus pubkey conversion failed".into()))?;
    if signature.len() != 96 {
        return Err(PrecompileError::Revert(
            "BLS proof of possession must be exactly 96 bytes".into(),
        ));
    }
    let sig: &[u8; 96] = signature[..96]
        .try_into()
        .map_err(|_| PrecompileError::Revert("BLS signature conversion failed".into()))?;
    Ok((pubkey, sig))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;
    use outbe_primitives::validators::{
        validator_registration_message, VALIDATOR_REGISTRATION_DST,
    };

    #[test]
    fn registration_v2_and_radicle_getters_round_trip_through_public_abi() {
        let chain_id = 54322345;
        let validator = Address::repeat_byte(0x61);
        let node_id = B256::repeat_byte(0x71);
        let secret = blst::min_pk::SecretKey::key_gen(&[0x41; 32], &[]).unwrap();
        let public_key = secret.sk_to_pk().to_bytes();
        let signature = secret
            .sign(
                &validator_registration_message(chain_id, validator, node_id),
                VALIDATOR_REGISTRATION_DST,
                &[],
            )
            .to_bytes();
        let mut provider = HashMapStorageProvider::new(chain_id);
        provider.set_block_number(1);

        StorageHandle::enter(&mut provider, |storage| {
            let vs = crate::schema::ValidatorSet::new(storage.clone());
            vs.config_owner.write(validator).unwrap();
            vs.config_max_validators.write(10).unwrap();

            dispatch(
                storage.clone(),
                &IValidatorSet::registerValidatorCall {
                    validatorAddress: validator,
                    consensusPubkey: Bytes::copy_from_slice(&public_key),
                    radicleNodeId: node_id,
                    blsRegistrationSignature: Bytes::copy_from_slice(&signature),
                }
                .abi_encode(),
                validator,
                U256::ZERO,
            )
            .unwrap();

            let forward = dispatch(
                storage.clone(),
                &IValidatorSet::getRadicleNodeIdCall { validator }.abi_encode(),
                validator,
                U256::ZERO,
            )
            .unwrap();
            assert_eq!(
                IValidatorSet::getRadicleNodeIdCall::abi_decode_returns(&forward).unwrap(),
                node_id
            );

            let reverse = dispatch(
                storage,
                &IValidatorSet::validatorByRadicleNodeIdCall { nodeId: node_id }.abi_encode(),
                validator,
                U256::ZERO,
            )
            .unwrap();
            assert_eq!(
                IValidatorSet::validatorByRadicleNodeIdCall::abi_decode_returns(&reverse).unwrap(),
                validator
            );
        });
    }
}
