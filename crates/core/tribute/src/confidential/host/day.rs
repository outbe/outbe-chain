//! Tags 13, 14, 16, and 17: day amount, Lysis fractions, and their reads.

use std::collections::BTreeMap;

use alloy_primitives::{B256, U256};
use outbe_core_common::confidential_tee::{
    verify_response_attestation, verify_signed_domain, EnclaveClientError,
};
use outbe_core_common::tribute_day_encryption::{
    EncryptedTributeDayAmountV2, EncryptedTributeDayFractionsV1,
};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::TransportError;

use super::super::day_protocol::{
    self, LysisFractionsRequestV1, LysisFractionsResultV1, TributeDayOpRequestV2,
};
use super::super::tribute_day::{
    self, apply_day_operation as apply_pure, execute_lysis_fractions as lysis_pure,
};
use super::call::{self, auth_spec};
use super::schema::{
    LysisFractionsExecutedV1, TributeDayAmountReadV2, TributeDayFractionsReadV1,
    TributeDayOpAppliedV2,
};

/// Apply a day transition and record tag 13.
pub fn apply_day_operation(
    request: TributeDayOpRequestV2,
) -> Result<EncryptedTributeDayAmountV2, TransportError> {
    let expected = day_protocol::day_operation_inputs_hash(&request).map_err(call::reject)?;
    let chain_id = request.chain_id;
    let day = request.worldwide_day;
    let frozen = matches!(
        request.operation,
        day_protocol::TributeDayOperationV2::Freeze
    );
    let version = expected_version(request.previous.as_ref())?;
    let answer = call::recorded(
        "apply_tribute_day_op_v2",
        move |context| execute_apply(context, &request, expected),
        move |key, value| {
            authenticate_apply(key, expected, (chain_id, day, frozen, version), value)
        },
    )?;
    Ok(answer.record)
}

/// Read a day nominal and record tag 14.
pub fn read_day_amount(record: EncryptedTributeDayAmountV2) -> Result<U256, TransportError> {
    let expected = day_protocol::day_read_inputs_hash(&record).map_err(call::reject)?;
    let answer = call::recorded(
        "read_tribute_day_amount_v2",
        move |context| execute_read(context, &record, expected),
        move |key, value| authenticate_read(key, expected, value),
    )?;
    Ok(answer.amount)
}

/// Execute Lysis fractions and record tag 16.
pub fn execute_lysis_fractions(
    request: LysisFractionsRequestV1,
) -> Result<LysisFractionsResultV1, TransportError> {
    let expected = day_protocol::lysis_fractions_inputs_hash(&request).map_err(call::reject)?;
    let identity = (
        request.chain_id,
        request.worldwide_day,
        request.emission_day,
        request.completed_at,
    );
    let answer = call::recorded(
        "execute_lysis_fractions_v1",
        move |context| execute_lysis(context, &request, expected),
        move |key, value| authenticate_lysis(key, expected, identity, value),
    )?;
    Ok(answer.result)
}

/// Read day fractions and record tag 17.
pub fn read_day_fractions(
    record: EncryptedTributeDayFractionsV1,
) -> Result<BTreeMap<u16, u64>, TransportError> {
    let expected = day_protocol::day_fractions_read_inputs_hash(&record).map_err(call::reject)?;
    let answer = call::recorded(
        "read_tribute_day_fractions_v1",
        move |context| execute_fractions(context, &record, expected),
        move |key, value| authenticate_fractions(key, expected, value),
    )?;
    Ok(answer.fractions)
}

fn expected_version(previous: Option<&EncryptedTributeDayAmountV2>) -> Result<u64, TransportError> {
    let Some(previous) = previous else {
        return Ok(1);
    };
    let version = previous.version().ok_or_else(|| {
        call::reject(EnclaveClientError::InvalidTributeDayPredecessorVersion {
            expected: 1,
            actual: 0,
        })
    })?;
    version.checked_add(1).ok_or_else(|| {
        call::reject(EnclaveClientError::InvalidTributeDayPredecessorVersion {
            expected: 1,
            actual: u8::try_from(version).unwrap_or(u8::MAX),
        })
    })
}

fn execute_apply(
    context: &InProcessTeeContext,
    request: &TributeDayOpRequestV2,
    expected: B256,
) -> Result<TributeDayOpAppliedV2, TransportError> {
    let record = call::with_offer(context, |offer| {
        call::require_chain(context, request.chain_id)?;
        let tribute = call::tribute_state_key(context)?;
        apply_pure(offer, &tribute, request).map_err(call::reject)
    })?;
    let preimage = day_protocol::day_operation_attestation_preimage(expected, &record)
        .map_err(call::reject)?;
    let answer = TributeDayOpAppliedV2 {
        record,
        inputs_canonical_hash: expected,
        attestation_tag: call::sign(context, &preimage),
    };
    let version = expected_version(request.previous.as_ref())?;
    let frozen = matches!(
        request.operation,
        day_protocol::TributeDayOperationV2::Freeze
    );
    authenticate_apply(
        &context.attestation_public(),
        expected,
        (request.chain_id, request.worldwide_day, frozen, version),
        &answer,
    )?;
    Ok(answer)
}

fn execute_read(
    context: &InProcessTeeContext,
    record: &EncryptedTributeDayAmountV2,
    expected: B256,
) -> Result<TributeDayAmountReadV2, TransportError> {
    let amount = call::with_offer(context, |offer| {
        call::require_chain(context, record.chain_id)?;
        tribute_day::read_day_amount(offer, record).map_err(call::reject)
    })?;
    let preimage = day_protocol::day_read_attestation_preimage(expected, amount);
    let answer = TributeDayAmountReadV2 {
        amount,
        inputs_canonical_hash: expected,
        attestation_tag: call::sign(context, &preimage),
    };
    authenticate_read(&context.attestation_public(), expected, &answer)?;
    Ok(answer)
}

fn execute_lysis(
    context: &InProcessTeeContext,
    request: &LysisFractionsRequestV1,
    expected: B256,
) -> Result<LysisFractionsExecutedV1, TransportError> {
    let result = call::with_offer(context, |offer| {
        call::require_chain(context, request.chain_id)?;
        let tribute = call::tribute_state_key(context)?;
        let nod = call::nod_state_key(context)?;
        lysis_pure(offer, &tribute, &nod, request).map_err(call::reject)
    })?;
    let preimage = day_protocol::lysis_fractions_attestation_preimage(expected, &result)
        .map_err(call::reject)?;
    let answer = LysisFractionsExecutedV1 {
        result,
        inputs_canonical_hash: expected,
        attestation_tag: call::sign(context, &preimage),
    };
    authenticate_lysis(
        &context.attestation_public(),
        expected,
        (
            request.chain_id,
            request.worldwide_day,
            request.emission_day,
            request.completed_at,
        ),
        &answer,
    )?;
    Ok(answer)
}

fn execute_fractions(
    context: &InProcessTeeContext,
    record: &EncryptedTributeDayFractionsV1,
    expected: B256,
) -> Result<TributeDayFractionsReadV1, TransportError> {
    let fractions = call::with_offer(context, |offer| {
        call::require_chain(context, record.chain_id)?;
        tribute_day::read_day_fractions(offer, record).map_err(call::reject)
    })?;
    let preimage = day_protocol::day_fractions_read_attestation_preimage(expected, &fractions)
        .map_err(call::reject)?;
    let answer = TributeDayFractionsReadV1 {
        fractions,
        inputs_canonical_hash: expected,
        attestation_tag: call::sign(context, &preimage),
    };
    authenticate_fractions(&context.attestation_public(), expected, &answer)?;
    Ok(answer)
}

fn authenticate_apply(
    key: &[u8; 32],
    expected: B256,
    (chain_id, day, frozen, version): (u64, outbe_system_common::time::WorldwideDay, bool, u64),
    value: &TributeDayOpAppliedV2,
) -> Result<(), TransportError> {
    verify_signed_domain(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        b"outbe/tribute/day-result/v2",
        &value.record,
    )?;
    let record = &value.record;
    call::same_identity(
        record.chain_id == chain_id
            && record.worldwide_day == day
            && record.frozen == frozen
            && record.version() == Some(version),
    )
}

fn authenticate_read(
    key: &[u8; 32],
    expected: B256,
    value: &TributeDayAmountReadV2,
) -> Result<(), TransportError> {
    let preimage = day_protocol::day_read_attestation_preimage(expected, value.amount);
    verify_response_attestation(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        &preimage,
    )
}

fn authenticate_lysis(
    key: &[u8; 32],
    expected: B256,
    identity: (
        u64,
        outbe_system_common::time::WorldwideDay,
        outbe_system_common::time::EmissionDate,
        u64,
    ),
    value: &LysisFractionsExecutedV1,
) -> Result<(), TransportError> {
    verify_signed_domain(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        b"outbe/tribute/lysis-fractions-result/v1",
        &value.result,
    )?;
    call::same_identity(lysis_identity_matches(
        &value.result.day_fractions,
        identity,
    ))
}

fn lysis_identity_matches(
    fractions: &outbe_core_common::tribute_day_encryption::EncryptedTributeDayFractionsV1,
    identity: (
        u64,
        outbe_system_common::time::WorldwideDay,
        outbe_system_common::time::EmissionDate,
        u64,
    ),
) -> bool {
    let chain = fractions.chain_id == identity.0;
    let day = fractions.worldwide_day == identity.1;
    let emission = fractions.emission_day == identity.2;
    let completed = fractions.completed_at == identity.3;
    let version = fractions.version() == Some(1);
    [chain, day, emission, completed, version]
        .into_iter()
        .all(|matches| matches)
}

fn authenticate_fractions(
    key: &[u8; 32],
    expected: B256,
    value: &TributeDayFractionsReadV1,
) -> Result<(), TransportError> {
    let preimage =
        day_protocol::day_fractions_read_attestation_preimage(expected, &value.fractions)
            .map_err(call::reject)?;
    verify_response_attestation(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        &preimage,
    )
}
