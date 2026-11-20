//! Tags 7 and 18: NOD created from a Tribute, and NOD issued from day fractions.

use alloy_primitives::{B256, U256};
use outbe_core_common::confidential_tee::{hash, verify_signed_domain};
use outbe_core_common::nod_encryption::{EncryptedNodV1, NodTermsV1};
use outbe_core_common::tribute_encryption::EncryptedTributeV2;
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::TransportError;

use super::super::nod_bridge::{self, IssueNodFromTributeOutcomeV1, IssueNodFromTributeRequestV1};
use super::call::{self, auth_spec};
use super::schema::{NodEncryptedForTributeV1, NodIssuedFromTributeV1};

/// Encrypt a NOD bound to a Tribute and record tag 7.
pub fn encrypt_nod_for_tribute(
    tribute: EncryptedTributeV2,
    terms: NodTermsV1,
    amount: U256,
) -> Result<EncryptedNodV1, TransportError> {
    let expected = hash(
        b"outbe/nod/tribute-create-inputs/v1",
        &(&tribute, &terms, amount),
    )
    .map_err(call::reject)?;
    let chain_id = terms.chain_id;
    let auth_terms = terms.clone();
    let answer = call::recorded(
        "encrypt_nod_for_tribute_v1",
        move |context| execute_encrypt(context, &tribute, &terms, amount, expected),
        move |key, value| authenticate_encrypt(key, expected, chain_id, &auth_terms, value),
    )?;
    Ok(answer.nod)
}

/// Issue a NOD from a Tribute and record tag 18.
pub fn issue_nod_from_tribute(
    request: IssueNodFromTributeRequestV1,
) -> Result<IssueNodFromTributeOutcomeV1, TransportError> {
    let expected =
        hash(b"outbe/nod/issue-from-tribute-inputs/v1", &request).map_err(call::reject)?;
    let chain_id = request.terms.chain_id;
    let auth_terms = request.terms.clone();
    let answer = call::recorded(
        "issue_nod_from_tribute_v1",
        move |context| execute_issue(context, &request, expected),
        move |key, value| authenticate_issue(key, expected, chain_id, &auth_terms, value),
    )?;
    Ok(answer.outcome)
}

fn execute_encrypt(
    context: &InProcessTeeContext,
    tribute: &EncryptedTributeV2,
    terms: &NodTermsV1,
    amount: U256,
    expected: B256,
) -> Result<NodEncryptedForTributeV1, TransportError> {
    call::require_chain(context, terms.chain_id)?;
    let nod_key = call::nod_state_key(context)?;
    let tribute_key = call::tribute_state_key(context)?;
    let nod =
        nod_bridge::encrypt_nod_for_tribute(&tribute_key, &nod_key, tribute, terms.clone(), amount)
            .map_err(call::reject)?;
    let preimage = outbe_core_common::confidential_tee::attestation(
        b"outbe/nod/tribute-create-result/v1",
        expected,
        &nod,
    )
    .map_err(call::reject)?;
    let answer = NodEncryptedForTributeV1 {
        nod,
        inputs_canonical_hash: expected,
        attestation_tag: call::sign(context, &preimage),
    };
    authenticate_encrypt(
        &context.attestation_public(),
        expected,
        terms.chain_id,
        terms,
        &answer,
    )?;
    Ok(answer)
}

fn execute_issue(
    context: &InProcessTeeContext,
    request: &IssueNodFromTributeRequestV1,
    expected: B256,
) -> Result<NodIssuedFromTributeV1, TransportError> {
    call::require_chain(context, request.terms.chain_id)?;
    let nod_key = call::nod_state_key(context)?;
    let tribute_key = call::tribute_state_key(context)?;
    let outcome = call::with_offer(context, |offer| {
        nod_bridge::issue_nod_from_tribute(offer, &tribute_key, &nod_key, request)
            .map_err(call::reject)
    })?;
    let preimage = outbe_core_common::confidential_tee::attestation(
        b"outbe/nod/issue-from-tribute-result/v1",
        expected,
        &outcome,
    )
    .map_err(call::reject)?;
    let answer = NodIssuedFromTributeV1 {
        outcome,
        inputs_canonical_hash: expected,
        attestation_tag: call::sign(context, &preimage),
    };
    authenticate_issue(
        &context.attestation_public(),
        expected,
        request.terms.chain_id,
        &request.terms,
        &answer,
    )?;
    Ok(answer)
}

fn authenticate_encrypt(
    key: &[u8; 32],
    expected: B256,
    chain_id: u64,
    terms: &NodTermsV1,
    value: &NodEncryptedForTributeV1,
) -> Result<(), TransportError> {
    verify_signed_domain(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        b"outbe/nod/tribute-create-result/v1",
        &value.nod,
    )?;
    call::same_identity(value.nod.terms == *terms && value.nod.terms.chain_id == chain_id)
}

fn authenticate_issue(
    key: &[u8; 32],
    expected: B256,
    chain_id: u64,
    terms: &NodTermsV1,
    value: &NodIssuedFromTributeV1,
) -> Result<(), TransportError> {
    verify_signed_domain(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        b"outbe/nod/issue-from-tribute-result/v1",
        &value.outcome,
    )?;
    let issued_matches = match &value.outcome {
        IssueNodFromTributeOutcomeV1::Issued(nod) => {
            nod.terms == *terms && nod.terms.chain_id == chain_id
        }
        IssueNodFromTributeOutcomeV1::LysisArithmetic(_)
        | IssueNodFromTributeOutcomeV1::ZeroSettlementCost => true,
    };
    call::same_identity(issued_matches)
}
