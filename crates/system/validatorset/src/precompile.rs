use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, mutate_void, reject_value, view};
use outbe_primitives::error::{PrecompileError, Result};

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
            let mut vs = crate::schema::ValidatorSet::new(storage);
            use IValidatorSet::IValidatorSetCalls::*;
            match call {
                getValidators(_) => metadata::<IValidatorSet::getValidatorsCall>(|| {
                    let validators = vs.get_all_validators()?;
                    Ok(validators
                        .iter()
                        .map(|v| v.validator_address)
                        .collect::<Vec<_>>())
                }),
                getActiveValidators(_) => {
                    metadata::<IValidatorSet::getActiveValidatorsCall>(|| {
                        let validators = vs.get_active_validators()?;
                        Ok(validators
                            .iter()
                            .map(|v| v.validator_address)
                            .collect::<Vec<_>>())
                    })
                }
                getActiveConsensusSet(_) => {
                    metadata::<IValidatorSet::getActiveConsensusSetCall>(|| {
                        let validators = vs.get_active_consensus_set()?;
                        Ok(validators
                            .iter()
                            .map(|v| v.validator_address)
                            .collect::<Vec<_>>())
                    })
                }
                validatorByAddress(c) => view(c, |c| {
                    let v = vs
                        .get_validator(c.addr)?
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
                    )
                        .into())
                }),
                validatorByIndex(c) => view(c, |c| {
                    let addr = vs.index_to_address.read(&c.index)?;
                    if addr.is_zero() {
                        return Err(PrecompileError::Revert(
                            "validator not found at index".into(),
                        ));
                    }
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
                    )
                        .into())
                }),
                validatorCount(_) => {
                    metadata::<IValidatorSet::validatorCountCall>(|| vs.validator_count.read())
                }
                activeValidatorCount(_) => {
                    metadata::<IValidatorSet::activeValidatorCountCall>(|| {
                        vs.active_validator_count()
                    })
                }
                activeConsensusCount(_) => {
                    metadata::<IValidatorSet::activeConsensusCountCall>(|| {
                        vs.active_consensus_count()
                    })
                }
                isValidator(c) => view(c, |c| vs.is_validator(c.addr)),
                isConsensusParticipant(c) => view(c, |c| vs.is_consensus_participant(c.addr)),
                hasPendingSetChange(_) => {
                    metadata::<IValidatorSet::hasPendingSetChangeCall>(|| {
                        vs.has_pending_set_change()
                    })
                }
                getEpochNumber(_) => {
                    metadata::<IValidatorSet::getEpochNumberCall>(|| vs.epoch_number.read())
                }
                getEpochStartTimestamp(_) => {
                    metadata::<IValidatorSet::getEpochStartTimestampCall>(|| {
                        vs.epoch_start_timestamp.read()
                    })
                }
                getEpochStartBlock(_) => metadata::<IValidatorSet::getEpochStartBlockCall>(|| {
                    vs.epoch_start_block.read()
                }),
                registerValidator(c) => mutate_void(c, caller, |sender, c| {
                    if c.consensusPubkey.len() != 48 {
                        return Err(PrecompileError::Revert(
                            "consensus pubkey must be 48 bytes".into(),
                        ));
                    }
                    let pubkey: [u8; 48] = c.consensusPubkey[..48].try_into().map_err(|_| {
                        PrecompileError::Revert("consensus pubkey conversion failed".into())
                    })?;
                    let sig: Option<&[u8; 96]> = if c.blsSignature.len() == 96 {
                        Some(c.blsSignature[..96].try_into().map_err(|_| {
                            PrecompileError::Revert("BLS signature conversion failed".into())
                        })?)
                    } else if c.blsSignature.is_empty() {
                        None
                    } else {
                        return Err(PrecompileError::Revert(
                            "BLS signature must be 96 bytes or empty".into(),
                        ));
                    };
                    vs.register_validator_with_sig(sender, c.validatorAddress, &pubkey, sig)
                }),
                setP2pAddress(c) => mutate_void(c, caller, |sender, c| {
                    vs.set_p2p_address(sender, c.validatorAddress, c.version, &c.encoded)
                }),
                getP2pAddress(c) => view(c, |c| {
                    let (version, encoded) = vs
                        .get_p2p_address(c.validatorAddress)?
                        .unwrap_or((0, Vec::new()));
                    Ok((version, Bytes::from(encoded)).into())
                }),
                deactivateValidator(c) => mutate_void(c, caller, |sender, c| {
                    vs.deactivate_validator(sender, c.validatorAddress)
                }),
                confirmValidatorReady(c) => {
                    mutate_void(c, caller, |sender, _c| vs.confirm_validator_ready(sender))
                }
                activateResharedSet(c) => mutate_void(c, caller, |sender, c| {
                    // Only the config owner (system) can call activateResharedSet
                    let owner = vs.config_owner.read()?;
                    if sender != owner {
                        return Err(PrecompileError::Revert(
                            "unauthorized: only owner can activate reshared set".into(),
                        ));
                    }
                    vs.activate_reshared_set(&c.newActiveSet, c.groupPublicKey)
                }),
            }
        },
    )
}
