//! Tag 12 private Tribute amount read.

use alloy_primitives::B256;
use outbe_core_common::confidential_tee::verify_response_attestation;
use outbe_core_common::tribute_encryption::{EncryptedTributeV2, TributeAmountsV2};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::TransportError;

use super::super::tribute_encryption::decrypt_tribute;
use super::super::tribute_v2::{self, tribute_read_attestation_preimage};
use super::call::{self, auth_spec};
use super::schema::TributeAmountsReadV2;

/// Read decrypted amounts and record tag 12.
pub fn read_tribute_amounts(
    tributes: Vec<EncryptedTributeV2>,
) -> Result<Vec<TributeAmountsV2>, TransportError> {
    let expected = tribute_v2::tribute_read_inputs_hash(&tributes).map_err(call::reject)?;
    let auth_tributes = tributes.clone();
    let answer = call::recorded(
        "read_tribute_amounts_v2",
        move |context| execute(context, &tributes, expected),
        move |key, value| authenticate(key, expected, &auth_tributes, value),
    )?;
    Ok(answer.amounts)
}

fn execute(
    context: &InProcessTeeContext,
    tributes: &[EncryptedTributeV2],
    expected: B256,
) -> Result<TributeAmountsReadV2, TransportError> {
    let key = call::tribute_state_key(context)?;
    let mut amounts = Vec::with_capacity(tributes.len());
    for tribute in tributes {
        call::require_chain(context, tribute.context.chain_id)?;
        amounts.push(decrypt_tribute(&key, tribute).map_err(call::reject)?);
    }
    let preimage = tribute_read_attestation_preimage(expected, &amounts).map_err(call::reject)?;
    let answer = TributeAmountsReadV2 {
        amounts,
        inputs_canonical_hash: expected,
        attestation_tag: call::sign(context, &preimage),
    };
    authenticate(&context.attestation_public(), expected, tributes, &answer)?;
    Ok(answer)
}

fn authenticate(
    key: &[u8; 32],
    expected: B256,
    tributes: &[EncryptedTributeV2],
    value: &TributeAmountsReadV2,
) -> Result<(), TransportError> {
    let preimage =
        tribute_read_attestation_preimage(expected, &value.amounts).map_err(call::reject)?;
    verify_response_attestation(
        &auth_spec(
            key,
            expected,
            value.inputs_canonical_hash,
            &value.attestation_tag,
        ),
        &preimage,
    )?;
    call::same_identity(value.amounts.len() == tributes.len())
}
