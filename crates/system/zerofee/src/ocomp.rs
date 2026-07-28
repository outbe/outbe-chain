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

/// Fixed compute ceiling for one signature verification and four bounded
/// consensus vote slots.
pub const MAX_ZERO_FEE_OCOMP_GAS_LIMIT: u64 =
    outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1.max_activation_gas;

/// Public txpool compatibility floor. The executor waives the actual debit
/// only after stateful validator authorization.
pub const MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS: u128 = MIN_PROTOCOL_BASE_FEE as u128;

#[derive(Debug, Clone, Copy)]
pub struct OcompSubmitResultVoteHook;

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
        validate_dynamic_vote_envelope(tx.input)?;
        Ok(Some(ZeroFeeCandidate::new(self.id(), tx.signer)))
    }

    fn authorize_fee_waiver(
        &self,
        storage: StorageHandle,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        let validators = outbe_validatorset::contract::ValidatorSet::new(storage);
        let Some(record) = validators.get_validator(candidate.signer)? else {
            return Err(ZeroFeePolicyError::UnauthorizedSigner);
        };
        if record.status != outbe_validatorset::logic::status::ACTIVE || !record.has_bls_share {
            return Err(ZeroFeePolicyError::UnauthorizedSigner);
        }
        Ok(ZeroFeeAuthorization {
            hook: self.id(),
            subject: candidate.signer,
        })
    }
}

fn validate_dynamic_vote_envelope(input: &[u8]) -> Result<(), ZeroFeePolicyError> {
    if input.len() < 68 || U256::from_be_slice(&input[4..36]) != U256::from(32) {
        return Err(malformed());
    }
    let payload_len =
        usize::try_from(U256::from_be_slice(&input[36..68])).map_err(|_| malformed())?;
    let vote_cap = usize::try_from(
        outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1.max_result_vote_bytes,
    )
    .map_err(|_| malformed())?;
    if payload_len == 0 || payload_len > vote_cap {
        return Err(malformed());
    }
    let padded_len = payload_len
        .checked_add(31)
        .map(|value| value & !31)
        .ok_or_else(malformed)?;
    let expected_len = 68_usize.checked_add(padded_len).ok_or_else(malformed)?;
    if input.len() != expected_len {
        return Err(malformed());
    }
    let payload_end = 68 + payload_len;
    if input[payload_end..].iter().any(|byte| *byte != 0) {
        return Err(malformed());
    }
    Ok(())
}

fn malformed() -> ZeroFeePolicyError {
    ZeroFeePolicyError::MalformedCalldata(
        "submitLysisResult(bytes) canonical ABI envelope failed".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{address, Address};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    const VALIDATOR: Address = address!("0x1111111111111111111111111111111111111111");

    fn calldata(payload_len: usize) -> Vec<u8> {
        let padded_len = (payload_len + 31) & !31;
        let mut input = vec![0_u8; 68 + padded_len];
        input[..4].copy_from_slice(&SUBMIT_LYSIS_RESULT_SELECTOR);
        input[4..36].copy_from_slice(&U256::from(32).to_be_bytes::<32>());
        input[36..68].copy_from_slice(&U256::from(payload_len).to_be_bytes::<32>());
        input
    }

    fn tx(input: &[u8]) -> ZeroFeeTransaction<'_> {
        ZeroFeeTransaction {
            signer: VALIDATOR,
            to: Some(METADOSIS_ADDRESS),
            value: U256::ZERO,
            input,
            gas_limit: MAX_ZERO_FEE_OCOMP_GAS_LIMIT,
            max_fee_per_gas: MIN_ZERO_FEE_OCOMP_MAX_FEE_PER_GAS,
            max_priority_fee_per_gas: Some(0),
        }
    }

    #[test]
    fn exact_bounded_result_vote_envelope_is_classified() {
        let vote_cap = usize::try_from(
            outbe_ocomp_protocol::generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1
                .max_result_vote_bytes,
        )
        .unwrap();
        let input = calldata(vote_cap);
        let candidate = crate::registry().classify(&tx(&input)).unwrap().unwrap();
        assert_eq!(candidate.hook, ZeroFeeHookId::OcompSubmitResultVote);
        assert_eq!(candidate.signer, VALIDATOR);
    }

    #[test]
    fn malformed_or_oversized_result_vote_envelope_is_rejected() {
        let mut malformed_padding = calldata(1);
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
        let oversized = calldata(vote_cap + 1);
        assert!(matches!(
            crate::registry().classify(&tx(&oversized)),
            Err(ZeroFeePolicyError::CalldataTooLarge { .. })
        ));
    }

    #[test]
    fn only_active_consensus_validator_receives_fee_waiver() {
        let input = calldata(1);
        let candidate = crate::registry().classify(&tx(&input)).unwrap().unwrap();
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            assert_eq!(
                crate::registry()
                    .authorize_fee_waiver(storage.clone(), candidate)
                    .unwrap_err(),
                ZeroFeePolicyError::UnauthorizedSigner
            );

            let validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
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

            let authorization = crate::registry()
                .authorize_fee_waiver(storage, candidate)
                .unwrap();
            assert_eq!(authorization.subject, VALIDATOR);
        });
    }
}
