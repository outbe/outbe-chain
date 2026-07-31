//! Bounded, per-Noise-session upload for enclave-resident DCAP verification.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{MAX_ATTESTATION_EVIDENCE_BYTES, MAX_TEE_POLICY_BYTES};
#[cfg(feature = "native-dcap")]
use outbe_tee::dcap_protocol::DcapRejectCodeV1;
use outbe_tee::{
    dcap_protocol::{
        dcap_verification_attestation_preimage, dcap_verification_request_hash,
        DcapVerificationOutcomeV1, MAX_DCAP_VERIFICATION_CHUNK_BYTES,
    },
    protocol::{EnclaveRequest, EnclaveResponse},
};

use crate::keys::EnclaveKeys;

#[derive(Debug)]
struct UploadV1 {
    request_hash: B256,
    evidence_len: usize,
    policy_len: usize,
    block_timestamp: u64,
    bytes: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CompleteDcapVerificationV1 {
    request_hash: B256,
    evidence: Vec<u8>,
    policy: Vec<u8>,
    block_timestamp: u64,
}

pub(crate) enum DcapVerificationProgressV1 {
    Started {
        request_hash: B256,
    },
    ChunkAccepted {
        request_hash: B256,
        next_offset: u32,
    },
    Complete(CompleteDcapVerificationV1),
}

/// One bounded upload at a time on one authenticated Noise connection.
#[derive(Default)]
pub(crate) struct DcapVerificationSessionV1 {
    upload: Option<UploadV1>,
}

impl DcapVerificationSessionV1 {
    pub(crate) fn handle(
        &mut self,
        request: EnclaveRequest,
    ) -> Result<DcapVerificationProgressV1, &'static str> {
        let result = match request {
            EnclaveRequest::BeginDcapVerificationV1 {
                request_hash,
                evidence_len,
                policy_len,
                block_timestamp,
            } => self.begin(request_hash, evidence_len, policy_len, block_timestamp),
            EnclaveRequest::DcapVerificationChunkV1 {
                request_hash,
                offset,
                bytes,
            } => self.push(request_hash, offset, bytes),
            EnclaveRequest::FinishDcapVerificationV1 { request_hash } => self.finish(request_hash),
            _ => Err("request is not part of DCAP verification upload"),
        };
        if result.is_err() {
            self.upload = None;
        }
        result
    }

    fn begin(
        &mut self,
        request_hash: B256,
        evidence_len: u32,
        policy_len: u32,
        block_timestamp: u64,
    ) -> Result<DcapVerificationProgressV1, &'static str> {
        if self.upload.is_some() {
            return Err("a DCAP verification upload is already active");
        }
        let evidence_len = usize::try_from(evidence_len)
            .map_err(|_| "DCAP evidence length exceeds this target")?;
        let policy_len =
            usize::try_from(policy_len).map_err(|_| "DCAP policy length exceeds this target")?;
        if evidence_len > MAX_ATTESTATION_EVIDENCE_BYTES {
            return Err("DCAP evidence length exceeds the protocol cap");
        }
        if policy_len > MAX_TEE_POLICY_BYTES {
            return Err("DCAP policy length exceeds the protocol cap");
        }
        let total = evidence_len
            .checked_add(policy_len)
            .ok_or("DCAP verification upload length overflow")?;
        self.upload = Some(UploadV1 {
            request_hash,
            evidence_len,
            policy_len,
            block_timestamp,
            bytes: Vec::with_capacity(total),
        });
        Ok(DcapVerificationProgressV1::Started { request_hash })
    }

    fn push(
        &mut self,
        request_hash: B256,
        offset: u32,
        bytes: Vec<u8>,
    ) -> Result<DcapVerificationProgressV1, &'static str> {
        let upload = self
            .upload
            .as_mut()
            .ok_or("no DCAP verification upload is active")?;
        if upload.request_hash != request_hash {
            return Err("DCAP verification chunk request hash mismatch");
        }
        if bytes.is_empty() || bytes.len() > MAX_DCAP_VERIFICATION_CHUNK_BYTES {
            return Err("DCAP verification chunk length is invalid");
        }
        let offset = usize::try_from(offset).map_err(|_| "DCAP verification offset overflow")?;
        if offset != upload.bytes.len() {
            return Err("DCAP verification chunk offset is not sequential");
        }
        let total = upload
            .evidence_len
            .checked_add(upload.policy_len)
            .ok_or("DCAP verification upload length overflow")?;
        let next = offset
            .checked_add(bytes.len())
            .ok_or("DCAP verification chunk length overflow")?;
        if next > total {
            return Err("DCAP verification chunk exceeds the declared upload length");
        }
        upload.bytes.extend_from_slice(&bytes);
        let next_offset = u32::try_from(next).map_err(|_| "DCAP verification offset overflow")?;
        Ok(DcapVerificationProgressV1::ChunkAccepted {
            request_hash,
            next_offset,
        })
    }

    fn finish(&mut self, request_hash: B256) -> Result<DcapVerificationProgressV1, &'static str> {
        let upload = self
            .upload
            .take()
            .ok_or("no DCAP verification upload is active")?;
        if upload.request_hash != request_hash {
            return Err("DCAP verification finish request hash mismatch");
        }
        let total = upload
            .evidence_len
            .checked_add(upload.policy_len)
            .ok_or("DCAP verification upload length overflow")?;
        if upload.bytes.len() != total {
            return Err("DCAP verification upload is truncated");
        }
        let policy = upload.bytes[upload.evidence_len..].to_vec();
        let evidence = upload.bytes[..upload.evidence_len].to_vec();
        let computed = dcap_verification_request_hash(&evidence, &policy, upload.block_timestamp)
            .map_err(|_| "DCAP verification request commitment is invalid")?;
        if computed != request_hash {
            return Err("DCAP verification request commitment mismatch");
        }
        Ok(DcapVerificationProgressV1::Complete(
            CompleteDcapVerificationV1 {
                request_hash,
                evidence,
                policy,
                block_timestamp: upload.block_timestamp,
            },
        ))
    }
}

pub(crate) fn complete_verification_response(
    request: CompleteDcapVerificationV1,
    keys: &EnclaveKeys,
) -> EnclaveResponse {
    #[cfg(feature = "native-dcap")]
    let outcome = verify_complete_request(&request);
    #[cfg(not(feature = "native-dcap"))]
    let outcome: Result<DcapVerificationOutcomeV1, &'static str> =
        Err("enclave was built without the pinned native DCAP verifier");

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(message) => {
            return EnclaveResponse::Error {
                message: message.to_string(),
            }
        }
    };
    let outcome = match outcome.encode_canonical() {
        Ok(outcome) => outcome,
        Err(_) => {
            return EnclaveResponse::Error {
                message: "enclave produced a non-canonical DCAP outcome".to_string(),
            }
        }
    };
    let preimage = match dcap_verification_attestation_preimage(request.request_hash, &outcome) {
        Ok(preimage) => preimage,
        Err(_) => {
            return EnclaveResponse::Error {
                message: "enclave produced an oversized DCAP outcome".to_string(),
            }
        }
    };
    EnclaveResponse::DcapVerificationFinishedV1 {
        request_hash: request.request_hash,
        outcome,
        attestation_tag: keys.sign_attestation(&preimage).to_vec(),
    }
}

#[cfg(feature = "native-dcap")]
fn verify_complete_request(
    request: &CompleteDcapVerificationV1,
) -> Result<DcapVerificationOutcomeV1, &'static str> {
    use outbe_primitives::tee_attestation_v1::{AttestationEvidenceV1, TeePolicyV1};

    let evidence = match AttestationEvidenceV1::decode_canonical(&request.evidence) {
        Ok(AttestationEvidenceV1::Dcap(evidence)) => evidence,
        Ok(AttestationEvidenceV1::GramineDirectDev(_)) | Err(_) => {
            return Ok(DcapVerificationOutcomeV1::Rejected(
                DcapRejectCodeV1::EvidenceNonCanonical,
            ))
        }
    };
    let policy = match TeePolicyV1::decode_canonical(&request.policy) {
        Ok(policy) => policy,
        Err(_) => {
            return Ok(DcapVerificationOutcomeV1::Rejected(
                DcapRejectCodeV1::PolicyNonCanonical,
            ))
        }
    };
    Ok(
        match outbe_tee::dcap_v1::verify_dcap_evidence(&evidence, &policy, request.block_timestamp)
        {
            Ok(verdict) => DcapVerificationOutcomeV1::Accepted(verdict),
            Err(code) => DcapVerificationOutcomeV1::Rejected(code),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_tee::dcap_protocol::dcap_verification_request_hash;

    #[test]
    fn bounded_upload_reassembles_the_exact_committed_inputs() {
        let evidence = vec![0x11; 70_000];
        let policy = vec![0x22; 1_000];
        let timestamp = 1_700_000_000;
        let hash = dcap_verification_request_hash(&evidence, &policy, timestamp).unwrap();
        let mut session = DcapVerificationSessionV1::default();
        assert!(matches!(
            session
                .handle(EnclaveRequest::BeginDcapVerificationV1 {
                    request_hash: hash,
                    evidence_len: evidence.len() as u32,
                    policy_len: policy.len() as u32,
                    block_timestamp: timestamp,
                })
                .unwrap(),
            DcapVerificationProgressV1::Started { request_hash } if request_hash == hash
        ));
        let mut combined = evidence.clone();
        combined.extend_from_slice(&policy);
        let first = combined[..MAX_DCAP_VERIFICATION_CHUNK_BYTES].to_vec();
        assert!(matches!(
            session
                .handle(EnclaveRequest::DcapVerificationChunkV1 {
                    request_hash: hash,
                    offset: 0,
                    bytes: first,
                })
                .unwrap(),
            DcapVerificationProgressV1::ChunkAccepted { next_offset, .. }
                if next_offset == MAX_DCAP_VERIFICATION_CHUNK_BYTES as u32
        ));
        session
            .handle(EnclaveRequest::DcapVerificationChunkV1 {
                request_hash: hash,
                offset: MAX_DCAP_VERIFICATION_CHUNK_BYTES as u32,
                bytes: combined[MAX_DCAP_VERIFICATION_CHUNK_BYTES..].to_vec(),
            })
            .unwrap();
        let DcapVerificationProgressV1::Complete(complete) = session
            .handle(EnclaveRequest::FinishDcapVerificationV1 { request_hash: hash })
            .unwrap()
        else {
            panic!("finish did not return the complete verifier request");
        };
        assert_eq!(complete.request_hash, hash);
        assert_eq!(complete.evidence, evidence);
        assert_eq!(complete.policy, policy);
        assert_eq!(complete.block_timestamp, timestamp);
    }

    #[test]
    fn upload_caps_reject_before_accepting_any_chunk() {
        let mut session = DcapVerificationSessionV1::default();
        let hash = B256::repeat_byte(0x41);
        assert!(session
            .handle(EnclaveRequest::BeginDcapVerificationV1 {
                request_hash: hash,
                evidence_len: (MAX_ATTESTATION_EVIDENCE_BYTES + 1) as u32,
                policy_len: 1,
                block_timestamp: 1,
            })
            .is_err());
        assert!(session
            .handle(EnclaveRequest::BeginDcapVerificationV1 {
                request_hash: hash,
                evidence_len: 1,
                policy_len: (MAX_TEE_POLICY_BYTES + 1) as u32,
                block_timestamp: 1,
            })
            .is_err());
        session
            .handle(EnclaveRequest::BeginDcapVerificationV1 {
                request_hash: hash,
                evidence_len: 1,
                policy_len: 1,
                block_timestamp: 1,
            })
            .unwrap();
        assert!(session
            .handle(EnclaveRequest::DcapVerificationChunkV1 {
                request_hash: hash,
                offset: 0,
                bytes: vec![0; MAX_DCAP_VERIFICATION_CHUNK_BYTES + 1],
            })
            .is_err());
        assert!(session
            .handle(EnclaveRequest::DcapVerificationChunkV1 {
                request_hash: hash,
                offset: 0,
                bytes: vec![0],
            })
            .is_err());
    }

    #[test]
    fn upload_order_hash_or_truncation_fault_clears_the_session() {
        let evidence = [0x51, 0x52];
        let policy = [0x61, 0x62];
        let timestamp = 7;
        let hash = dcap_verification_request_hash(&evidence, &policy, timestamp).unwrap();
        let begin = || EnclaveRequest::BeginDcapVerificationV1 {
            request_hash: hash,
            evidence_len: evidence.len() as u32,
            policy_len: policy.len() as u32,
            block_timestamp: timestamp,
        };
        let mut session = DcapVerificationSessionV1::default();

        session.handle(begin()).unwrap();
        assert!(session.handle(begin()).is_err());
        assert!(session
            .handle(EnclaveRequest::FinishDcapVerificationV1 { request_hash: hash })
            .is_err());

        session.handle(begin()).unwrap();
        assert!(session
            .handle(EnclaveRequest::DcapVerificationChunkV1 {
                request_hash: hash,
                offset: 1,
                bytes: vec![0x51],
            })
            .is_err());
        assert!(session
            .handle(EnclaveRequest::FinishDcapVerificationV1 { request_hash: hash })
            .is_err());

        session.handle(begin()).unwrap();
        session
            .handle(EnclaveRequest::DcapVerificationChunkV1 {
                request_hash: hash,
                offset: 0,
                bytes: evidence.to_vec(),
            })
            .unwrap();
        assert!(session
            .handle(EnclaveRequest::FinishDcapVerificationV1 { request_hash: hash })
            .is_err());

        session.handle(begin()).unwrap();
        let mut combined = evidence.to_vec();
        combined.extend_from_slice(&policy);
        session
            .handle(EnclaveRequest::DcapVerificationChunkV1 {
                request_hash: hash,
                offset: 0,
                bytes: combined,
            })
            .unwrap();
        assert!(session
            .handle(EnclaveRequest::FinishDcapVerificationV1 {
                request_hash: B256::repeat_byte(0x99),
            })
            .is_err());
    }
}
