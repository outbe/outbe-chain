//! Witness-recorded NOD amount, group-load, and quote calls.
//!
//! Payload bytes are `tag ||` canonical postcard of the schema, with an empty
//! inner `attestation_tag`. The witness entry stores the inputs hash and the
//! Ed25519 signature beside that payload.

use alloy_primitives::{B256, U256};
use outbe_core_common::confidential_tee::{
    attestation, consensus_witness_call, derive_resident_state_key, hash, sign_attestation_tag,
    verify_response_attestation, verify_signed_domain, ResponseAuthSpec, WitnessResponseTag,
    WitnessSchema, NOD, OFFER_HKDF_SALT,
};
use outbe_core_common::nod_encryption::{
    EncryptedNodGroupLoadV1, EncryptedNodV1, NodQuoteContextV1,
};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::resident::StateKeyHandle;
use outbe_tee::{AppErrorDescriptor, CallMeta, TransportError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::nod_encryption::{
    self, nod_group_load_attestation_preimage, nod_group_load_inputs_hash,
    nod_group_load_read_hash, nod_group_load_read_preimage, nod_quote_inputs_hash, nod_read_hash,
    nod_read_preimage, NodGroupLoadOpRequestV1,
};
use crate::offer::issue::{IssueNodAtOfferRequestV1, NodAtOfferResultV1, NodAtOfferStatusV1};

const NOD_ATTESTATION: AppErrorDescriptor =
    AppErrorDescriptor::new("nod_attestation", "nod attestation signature invalid");

/// Resident chain or runtime failure for a NOD host call.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NodHostError {
    /// The request chain is not the resident manifest chain.
    #[error("request chain differs from resident chain")]
    ChainMismatch,
    /// The resident chain identifier does not fit in `u64`.
    #[error("resident chain identifier exceeds u64")]
    ChainOverflow,
    /// `session_witness_call` found no installed runtime.
    #[error("in-process TEE runtime is not installed")]
    RuntimeMissing,
    /// The predecessor record has no version, or the next version overflows.
    #[error("invalid Nod group load version")]
    InvalidGroupLoadVersion,
}

/// `NodAmountReadV1` body. Tag byte is [`WitnessResponseTag::NodAmountReadV1`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodAmountReadV1 {
    /// Decrypted Gratis amount.
    pub amount: U256,
    /// Canonical commitment over the encrypted NOD.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// `NodGroupLoadOpAppliedV1` body. Tag byte is 19.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodGroupLoadOpAppliedV1 {
    /// Updated encrypted group-load record.
    pub record: EncryptedNodGroupLoadV1,
    /// Canonical commitment over the operation request.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// `NodGroupLoadReadV1` body. Tag byte is 20.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodGroupLoadReadV1 {
    /// Decrypted group-load total.
    pub amount: U256,
    /// Canonical commitment over the encrypted record.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// `NodQuoteEncryptedV1` body. Tag byte is 21.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodQuoteEncryptedV1 {
    /// Encrypted settlement payment.
    pub encrypted_payment: Vec<u8>,
    /// Canonical commitment over the quote inputs.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

/// `NodIssuedAtOfferV1` body. Tag byte is 22.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodIssuedAtOfferV1 {
    /// Outcome of the offer, with its Nod when issued.
    pub result: NodAtOfferResultV1,
    /// Canonical commitment over the request.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature. Empty inside the postcard body.
    pub attestation_tag: Vec<u8>,
}

macro_rules! nod_schema {
    ($ty:ty, $tag:expr) => {
        impl WitnessSchema for $ty {
            const TAG: WitnessResponseTag = $tag;

            fn inputs_canonical_hash(&self) -> B256 {
                self.inputs_canonical_hash
            }

            fn attestation_tag(&self) -> &[u8] {
                &self.attestation_tag
            }

            fn set_attestation_tag(&mut self, tag: Vec<u8>) {
                self.attestation_tag = tag;
            }
        }
    };
}

nod_schema!(NodAmountReadV1, WitnessResponseTag::NodAmountReadV1);
nod_schema!(
    NodGroupLoadOpAppliedV1,
    WitnessResponseTag::NodGroupLoadOpAppliedV1
);
nod_schema!(NodGroupLoadReadV1, WitnessResponseTag::NodGroupLoadReadV1);
nod_schema!(NodQuoteEncryptedV1, WitnessResponseTag::NodQuoteEncryptedV1);
nod_schema!(NodIssuedAtOfferV1, WitnessResponseTag::NodIssuedAtOfferV1);

const ISSUE_AT_OFFER_INPUTS: &[u8] = b"outbe/nod/issue-at-offer-inputs/v1";
const ISSUE_AT_OFFER_RESULT: &[u8] = b"outbe/nod/issue-at-offer-result/v1";

/// Caller-side binding for a group-load witness entry.
#[derive(Clone, Copy)]
struct GroupLoadExpect {
    inputs: B256,
    chain_id: u64,
    scope: B256,
    version: u64,
}

/// Read the decrypted Gratis amount inside an [`EncryptedNodV1`]. Tag 10.
pub fn read_nod_amount(nod: &EncryptedNodV1) -> Result<U256, TransportError> {
    let expected = nod_read_hash(nod).map_err(TransportError::codec)?;
    let nod = nod.clone();
    let read = recorded(
        "read_nod_amount_v1",
        move |context| execute_amount_read(context, &nod, expected),
        move |key, value| auth_amount_read(key, expected, value),
    )?;
    Ok(read.amount)
}

/// Apply an arithmetic transition and record tag 19.
pub fn apply_nod_group_load_operation(
    request: NodGroupLoadOpRequestV1,
) -> Result<EncryptedNodGroupLoadV1, TransportError> {
    let expected = GroupLoadExpect {
        inputs: nod_group_load_inputs_hash(&request).map_err(TransportError::codec)?,
        chain_id: request.chain_id,
        scope: request.scope,
        version: expected_version(&request)?,
    };
    let applied = recorded(
        "apply_nod_group_load_op_v1",
        move |context| execute_group_load(context, &request, expected),
        move |key, value| auth_group_load(key, expected, value),
    )?;
    Ok(applied.record)
}

/// Read the decrypted load total inside an [`EncryptedNodGroupLoadV1`]. Tag 20.
pub fn read_nod_group_load(record: &EncryptedNodGroupLoadV1) -> Result<U256, TransportError> {
    let expected = nod_group_load_read_hash(record).map_err(TransportError::codec)?;
    let record = record.clone();
    let read = recorded(
        "read_nod_group_load_v1",
        move |context| execute_group_read(context, &record, expected),
        move |key, value| auth_group_read(key, expected, value),
    )?;
    Ok(read.amount)
}

/// Encrypt a NOD quote payment and record tag 21.
pub fn encrypt_nod_quote(
    nod: &EncryptedNodV1,
    context: NodQuoteContextV1,
    payment_minor: U256,
) -> Result<Vec<u8>, TransportError> {
    let expected =
        nod_quote_inputs_hash(nod, &context, payment_minor).map_err(TransportError::codec)?;
    let nod = nod.clone();
    let sealed = recorded(
        "encrypt_nod_quote_v1",
        move |runtime| execute_quote(runtime, &nod, &context, payment_minor, expected),
        move |key, value| auth_quote(key, expected, value),
    )?;
    Ok(sealed.encrypted_payment)
}

/// Decrypt one offer, price it and encrypt its Nod. Tag 22.
pub fn issue_nod_at_offer(
    request: IssueNodAtOfferRequestV1,
) -> Result<NodAtOfferResultV1, TransportError> {
    let expected = hash(ISSUE_AT_OFFER_INPUTS, &request).map_err(TransportError::codec)?;
    let terms_request = request.clone();
    let issued = recorded(
        "issue_nod_at_offer_v1",
        move |context| execute_issue_at_offer(context, &request, expected),
        move |key, value| auth_issue_at_offer(key, expected, &terms_request, value),
    )?;
    Ok(issued.result)
}

/// What the enclave opened from the canary offer. Session-only, not witness-recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferCanaryV1 {
    /// The `creator` field of the decrypted payload.
    pub creator: String,
    /// The canonical amount of the decrypted payload, in minor units.
    pub amount_minor: U256,
    /// `inputs_canonical_hash` of the one canary offer.
    pub inputs_canonical_hash: B256,
    /// Ed25519 signature over the canary result.
    pub attestation_tag: Vec<u8>,
}

const OFFER_CANARY_RESULT: &[u8] = b"outbe/nod/offer-canary-result/v1";

/// Open `offer` with the resident offer key of `context` and sign what it holds.
pub fn open_offer_canary_with(
    context: &InProcessTeeContext,
    offer: &crate::offer::protocol::EncryptedOffer,
) -> Result<OfferCanaryV1, TransportError> {
    let expected = crate::offer::protocol::inputs_canonical_hash(std::slice::from_ref(offer));
    let payload = context
        .with_offer_key_handle(|handle| {
            let material = crate::offer::OfferKeyMaterial {
                offer_private_key: handle,
                salt: &OFFER_HKDF_SALT,
            };
            Ok::<_, TransportError>(crate::offer::open_offer(&material, offer).map_err(reject))
        })
        .map_err(|err| {
            TransportError::from(outbe_tee::InProcessDispatchError::Resident(err))
        })???;
    let amount_minor =
        crate::offer::compute::parse_canonical_amount(&payload.amount_base, &payload.amount_micro)
            .map_err(reject)?
            .amount_minor;
    let preimage = attestation(
        OFFER_CANARY_RESULT,
        expected,
        &(payload.creator.as_str(), amount_minor),
    )
    .map_err(TransportError::codec)?;
    Ok(OfferCanaryV1 {
        creator: payload.creator,
        amount_minor,
        inputs_canonical_hash: expected,
        attestation_tag: sign_bytes(context, &preimage),
    })
}

/// Verify the signature of a canary result against `expected` inputs.
pub fn verify_offer_canary(
    attestation_pub: &[u8; 32],
    expected: B256,
    value: &OfferCanaryV1,
) -> Result<(), TransportError> {
    verify_signed_domain(
        &auth_spec(
            attestation_pub,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        OFFER_CANARY_RESULT,
        &(value.creator.as_str(), value.amount_minor),
    )
}

fn recorded<T: WitnessSchema>(
    label: &'static str,
    execute: impl FnOnce(&InProcessTeeContext) -> Result<T, TransportError>,
    authenticate: impl Fn(&[u8; 32], &T) -> Result<(), TransportError>,
) -> Result<T, TransportError> {
    consensus_witness_call(CallMeta::recorded(label), execute, authenticate)
}

fn execute_amount_read(
    context: &InProcessTeeContext,
    nod: &EncryptedNodV1,
    expected: B256,
) -> Result<NodAmountReadV1, TransportError> {
    require_resident_chain(context, nod.terms.chain_id)?;
    let amount = nod_encryption::decrypt_nod(&nod_state_key(context)?, nod).map_err(reject)?;
    let value = NodAmountReadV1 {
        amount,
        inputs_canonical_hash: expected,
        attestation_tag: sign_bytes(context, &nod_read_preimage(expected, amount)),
    };
    auth_amount_read(&context.attestation_public(), expected, &value)?;
    Ok(value)
}

fn execute_group_load(
    context: &InProcessTeeContext,
    request: &NodGroupLoadOpRequestV1,
    expected: GroupLoadExpect,
) -> Result<NodGroupLoadOpAppliedV1, TransportError> {
    require_resident_chain(context, request.chain_id)?;
    let record = nod_encryption::apply_nod_group_load_operation(&nod_state_key(context)?, request)
        .map_err(reject)?;
    let preimage = nod_group_load_attestation_preimage(expected.inputs, &record)
        .map_err(TransportError::codec)?;
    let value = NodGroupLoadOpAppliedV1 {
        record,
        inputs_canonical_hash: expected.inputs,
        attestation_tag: sign_bytes(context, &preimage),
    };
    auth_group_load(&context.attestation_public(), expected, &value)?;
    Ok(value)
}

fn execute_group_read(
    context: &InProcessTeeContext,
    record: &EncryptedNodGroupLoadV1,
    expected: B256,
) -> Result<NodGroupLoadReadV1, TransportError> {
    require_resident_chain(context, record.chain_id)?;
    let amount =
        nod_encryption::read_nod_group_load(&nod_state_key(context)?, record).map_err(reject)?;
    let value = NodGroupLoadReadV1 {
        amount,
        inputs_canonical_hash: expected,
        attestation_tag: sign_bytes(context, &nod_group_load_read_preimage(expected, amount)),
    };
    auth_group_read(&context.attestation_public(), expected, &value)?;
    Ok(value)
}

fn execute_quote(
    context: &InProcessTeeContext,
    nod: &EncryptedNodV1,
    quote: &NodQuoteContextV1,
    payment_minor: U256,
    expected: B256,
) -> Result<NodQuoteEncryptedV1, TransportError> {
    require_resident_chain(context, quote.chain_id)?;
    let encrypted_payment =
        nod_encryption::encrypt_nod_quote(&nod_state_key(context)?, nod, quote, payment_minor)
            .map_err(reject)?;
    let preimage = attestation(
        b"outbe/nod/quote-encrypt-result/v1",
        expected,
        &encrypted_payment,
    )
    .map_err(TransportError::codec)?;
    let value = NodQuoteEncryptedV1 {
        encrypted_payment,
        inputs_canonical_hash: expected,
        attestation_tag: sign_bytes(context, &preimage),
    };
    auth_quote(&context.attestation_public(), expected, &value)?;
    Ok(value)
}

fn execute_issue_at_offer(
    context: &InProcessTeeContext,
    request: &IssueNodAtOfferRequestV1,
    expected: B256,
) -> Result<NodIssuedAtOfferV1, TransportError> {
    require_resident_chain(context, request.chain_id)?;
    let key = nod_state_key(context)?;
    let result = context
        .with_offer_key_handle(|handle| {
            let material = crate::offer::OfferKeyMaterial {
                offer_private_key: handle,
                salt: &OFFER_HKDF_SALT,
            };
            Ok::<_, TransportError>(crate::offer::issue::issue_nod_at_offer(
                &material,
                &key,
                request.chain_id,
                request,
            ))
        })
        .map_err(|err| TransportError::from(outbe_tee::InProcessDispatchError::Resident(err)))??;
    let preimage =
        attestation(ISSUE_AT_OFFER_RESULT, expected, &result).map_err(TransportError::codec)?;
    let value = NodIssuedAtOfferV1 {
        result,
        inputs_canonical_hash: expected,
        attestation_tag: sign_bytes(context, &preimage),
    };
    auth_issue_at_offer(&context.attestation_public(), expected, request, &value)?;
    Ok(value)
}

fn auth_issue_at_offer(
    key: &[u8; 32],
    expected: B256,
    request: &IssueNodAtOfferRequestV1,
    value: &NodIssuedAtOfferV1,
) -> Result<(), TransportError> {
    verify_signed_domain(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        ISSUE_AT_OFFER_RESULT,
        &value.result,
    )?;
    let issued = matches!(value.result.status, NodAtOfferStatusV1::Issued);
    let matches = match &value.result.nod {
        Some(nod) => issued && has_requested_terms(&nod.terms, request),
        None => !issued,
    };
    if matches {
        return Ok(());
    }
    Err(TransportError::UnexpectedResponse)
}

fn has_requested_terms(
    terms: &outbe_core_common::nod_encryption::NodTermsV1,
    request: &IssueNodAtOfferRequestV1,
) -> bool {
    (
        terms.chain_id,
        terms.owner,
        terms.emission_day,
        terms.entry_price_minor,
        terms.issuance_currency,
        terms.reference_currency,
    ) == (
        request.chain_id,
        request.offer.owner,
        request.emission_day,
        request.entry_price_minor,
        request.offer.tribute_currency,
        request.offer.reference_currency,
    )
}

fn auth_amount_read(
    key: &[u8; 32],
    expected: B256,
    value: &NodAmountReadV1,
) -> Result<(), TransportError> {
    verify_response_attestation(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        &nod_read_preimage(expected, value.amount),
    )
}

fn auth_group_read(
    key: &[u8; 32],
    expected: B256,
    value: &NodGroupLoadReadV1,
) -> Result<(), TransportError> {
    verify_response_attestation(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        &nod_group_load_read_preimage(expected, value.amount),
    )
}

fn auth_group_load(
    key: &[u8; 32],
    expected: GroupLoadExpect,
    value: &NodGroupLoadOpAppliedV1,
) -> Result<(), TransportError> {
    verify_signed_domain(
        &auth_spec(
            key,
            expected.inputs,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        b"outbe/nod/group-load-result/v1",
        &value.record,
    )?;
    if value.record.chain_id == expected.chain_id
        && value.record.scope == expected.scope
        && value.record.version() == Some(expected.version)
    {
        return Ok(());
    }
    Err(TransportError::UnexpectedResponse)
}

fn auth_quote(
    key: &[u8; 32],
    expected: B256,
    value: &NodQuoteEncryptedV1,
) -> Result<(), TransportError> {
    verify_signed_domain(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        b"outbe/nod/quote-encrypt-result/v1",
        &value.encrypted_payment,
    )
}

fn auth_spec<'a>(
    key: &'a [u8; 32],
    expected: B256,
    actual: B256,
    tag: &'a [u8],
) -> ResponseAuthSpec<'a> {
    ResponseAuthSpec {
        attestation_pub: key,
        descriptor: NOD_ATTESTATION,
        expected_inputs_hash: expected,
        actual_inputs_hash: actual,
        tag,
    }
}

fn sign_bytes(context: &InProcessTeeContext, preimage: &[u8]) -> Vec<u8> {
    sign_attestation_tag(context, preimage).to_vec()
}

fn nod_state_key(context: &InProcessTeeContext) -> Result<StateKeyHandle, TransportError> {
    derive_resident_state_key(context, &NOD, 0)
}

fn require_resident_chain(
    context: &InProcessTeeContext,
    requested: u64,
) -> Result<(), TransportError> {
    let resident = U256::from_be_bytes(context.manifest().chain_id);
    let resident = u64::try_from(resident).map_err(|_| reject(NodHostError::ChainOverflow))?;
    if resident != requested {
        return Err(reject(NodHostError::ChainMismatch));
    }
    Ok(())
}

fn expected_version(request: &NodGroupLoadOpRequestV1) -> Result<u64, TransportError> {
    match &request.previous {
        None => Ok(1),
        Some(previous) => previous
            .version()
            .and_then(|version| version.checked_add(1))
            .ok_or_else(|| reject(NodHostError::InvalidGroupLoadVersion)),
    }
}

fn reject(error: impl std::error::Error + Send + Sync + 'static) -> TransportError {
    TransportError::enclave_error(error)
}

/// Signed tag 10, 19, 20, and 21 bodies from one resident context.
#[cfg(feature = "test-enclave")]
#[derive(Clone)]
pub struct SignedNodHost {
    /// Tag 10.
    pub amount: NodAmountReadV1,
    /// Tag 19.
    pub applied: NodGroupLoadOpAppliedV1,
    /// Tag 20.
    pub load: NodGroupLoadReadV1,
    /// Tag 21.
    pub quote: NodQuoteEncryptedV1,
    amount_inputs: B256,
    load_expect: GroupLoadExpect,
    load_inputs: B256,
    quote_inputs: B256,
}

#[cfg(feature = "test-enclave")]
impl SignedNodHost {
    /// Run the four host authenticators against this snapshot's attestation key.
    pub fn authenticate(&self, key: &[u8; 32]) -> Result<(), TransportError> {
        auth_amount_read(key, self.amount_inputs, &self.amount)?;
        auth_group_load(key, self.load_expect, &self.applied)?;
        auth_group_read(key, self.load_inputs, &self.load)?;
        auth_quote(key, self.quote_inputs, &self.quote)
    }
}

/// Sign the four recorded NOD responses on `context` and authenticate them there.
#[cfg(feature = "test-enclave")]
pub fn sign_nod_host_for_test(
    context: &InProcessTeeContext,
    nod: &EncryptedNodV1,
    request: &NodGroupLoadOpRequestV1,
    load: &EncryptedNodGroupLoadV1,
    quote: (&NodQuoteContextV1, U256),
) -> Result<SignedNodHost, TransportError> {
    let amount_inputs = nod_read_hash(nod).map_err(TransportError::codec)?;
    let load_expect = GroupLoadExpect {
        inputs: nod_group_load_inputs_hash(request).map_err(TransportError::codec)?,
        chain_id: request.chain_id,
        scope: request.scope,
        version: expected_version(request)?,
    };
    let (quote, payment_minor) = quote;
    let load_inputs = nod_group_load_read_hash(load).map_err(TransportError::codec)?;
    let quote_inputs =
        nod_quote_inputs_hash(nod, quote, payment_minor).map_err(TransportError::codec)?;
    Ok(SignedNodHost {
        amount: execute_amount_read(context, nod, amount_inputs)?,
        applied: execute_group_load(context, request, load_expect)?,
        load: execute_group_read(context, load, load_inputs)?,
        quote: execute_quote(context, nod, quote, payment_minor, quote_inputs)?,
        amount_inputs,
        load_expect,
        load_inputs,
        quote_inputs,
    })
}
