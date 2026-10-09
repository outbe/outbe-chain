//! Characterization of the ValidatorSet precompile routing: each selector
//! returns the ABI encoding of its ValidatorSet operation, and each argument
//! check fails with its exact text.

use super::*;
use crate::delegation::ValidatorDelegateRole;
use crate::precompile::{dispatch, IValidatorSet as I};
use alloy_primitives::Bytes;
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

const FIRST: Address = address!("0x00000000000000000000000000000000000000F1");
const SECOND: Address = address!("0x00000000000000000000000000000000000000F2");
const DELEGATE: Address = address!("0x00000000000000000000000000000000000000F3");
const UNKNOWN: Address = address!("0x00000000000000000000000000000000000000FF");

/// The exact text of a dispatch result: `Revert: <message>` or
/// `Fatal: <message>` for a rejection, else the debug form of the result.
fn outcome_text(result: Result<Bytes>) -> String {
    match result {
        Err(PrecompileError::Revert(message)) => format!("Revert: {message}"),
        Err(PrecompileError::Fatal(message)) => format!("Fatal: {message}"),
        other => format!("{other:?}"),
    }
}

/// FIRST is ACTIVE with a live BLS share and DELEGATE as its Oracle
/// delegate. SECOND is registered only.
fn routed_storage() -> Result<HashMapStorageProvider> {
    let mut storage = configured_storage(10);
    storage.enter(|storage| -> Result<()> {
        let mut vs = ValidatorSet::new(storage);
        vs.register_validator(OWNER, FIRST, &dummy_consensus_pubkey(0xF1))?;
        activate_staked_for_test(&mut vs, FIRST);
        vs.val_has_bls_share.write(&FIRST, true)?;
        vs.register_validator(OWNER, SECOND, &dummy_consensus_pubkey(0xF2))?;
        vs.set_delegate(FIRST, ValidatorDelegateRole::Oracle, DELEGATE)
    })?;
    Ok(storage)
}

fn send<T: SolCall>(
    storage: &mut HashMapStorageProvider,
    caller: Address,
    call: &T,
) -> Result<Bytes> {
    storage.enter(|storage| dispatch(storage, &call.abi_encode(), caller, U256::ZERO))
}

/// Requires the dispatch of `call` to return the ABI encoding of `expected`.
fn assert_returns<T: SolCall>(storage: &mut HashMapStorageProvider, call: T, expected: T::Return) {
    let encoded = Bytes::from(T::abi_encode_returns(&expected));
    assert_eq!(
        send(storage, OWNER, &call).ok(),
        Some(encoded),
        "{}",
        T::SIGNATURE
    );
}

#[test]
fn list_and_count_views_route_to_their_queries() -> Result<()> {
    let mut storage = routed_storage()?;
    assert_returns(&mut storage, I::getValidatorsCall {}, vec![FIRST, SECOND]);
    assert_returns(&mut storage, I::getActiveValidatorsCall {}, vec![FIRST]);
    assert_returns(&mut storage, I::getActiveConsensusSetCall {}, vec![FIRST]);
    let (count, active, consensus, pending, epoch, start_time, start_block) =
        storage.enter(|storage| -> Result<_> {
            let vs = ValidatorSet::new(storage);
            Ok((
                vs.validator_count()?,
                vs.active_validator_count()?,
                vs.active_consensus_count()?,
                vs.has_pending_set_change()?,
                vs.epoch_number.read()?,
                vs.epoch_start_timestamp.read()?,
                vs.epoch_start_block.read()?,
            ))
        })?;
    assert_eq!((count, active, consensus), (2, 1, 1));
    assert_returns(&mut storage, I::validatorCountCall {}, count);
    assert_returns(&mut storage, I::activeValidatorCountCall {}, active);
    assert_returns(&mut storage, I::activeConsensusCountCall {}, consensus);
    assert_returns(&mut storage, I::hasPendingSetChangeCall {}, pending);
    assert_returns(&mut storage, I::getEpochNumberCall {}, epoch);
    assert_returns(&mut storage, I::getEpochStartTimestampCall {}, start_time);
    assert_returns(&mut storage, I::getEpochStartBlockCall {}, start_block);
    Ok(())
}

#[test]
fn validator_views_route_by_address_and_index() -> Result<()> {
    let mut storage = routed_storage()?;
    let record = storage
        .enter(|storage| ValidatorSet::new(storage).get_validator(FIRST))?
        .ok_or_else(|| PrecompileError::Fatal("fixture validator is absent".into()))?;
    let by_address = send(
        &mut storage,
        OWNER,
        &I::validatorByAddressCall { addr: FIRST },
    )?;
    let by_index = send(&mut storage, OWNER, &I::validatorByIndexCall { index: 1 })?;
    assert_eq!(by_address, by_index);
    let decoded = I::validatorByAddressCall::abi_decode_returns(&by_address)
        .map_err(|error| PrecompileError::Fatal(format!("decode validator view: {error}")))?;
    assert_eq!(decoded.validatorAddress, record.validator_address);
    assert_eq!(
        decoded.consensusPubkey.as_ref(),
        record.consensus_pubkey.as_slice()
    );
    assert_eq!(
        (
            decoded.stake,
            decoded.status,
            decoded.joinedAtHeight,
            decoded.hasBLSShare
        ),
        (
            record.stake,
            record.status,
            record.joined_at_height,
            record.has_bls_share
        )
    );
    assert_eq!(
        outcome_text(send(
            &mut storage,
            OWNER,
            &I::validatorByAddressCall { addr: UNKNOWN }
        )),
        "Revert: validator not found"
    );
    assert_eq!(
        outcome_text(send(
            &mut storage,
            OWNER,
            &I::validatorByIndexCall { index: 9 }
        )),
        "Revert: validator not found at index"
    );
    assert_returns(&mut storage, I::isValidatorCall { addr: SECOND }, true);
    assert_returns(
        &mut storage,
        I::isConsensusParticipantCall { addr: SECOND },
        false,
    );
    assert_returns(
        &mut storage,
        I::isConsensusParticipantCall { addr: FIRST },
        true,
    );
    Ok(())
}

#[test]
fn delegate_routes_decode_the_role_first() -> Result<()> {
    let mut storage = routed_storage()?;
    let oracle = ValidatorDelegateRole::Oracle.id();
    assert_returns(
        &mut storage,
        I::getDelegateCall {
            validator: FIRST,
            role: oracle,
        },
        DELEGATE,
    );
    assert_returns(
        &mut storage,
        I::resolveValidatorCall {
            signer: DELEGATE,
            role: oracle,
        },
        FIRST,
    );
    assert_returns(
        &mut storage,
        I::resolveValidatorCall {
            signer: UNKNOWN,
            role: oracle,
        },
        Address::ZERO,
    );
    let unsupported = "Revert: unsupported validator delegate role";
    let bad_role = 9;
    assert_eq!(
        outcome_text(send(
            &mut storage,
            OWNER,
            &I::getDelegateCall {
                validator: FIRST,
                role: bad_role
            }
        )),
        unsupported
    );
    assert_eq!(
        outcome_text(send(
            &mut storage,
            FIRST,
            &I::revokeDelegateCall { role: bad_role }
        )),
        unsupported
    );
    assert_eq!(
        outcome_text(send(
            &mut storage,
            FIRST,
            &I::revokeDelegateCall { role: oracle }
        )),
        "Ok(0x)"
    );
    assert_returns(
        &mut storage,
        I::resolveValidatorCall {
            signer: DELEGATE,
            role: oracle,
        },
        Address::ZERO,
    );
    Ok(())
}

#[test]
fn registration_route_checks_key_length_before_proof_length() -> Result<()> {
    let mut storage = routed_storage()?;
    let register = |key_len: usize, proof_len: usize| I::registerValidatorCall {
        validatorAddress: UNKNOWN,
        consensusPubkey: vec![0xF4; key_len].into(),
        radicleNodeId: B256::repeat_byte(0xF4),
        blsRegistrationSignature: vec![0xF5; proof_len].into(),
    };
    assert_eq!(
        outcome_text(send(&mut storage, OWNER, &register(47, 95))),
        "Revert: consensus pubkey must be 48 bytes"
    );
    assert_eq!(
        outcome_text(send(&mut storage, OWNER, &register(48, 95))),
        "Revert: BLS proof of possession must be exactly 96 bytes"
    );
    assert_eq!(
        outcome_text(send(&mut storage, OWNER, &register(48, 96))),
        "Revert: invalid BLS public key"
    );
    Ok(())
}

#[test]
fn p2p_lifecycle_and_value_routes_keep_their_results() -> Result<()> {
    let mut storage = routed_storage()?;
    let unset = I::getP2pAddressCall {
        validatorAddress: FIRST,
    };
    let encoded = send(&mut storage, OWNER, &unset)?;
    assert_eq!(
        encoded,
        Bytes::from(I::getP2pAddressCall::abi_encode_returns(
            &(0u8, Bytes::new()).into()
        ))
    );
    let data = I::deactivateValidatorCall {
        validatorAddress: FIRST,
    }
    .abi_encode();
    assert_eq!(
        outcome_text(storage.enter(|storage| dispatch(storage, &data, OWNER, U256::from(1)))),
        "Revert: non-payable function called with value"
    );
    assert_eq!(
        outcome_text(send(
            &mut storage,
            OWNER,
            &I::deactivateValidatorCall {
                validatorAddress: FIRST
            }
        )),
        "Ok(0x)"
    );
    assert_eq!(
        outcome_text(send(
            &mut storage,
            FIRST,
            &I::confirmValidatorReadyCall {
                registration: Bytes::new()
            }
        )),
        "Revert: confirmValidatorReady requires PENDING status, got 3"
    );
    Ok(())
}
