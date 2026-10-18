//! Confidential Lysis execution and verified issuance artifacts.

use alloy_primitives::{B256, U256};
use outbe_primitives::{
    nod_encryption::EncryptedNodV2, time::WorldwideDay, tribute_encryption::EncryptedTributeV2,
};
use serde::{Deserialize, Serialize};

use crate::{
    protocol::{EnclaveRequest, EnclaveResponse, FidelitySnapshotRequest},
    response_auth::{attestation, hash},
    TransportError,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisRequest {
    pub chain_id: u64,
    pub worldwide_day: WorldwideDay,
    pub timestamp: u64,
    pub lysis_limit_minor: U256,
    pub tributes: Vec<EncryptedTributeV2>,
    pub fidelity: FidelitySnapshotRequest,
    pub entry_prices: Vec<(u16, Option<U256>)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisNod {
    pub nod: EncryptedNodV2,
    pub settlement_cost_minor: U256,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LysisResult {
    pub nods: Vec<LysisNod>,
    pub total_nominal_minor: U256,
    pub unused_lysis_limit_minor: U256,
}

/// Only the attestation verifier can construct an issuance artifact.
pub struct VerifiedLysisNod(LysisNod);

impl VerifiedLysisNod {
    pub fn nod(&self) -> &EncryptedNodV2 {
        &self.0.nod
    }

    pub fn settlement_cost_minor(&self) -> U256 {
        self.0.settlement_cost_minor
    }
}

pub struct VerifiedLysisResult {
    pub nods: Vec<VerifiedLysisNod>,
    pub total_nominal_minor: U256,
    pub unused_lysis_limit_minor: U256,
    pub inputs_hash: B256,
    pub result_hash: B256,
}

pub struct VerifiedLysisRejection(());

pub enum LysisOutcome {
    Completed(VerifiedLysisResult),
    Rejected(VerifiedLysisRejection),
}

pub fn inputs_hash(request: &LysisRequest) -> Result<B256, TransportError> {
    hash(b"outbe/lysis/inputs", request).map_err(TransportError::codec)
}

pub fn result_preimage(inputs: B256, result: &LysisResult) -> Result<Vec<u8>, TransportError> {
    attestation(b"outbe/lysis/result", inputs, result).map_err(TransportError::codec)
}

pub fn rejection_preimage(inputs: B256) -> Vec<u8> {
    let mut bytes = b"outbe/lysis/calculation-rejected".to_vec();
    bytes.extend_from_slice(inputs.as_slice());
    bytes
}

fn matches_tribute_terms(
    request: &LysisRequest,
    terms: &outbe_primitives::nod_encryption::NodTermsV2,
    context: &outbe_primitives::tribute_encryption::TributeContextV2,
) -> bool {
    let basic_match =
        terms.chain_id == request.chain_id && terms.worldwide_day == request.worldwide_day;
    let identity_match = terms.owner == context.owner && terms.nod_id == context.tribute_id;
    let currency_match = terms.issuance_currency == context.issuance_currency
        && terms.reference_currency == context.reference_currency;
    basic_match && identity_match && currency_match
}

fn is_valid_nod_output(
    request: &LysisRequest,
    output: &LysisNod,
    tribute: &EncryptedTributeV2,
) -> bool {
    let terms = &output.nod.terms;
    let has_entry_price = request
        .entry_prices
        .contains(&(terms.reference_currency, Some(terms.entry_price_minor)));
    output.nod.has_valid_encoding()
        && matches_tribute_terms(request, terms, &tribute.context)
        && has_entry_price
}

fn validate_executed_result(
    request: &LysisRequest,
    result: &LysisResult,
) -> Result<(), TransportError> {
    let valid_bounds = result.nods.len() == request.tributes.len()
        && result.unused_lysis_limit_minor <= request.lysis_limit_minor;
    let valid_nods = result
        .nods
        .iter()
        .zip(&request.tributes)
        .all(|(output, tribute)| is_valid_nod_output(request, output, tribute));
    if valid_bounds && valid_nods {
        Ok(())
    } else {
        Err(TransportError::UnexpectedResponse)
    }
}

pub fn execute(request: LysisRequest) -> Result<LysisOutcome, TransportError> {
    let expected = inputs_hash(&request)?;
    #[cfg(any(test, feature = "test-utils"))]
    let exchange = test_transport::exchange(&request);
    #[cfg(not(any(test, feature = "test-utils")))]
    let exchange: Option<Result<([u8; 32], EnclaveResponse), TransportError>> = None;
    let (key, response) = match exchange {
        Some(response) => response?,
        None => crate::nod_transport::request(EnclaveRequest::ExecuteLysis {
            request: Box::new(request.clone()),
        })?,
    };
    match response {
        EnclaveResponse::LysisRejected {
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            crate::tribute_v2::verify_attestation(
                &key,
                &rejection_preimage(expected),
                &attestation_tag,
            )?;
            Ok(LysisOutcome::Rejected(VerifiedLysisRejection(())))
        }
        EnclaveResponse::LysisExecuted {
            result,
            inputs_canonical_hash,
            attestation_tag,
        } if inputs_canonical_hash == expected => {
            crate::tribute_v2::verify_attestation(
                &key,
                &result_preimage(expected, &result)?,
                &attestation_tag,
            )?;
            validate_executed_result(&request, &result)?;
            let result_hash =
                hash(b"outbe/lysis/output", &result).map_err(TransportError::codec)?;
            Ok(LysisOutcome::Completed(VerifiedLysisResult {
                nods: result.nods.into_iter().map(VerifiedLysisNod).collect(),
                total_nominal_minor: result.total_nominal_minor,
                unused_lysis_limit_minor: result.unused_lysis_limit_minor,
                inputs_hash: expected,
                result_hash,
            }))
        }
        _ => Err(TransportError::UnexpectedResponse),
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test_transport {
    use super::*;
    use std::cell::RefCell;

    type Exchange = fn(&LysisRequest) -> Result<([u8; 32], EnclaveResponse), TransportError>;
    thread_local! {
        static EXCHANGE: RefCell<Option<Exchange>> = const { RefCell::new(None) };
    }
    pub struct Guard {
        previous: Option<Exchange>,
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            EXCHANGE.with(|exchange| exchange.replace(self.previous));
        }
    }
    pub fn scope(exchange: Exchange) -> Guard {
        Guard {
            previous: EXCHANGE.with(|slot| slot.replace(Some(exchange))),
            _thread: std::marker::PhantomData,
        }
    }
    pub fn is_installed() -> bool {
        EXCHANGE.with(|exchange| exchange.borrow().is_some())
    }
    pub(super) fn exchange(
        request: &LysisRequest,
    ) -> Option<Result<([u8; 32], EnclaveResponse), TransportError>> {
        EXCHANGE
            .with(|exchange| *exchange.borrow())
            .map(|exchange| exchange(request))
    }
}

pub fn is_test_transport_installed() -> bool {
    #[cfg(any(test, feature = "test-utils"))]
    {
        test_transport::is_installed()
    }
    #[cfg(not(any(test, feature = "test-utils")))]
    {
        false
    }
}
