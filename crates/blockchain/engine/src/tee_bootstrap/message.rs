use super::*;

// Keep the complete participant evidence boxed while carrying small signature messages.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Ost3WireMessage {
    Submission(Box<TeeBootstrapParticipantSubmissionV2>),
    Signature {
        signing_hash: B256,
        validator: Address,
        signature: [u8; 65],
    },
}

impl Ost3WireMessage {
    pub(super) fn encode(&self) -> eyre::Result<Vec<u8>> {
        match self {
            Self::Submission(submission) => {
                let evidence = submission
                    .evidence
                    .encode_canonical()
                    .map_err(|error| eyre::eyre!("cannot encode OST3 evidence: {error}"))?;
                let evidence_len = u32::try_from(evidence.len())
                    .map_err(|_| eyre::eyre!("OST3 evidence length exceeds u32"))?;
                let mut out = Vec::with_capacity(OST3_SUBMISSION_FIXED_BYTES + evidence.len());
                out.push(OST3_SUBMISSION);
                out.extend_from_slice(&evidence_len.to_be_bytes());
                out.extend_from_slice(&evidence);
                out.extend_from_slice(
                    &submission
                        .validator_binding
                        .encode_canonical()
                        .map_err(|error| eyre::eyre!("cannot encode OST3 binding: {error}"))?,
                );
                out.extend_from_slice(&submission.validator_signature);
                out.extend_from_slice(&submission.node_binding_signature);
                out.extend_from_slice(&submission.node_signature);
                out.extend_from_slice(&submission.enclave_signature);
                Ok(out)
            }
            Self::Signature {
                signing_hash,
                validator,
                signature,
            } => {
                let mut out = Vec::with_capacity(OST3_SIGNATURE_BYTES);
                out.push(OST3_SIGNATURE);
                out.extend_from_slice(signing_hash.as_slice());
                out.extend_from_slice(validator.as_slice());
                out.extend_from_slice(signature);
                Ok(out)
            }
        }
    }

    pub(super) fn decode(input: &[u8]) -> Option<Self> {
        match input.first().copied()? {
            OST3_SUBMISSION => Self::decode_submission(input),
            OST3_SIGNATURE if input.len() == OST3_SIGNATURE_BYTES => {
                let signing_hash = B256::from_slice(input.get(1..33)?);
                let validator = Address::from_slice(input.get(33..53)?);
                let signature = input.get(53..118)?.try_into().ok()?;
                Some(Self::Signature {
                    signing_hash,
                    validator,
                    signature,
                })
            }
            _ => None,
        }
    }

    fn decode_submission(input: &[u8]) -> Option<Self> {
        if input.len() < OST3_SUBMISSION_FIXED_BYTES {
            return None;
        }
        let evidence_len =
            usize::try_from(u32::from_be_bytes(input.get(1..5)?.try_into().ok()?)).ok()?;
        if evidence_len > MAX_ATTESTATION_EVIDENCE_BYTES
            || input.len() != OST3_SUBMISSION_FIXED_BYTES.checked_add(evidence_len)?
        {
            return None;
        }
        let evidence_end = 5usize.checked_add(evidence_len)?;
        let evidence = AttestationEvidenceV1::decode_canonical(input.get(5..evidence_end)?).ok()?;
        let binding_end = evidence_end.checked_add(ValidatorNodeBindingV1::CANONICAL_LEN)?;
        let validator_binding =
            ValidatorNodeBindingV1::decode_canonical(input.get(evidence_end..binding_end)?).ok()?;
        let mut offset = binding_end;
        let validator_signature = take_signature(input, &mut offset)?;
        let node_binding_signature = take_signature(input, &mut offset)?;
        let node_signature = take_signature(input, &mut offset)?;
        let enclave_signature = input.get(offset..)?.try_into().ok()?;
        Some(Self::Submission(Box::new(
            TeeBootstrapParticipantSubmissionV2 {
                evidence,
                validator_binding,
                validator_signature,
                node_binding_signature,
                node_signature,
                enclave_signature,
            },
        )))
    }
}

fn take_signature<const N: usize>(input: &[u8], offset: &mut usize) -> Option<[u8; N]> {
    let end = offset.checked_add(N)?;
    let signature = input.get(*offset..end)?.try_into().ok()?;
    *offset = end;
    Some(signature)
}
