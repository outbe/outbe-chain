//! Borrowed canonical ABI validation before policy reads or verifier execution.
use alloy_sol_types::SolCall;
use outbe_primitives::{
    error::{PrecompileError, Result},
    tee_attestation_v1::{
        ValidatorNodeBindingV1, MAX_ATTESTATION_EVIDENCE_BYTES, MAX_EVIDENCE_CALL_FRAMING_BYTES,
    },
    tee_registry_abi_v1::ITeeRegistryV1,
};

#[derive(Clone, Copy)]
pub(super) struct RegisterPreflight<'a> {
    pub(super) evidence: &'a [u8],
    pub(super) node_signature: &'a [u8],
    pub(super) enclave_signature: &'a [u8],
    pub(super) validator_node_binding: Option<&'a [u8]>,
    pub(super) validator_signature: Option<&'a [u8]>,
    pub(super) node_binding_signature: Option<&'a [u8]>,
}

struct MutatorAbiHead<'a> {
    args: &'a [u8],
    head: &'a [u8],
    is_register: bool,
    evidence_offset: usize,
    node_signature_offset: usize,
    enclave_signature_offset: usize,
}

/// Validates the complete canonical ABI layout without allocating dynamic
/// arguments, before policy allocation or native QVL execution.
pub(super) fn preflight_evidence_mutator_call(data: &[u8]) -> Result<RegisterPreflight<'_>> {
    let head = mutator_head(data)?;
    let (mut preflight, next_offset) = common_signatures(&head)?;
    let final_offset = if head.is_register {
        validator_binding(&head, &mut preflight, next_offset)?
    } else {
        next_offset
    };
    validate_call_end(data, head.args, final_offset, preflight.evidence.len())?;
    Ok(preflight)
}

fn mutator_head(data: &[u8]) -> Result<MutatorAbiHead<'_>> {
    const MUTATOR_HEAD_WORDS: usize = 3;
    const REGISTER_HEAD_WORDS: usize = 6;
    let is_register =
        data.get(..4) == Some(ITeeRegistryV1::registerEnclaveCall::SELECTOR.as_slice());
    let head_words = if is_register {
        REGISTER_HEAD_WORDS
    } else {
        MUTATOR_HEAD_WORDS
    };
    let head_len = head_words * 32;

    let args = data
        .get(4..)
        .ok_or_else(|| invalid_register_abi("missing function selector"))?;
    let head = args
        .get(..head_len)
        .ok_or_else(|| invalid_register_abi("truncated argument head"))?;
    let evidence_offset = abi_usize(&head[0..32])?;
    let node_signature_offset = abi_usize(&head[32..64])?;
    let enclave_signature_offset = abi_usize(&head[64..96])?;
    if evidence_offset != head_len {
        return Err(invalid_register_abi("non-canonical evidence offset"));
    }

    Ok(MutatorAbiHead {
        args,
        head,
        is_register,
        evidence_offset,
        node_signature_offset,
        enclave_signature_offset,
    })
}

fn common_signatures<'a>(head: &MutatorAbiHead<'a>) -> Result<(RegisterPreflight<'a>, usize)> {
    let MutatorAbiHead {
        args,
        evidence_offset,
        node_signature_offset,
        enclave_signature_offset,
        ..
    } = *head;
    let (evidence, next_offset) = dynamic_bytes(args, evidence_offset)?;
    if evidence.len() > MAX_ATTESTATION_EVIDENCE_BYTES {
        return Err(PrecompileError::Revert(format!(
            "attestation evidence exceeds {} bytes",
            MAX_ATTESTATION_EVIDENCE_BYTES
        )));
    }
    if node_signature_offset != next_offset {
        return Err(invalid_register_abi("non-canonical node signature offset"));
    }
    let (node_signature, next_offset) = dynamic_bytes(args, node_signature_offset)?;
    if node_signature.len() != 65 {
        return Err(PrecompileError::Revert(
            "node proof-of-possession signature must be 65 bytes".into(),
        ));
    }
    if enclave_signature_offset != next_offset {
        return Err(invalid_register_abi(
            "non-canonical enclave signature offset",
        ));
    }
    let (enclave_signature, next_offset) = dynamic_bytes(args, enclave_signature_offset)?;
    if enclave_signature.len() != 64 {
        return Err(PrecompileError::Revert(
            "enclave proof-of-possession signature must be 64 bytes".into(),
        ));
    }
    Ok((
        RegisterPreflight {
            evidence,
            node_signature,
            enclave_signature,
            validator_node_binding: None,
            validator_signature: None,
            node_binding_signature: None,
        },
        next_offset,
    ))
}

fn validator_binding<'a>(
    head: &MutatorAbiHead<'a>,
    preflight: &mut RegisterPreflight<'a>,
    next_offset: usize,
) -> Result<usize> {
    let args = head.args;
    let head = head.head;
    let binding_offset = abi_usize(&head[96..128])?;
    let validator_signature_offset = abi_usize(&head[128..160])?;
    let node_binding_signature_offset = abi_usize(&head[160..192])?;
    let (binding, next_offset) = validator_node_binding(args, binding_offset, next_offset)?;
    if validator_signature_offset != next_offset {
        return Err(invalid_register_abi(
            "non-canonical validator signature offset",
        ));
    }
    let (validator_signature, next_offset) = dynamic_bytes(args, validator_signature_offset)?;
    if validator_signature.len() != 65 {
        return Err(PrecompileError::Revert(
            "validator NodeHost binding signature must be 65 bytes".into(),
        ));
    }
    if node_binding_signature_offset != next_offset {
        return Err(invalid_register_abi(
            "non-canonical NodeHost binding signature offset",
        ));
    }
    let (node_binding_signature, final_offset) =
        dynamic_bytes(args, node_binding_signature_offset)?;
    if node_binding_signature.len() != 65 {
        return Err(PrecompileError::Revert(
            "NodeHost binding signature must be 65 bytes".into(),
        ));
    }
    preflight.validator_node_binding = Some(binding);
    preflight.validator_signature = Some(validator_signature);
    preflight.node_binding_signature = Some(node_binding_signature);
    Ok(final_offset)
}

fn validator_node_binding(
    args: &[u8],
    binding_offset: usize,
    next_offset: usize,
) -> Result<(&[u8], usize)> {
    if binding_offset != next_offset {
        return Err(invalid_register_abi(
            "non-canonical validator NodeHost binding offset",
        ));
    }
    let (binding, next_offset) = dynamic_bytes(args, binding_offset)?;
    if binding.len() != ValidatorNodeBindingV1::CANONICAL_LEN {
        return Err(PrecompileError::Revert(format!(
            "validator NodeHost binding must be {} bytes",
            ValidatorNodeBindingV1::CANONICAL_LEN
        )));
    }
    Ok((binding, next_offset))
}

fn validate_call_end(
    data: &[u8],
    args: &[u8],
    final_offset: usize,
    evidence_len: usize,
) -> Result<()> {
    if final_offset != args.len() {
        return Err(invalid_register_abi("trailing ABI bytes"));
    }
    let framing_len = data
        .len()
        .checked_sub(evidence_len)
        .ok_or_else(|| invalid_register_abi("evidence length exceeds calldata"))?;
    if framing_len > MAX_EVIDENCE_CALL_FRAMING_BYTES {
        return Err(PrecompileError::Revert(format!(
            "evidence call framing exceeds {} bytes",
            MAX_EVIDENCE_CALL_FRAMING_BYTES
        )));
    }

    Ok(())
}

impl RegisterPreflight<'_> {
    pub(super) fn signatures(&self) -> Result<([u8; 65], [u8; 64])> {
        let node_signature = self
            .node_signature
            .try_into()
            .map_err(|_| PrecompileError::Fatal("preflight node signature mismatch".into()))?;
        let enclave_signature = self
            .enclave_signature
            .try_into()
            .map_err(|_| PrecompileError::Fatal("preflight enclave signature mismatch".into()))?;
        Ok((node_signature, enclave_signature))
    }
}

fn dynamic_bytes(args: &[u8], offset: usize) -> Result<(&[u8], usize)> {
    let length_word_end = offset
        .checked_add(32)
        .ok_or_else(|| invalid_register_abi("dynamic offset overflow"))?;
    let length = abi_usize(
        args.get(offset..length_word_end)
            .ok_or_else(|| invalid_register_abi("truncated dynamic length"))?,
    )?;
    let value_end = length_word_end
        .checked_add(length)
        .ok_or_else(|| invalid_register_abi("dynamic length overflow"))?;
    let value = args
        .get(length_word_end..value_end)
        .ok_or_else(|| invalid_register_abi("truncated dynamic value"))?;
    let padded_length = length
        .checked_add(31)
        .map(|length| length / 32 * 32)
        .ok_or_else(|| invalid_register_abi("dynamic padding overflow"))?;
    let padded_end = length_word_end
        .checked_add(padded_length)
        .ok_or_else(|| invalid_register_abi("dynamic padding overflow"))?;
    let padding = args
        .get(value_end..padded_end)
        .ok_or_else(|| invalid_register_abi("truncated dynamic padding"))?;
    if padding.iter().any(|byte| *byte != 0) {
        return Err(invalid_register_abi("non-zero dynamic padding"));
    }
    Ok((value, padded_end))
}

fn abi_usize(word: &[u8]) -> Result<usize> {
    let width = core::mem::size_of::<usize>();
    if word.len() != 32 || word[..32 - width].iter().any(|byte| *byte != 0) {
        return Err(invalid_register_abi("ABI integer exceeds host usize"));
    }
    let mut value = [0_u8; core::mem::size_of::<usize>()];
    value.copy_from_slice(&word[32 - width..]);
    Ok(usize::from_be_bytes(value))
}

fn invalid_register_abi(reason: &'static str) -> PrecompileError {
    PrecompileError::Revert(format!("invalid canonical V1 registration ABI: {reason}"))
}
