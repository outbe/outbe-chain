use alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE;
use alloy_primitives::U256;
use outbe_ocomp_protocol::abi::{METADOSIS_ADDRESS, SUBMIT_LYSIS_RESULT_SELECTOR};
use outbe_primitives::storage::StorageHandle;

use crate::hooks::{
    ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeeHook, ZeroFeeHookId, ZeroFeePolicyError,
    ZeroFeeTransaction,
};

/// Exact canonical ABI envelope cap for `submitLysisResult(bytes)`:
/// selector + offset + length + padded frozen OCB1 vote bytes.
pub const MAX_ZERO_FEE_OCOMP_CALLDATA_BYTES: usize = 68
    + ((outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1.max_result_vote_bytes
        as usize
        + 31)
        & !31);

/// Consensus-generated compute ceiling for one signature verification and the
/// validator set bounded vote-accountability object.
pub const MAX_ZERO_FEE_OCOMP_GAS_LIMIT: u64 =
    outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_gas;

/// Public txpool compatibility floor. The executor waives the actual debit
/// only after stateful validator authorization.
pub const MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS: u128 = MIN_PROTOCOL_BASE_FEE as u128;

#[derive(Debug, Clone, Copy)]
pub struct OcompSubmitResultVoteHook;

fn resolve_historical_ocomp_signer(
    validators: &outbe_validatorset::contract::ValidatorSet<'_>,
    historical_validator: alloy_primitives::Address,
    signer: alloy_primitives::Address,
) -> Result<Option<alloy_primitives::Address>, ZeroFeePolicyError> {
    let role = outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp;
    let explicit = validators.get_delegate(historical_validator, role)?;
    if signer == historical_validator {
        return Ok(explicit.is_zero().then_some(historical_validator));
    }
    if explicit != signer {
        return Ok(None);
    }
    let reverse = validators
        .validator_by_role_delegate
        .get_nested(&role.id())
        .read(&signer)?;
    Ok((reverse == historical_validator).then_some(historical_validator))
}

impl ZeroFeeHook for OcompSubmitResultVoteHook {
    fn id(&self) -> ZeroFeeHookId {
        ZeroFeeHookId::OcompSubmitResultVote
    }

    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        if tx.to != Some(METADOSIS_ADDRESS) {
            return Ok(None);
        }
        if tx.input.get(..4) != Some(SUBMIT_LYSIS_RESULT_SELECTOR.as_slice()) {
            return Ok(None);
        }
        if tx.max_priority_fee_per_gas != Some(0) {
            return Ok(None);
        }
        if tx.max_fee_per_gas < MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS {
            return Err(ZeroFeePolicyError::FeeCapTooLow {
                max_fee_per_gas: tx.max_fee_per_gas,
                minimum: MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS,
            });
        }
        if tx.value != U256::ZERO {
            return Err(ZeroFeePolicyError::NonZeroValue);
        }
        if tx.input.len() > MAX_ZERO_FEE_OCOMP_CALLDATA_BYTES {
            return Err(ZeroFeePolicyError::CalldataTooLarge {
                size: tx.input.len(),
                limit: MAX_ZERO_FEE_OCOMP_CALLDATA_BYTES,
            });
        }
        if tx.gas_limit > MAX_ZERO_FEE_OCOMP_GAS_LIMIT {
            return Err(ZeroFeePolicyError::GasLimitTooHigh {
                gas_limit: tx.gas_limit,
                limit: MAX_ZERO_FEE_OCOMP_GAS_LIMIT,
            });
        }
        let prefix = outbe_ocomp_protocol::vote::decode_submit_lysis_result_prefix(
            tx.input,
            &outbe_ocomp_protocol::profile::poc_schema_limits(),
        )
        .map_err(|_| malformed())?;
        Ok(Some(ZeroFeeCandidate::new_ocomp_vote(
            self.id(),
            tx.signer,
            prefix,
        )))
    }

    fn authorize_fee_waiver(
        &self,
        storage: StorageHandle,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        let prefix = candidate
            .ocomp_vote_prefix()
            .ok_or(ZeroFeePolicyError::UnauthorizedSigner)?;
        let historical_validator = outbe_metadosis::resolve_historical_result_vote_participant(
            storage.clone(),
            &prefix,
            &outbe_ocomp_protocol::profile::poc_schema_limits(),
        )?
        .ok_or(ZeroFeePolicyError::UnauthorizedSigner)?;
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage);
        let validator =
            resolve_historical_ocomp_signer(&validators, historical_validator, candidate.signer)?
                .ok_or(ZeroFeePolicyError::UnauthorizedSigner)?;
        Ok(ZeroFeeAuthorization {
            hook: self.id(),
            subject: validator,
        })
    }
}

fn malformed() -> ZeroFeePolicyError {
    ZeroFeePolicyError::MalformedCalldata(
        "submitLysisResult(bytes) canonical ABI envelope failed".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address, B256};
    use outbe_ocomp_protocol::{
        encode_envelope, profile::poc_schema_limits, registry::ObjectKind,
        vote::ResultVotePrefixV1, OCB1_HEADER_LEN,
    };
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    const VALIDATOR: Address = address!("0x1111111111111111111111111111111111111111");
    const DELEGATE: Address = address!("0x2222222222222222222222222222222222222222");

    fn vote_prefix() -> ResultVotePrefixV1 {
        ResultVotePrefixV1 {
            protocol_bundle_hash: B256::repeat_byte(0x31),
            job_id: B256::repeat_byte(0x32),
            attempt: 3,
            result_validator_set_epoch: 7,
            result_committee_set_hash: B256::repeat_byte(0x33),
            result_ocomp_binding_hash: B256::repeat_byte(0x34),
            validator_index: 1,
            key_epoch: 1,
        }
    }

    fn canonical_vote_payload(encoded_len: usize) -> Vec<u8> {
        let prefix = vote_prefix();
        let mut body = Vec::new();
        body.extend_from_slice(prefix.protocol_bundle_hash.as_slice());
        body.extend_from_slice(prefix.job_id.as_slice());
        body.extend_from_slice(&prefix.attempt.to_be_bytes());
        body.extend_from_slice(&prefix.result_validator_set_epoch.to_be_bytes());
        body.extend_from_slice(prefix.result_committee_set_hash.as_slice());
        body.extend_from_slice(prefix.result_ocomp_binding_hash.as_slice());
        body.extend_from_slice(&prefix.validator_index.to_be_bytes());
        body.extend_from_slice(&prefix.key_epoch.to_be_bytes());
        assert!(encoded_len >= OCB1_HEADER_LEN + body.len());
        body.resize(encoded_len - OCB1_HEADER_LEN, 0);
        encode_envelope(ObjectKind::ResultVoteV1, &body, poc_schema_limits().codec).unwrap()
    }

    fn calldata_from_payload(payload: &[u8]) -> Vec<u8> {
        let padded_len = (payload.len() + 31) & !31;
        let mut input = vec![0_u8; 68 + padded_len];
        input[..4].copy_from_slice(&SUBMIT_LYSIS_RESULT_SELECTOR);
        input[4..36].copy_from_slice(&U256::from(32).to_be_bytes::<32>());
        input[36..68].copy_from_slice(&U256::from(payload.len()).to_be_bytes::<32>());
        input[68..68 + payload.len()].copy_from_slice(payload);
        input
    }

    fn canonical_calldata(encoded_len: usize) -> Vec<u8> {
        calldata_from_payload(&canonical_vote_payload(encoded_len))
    }

    fn minimal_canonical_calldata() -> Vec<u8> {
        canonical_calldata(OCB1_HEADER_LEN + 150)
    }

    fn tx_from(signer: Address, input: &[u8]) -> ZeroFeeTransaction<'_> {
        ZeroFeeTransaction {
            signer,
            to: Some(METADOSIS_ADDRESS),
            value: U256::ZERO,
            input,
            gas_limit: MAX_ZERO_FEE_OCOMP_GAS_LIMIT,
            max_fee_per_gas: MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: Some(0),
        }
    }

    fn tx(input: &[u8]) -> ZeroFeeTransaction<'_> {
        tx_from(VALIDATOR, input)
    }

    #[test]
    fn exact_bounded_result_vote_envelope_is_classified() {
        let vote_cap = usize::try_from(
            outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1
                .max_result_vote_bytes,
        )
        .unwrap();
        let input = canonical_calldata(vote_cap);
        let candidate = crate::registry().classify(&tx(&input)).unwrap().unwrap();
        assert_eq!(candidate.hook, ZeroFeeHookId::OcompSubmitResultVote);
        assert_eq!(candidate.signer, VALIDATOR);
    }

    #[test]
    fn classified_candidate_carries_the_canonical_vote_prefix() {
        let input = minimal_canonical_calldata();
        let candidate = crate::registry().classify(&tx(&input)).unwrap().unwrap();
        assert_eq!(candidate.ocomp_vote_prefix(), Some(vote_prefix()));
    }

    #[test]
    fn malformed_or_oversized_result_vote_envelope_is_rejected() {
        let mut malformed_padding = minimal_canonical_calldata();
        *malformed_padding.last_mut().unwrap() = 1;
        assert!(matches!(
            crate::registry().classify(&tx(&malformed_padding)),
            Err(ZeroFeePolicyError::MalformedCalldata(_))
        ));

        let vote_cap = usize::try_from(
            outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1
                .max_result_vote_bytes,
        )
        .unwrap();
        let oversized = calldata_from_payload(&vec![0_u8; vote_cap + 1]);
        assert!(matches!(
            crate::registry().classify(&tx(&oversized)),
            Err(ZeroFeePolicyError::CalldataTooLarge { .. })
        ));
    }

    #[test]
    fn current_active_status_without_a_matching_open_job_cannot_authorize_a_waiver() {
        let input = minimal_canonical_calldata();
        let candidate = crate::registry().classify(&tx(&input)).unwrap().unwrap();
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
            validators.validator_count.write(1).unwrap();
            validators.address_to_index.write(&VALIDATOR, 1).unwrap();
            validators.index_to_address.write(&1, VALIDATOR).unwrap();
            validators
                .val_status
                .write(&VALIDATOR, outbe_validatorset::logic::status::ACTIVE)
                .unwrap();
            validators
                .val_has_bls_share
                .write(&VALIDATOR, true)
                .unwrap();

            assert_eq!(
                crate::registry()
                    .authorize_fee_waiver(storage.clone(), candidate)
                    .unwrap_err(),
                ZeroFeePolicyError::UnauthorizedSigner
            );

            validators
                .set_delegate(
                    VALIDATOR,
                    outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp,
                    DELEGATE,
                )
                .unwrap();
            let delegated = crate::registry()
                .classify(&tx_from(DELEGATE, &input))
                .unwrap()
                .unwrap();
            assert_eq!(
                crate::registry()
                    .authorize_fee_waiver(storage, delegated)
                    .unwrap_err(),
                ZeroFeePolicyError::UnauthorizedSigner
            );
        });
    }

    #[test]
    fn historical_participant_or_its_explicit_delegate_is_authorized_without_active_status() {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let mut validators = outbe_validatorset::contract::ValidatorSet::new(storage);
            validators.validator_count.write(1).unwrap();
            validators.address_to_index.write(&VALIDATOR, 1).unwrap();
            validators.index_to_address.write(&1, VALIDATOR).unwrap();
            validators
                .val_status
                .write(&VALIDATOR, outbe_validatorset::logic::status::INACTIVE)
                .unwrap();
            validators
                .val_has_bls_share
                .write(&VALIDATOR, false)
                .unwrap();

            assert_eq!(
                resolve_historical_ocomp_signer(&validators, VALIDATOR, VALIDATOR).unwrap(),
                Some(VALIDATOR)
            );

            validators
                .set_delegate(
                    VALIDATOR,
                    outbe_validatorset::delegation::ValidatorDelegateRole::Ocomp,
                    DELEGATE,
                )
                .unwrap();
            assert_eq!(
                resolve_historical_ocomp_signer(&validators, VALIDATOR, DELEGATE).unwrap(),
                Some(VALIDATOR)
            );
            assert_eq!(
                resolve_historical_ocomp_signer(&validators, VALIDATOR, VALIDATOR).unwrap(),
                None
            );
        });
    }
}
