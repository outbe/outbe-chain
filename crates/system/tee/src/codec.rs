//! Length-prefixed framing + message (de)serialization for the node <-> enclave
//! channel.
//!
//! Wire frame: a 4-byte big-endian length prefix followed by that many bytes.
//! Frame bodies are either:
//!   - a serialized [`EnclaveRequest`] / [`EnclaveResponse`] (pre-handshake
//!     `GetQuote`), or
//!   - a Noise-encrypted ciphertext wrapping such a serialization (post
//!     handshake), or
//!   - a raw Noise handshake message.
//!
//! Messages are serialized with `postcard` - a compact binary serde format. The
//! offer ciphertext rides as a length-prefixed raw byte string (1x) rather than a
//! JSON number array (~4x under `serde_json`), and alloy `U256`/`Address`/`B256`
//! serialize as raw bytes (non-human-readable serde) instead of hex strings, so
//! many more offers fit under the 64 KiB Noise frame per `ProcessTributeOfferBatch`.
//! Authenticated calls carry a versioned context envelope. The decoder also
//! accepts legacy requests and marks their context as unavailable; producers of
//! context envelopes require a matching enclave receiver.

use std::io::{Read, Write};

use crate::call_context::{EnclaveCallContextV1, EnclaveContextKindV1};
use crate::errors::TransportError;
use crate::protocol::{EnclaveRequest, EnclaveResponse};

// Outside the legacy enum's postcard discriminants. Existing requests retain
// their exact wire indices; the common envelope can evolve independently.
const CALL_V1: &[u8] = b"\xff\xff\x00OUTBE-CTX\x01";

#[derive(serde::Serialize, serde::Deserialize)]
pub struct EnclaveCallV1 {
    pub ctx: EnclaveCallContextV1,
    pub request: EnclaveRequest,
}

/// Encode every post-handshake call with one frozen context.
pub fn encode_call(
    ctx: EnclaveCallContextV1,
    request: &EnclaveRequest,
) -> Result<Vec<u8>, TransportError> {
    let mut bytes = CALL_V1.to_vec();
    // A tuple has the same postcard layout as the owned envelope, without
    // cloning potentially large request buffers.
    bytes.extend(
        postcard::to_allocvec(&(ctx, request)).map_err(|e| TransportError::Codec(e.to_string()))?,
    );
    if bytes.len() > MAX_FRAME_LEN - 16 {
        return Err(TransportError::FrameTooLarge(bytes.len() + 16));
    }
    Ok(bytes)
}

pub fn decode_call(bytes: &[u8]) -> Result<EnclaveCallV1, TransportError> {
    if let Some(body) = bytes.strip_prefix(CALL_V1) {
        let (call, rest) = postcard::take_from_bytes::<EnclaveCallV1>(body)
            .map_err(|e| TransportError::Codec(e.to_string()))?;
        if !rest.is_empty() {
            return Err(TransportError::Codec("trailing enclave call bytes".into()));
        }
        Ok(call)
    } else {
        let (request, rest) = postcard::take_from_bytes::<EnclaveRequest>(bytes)
            .map_err(|e| TransportError::Codec(e.to_string()))?;
        if !rest.is_empty() {
            return Err(TransportError::Codec(
                "trailing legacy request bytes".into(),
            ));
        }
        Ok(EnclaveCallV1 {
            ctx: EnclaveCallContextV1 {
                kind: EnclaveContextKindV1::Legacy,
                ..Default::default()
            },
            request,
        })
    }
}

/// Hard cap on a single frame body. Bounds memory and matches the Noise 64 KiB
/// message ceiling closely enough for the PoC (larger batches need chunking).
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// Write a single length-prefixed frame.
pub fn write_frame<W: Write>(w: &mut W, body: &[u8]) -> Result<(), TransportError> {
    if body.len() > MAX_FRAME_LEN {
        return Err(TransportError::FrameTooLarge(body.len()));
    }
    let len = (body.len() as u32).to_be_bytes();
    w.write_all(&len)?;
    w.write_all(body)?;
    w.flush()?;
    Ok(())
}

/// Read a single length-prefixed frame.
pub fn read_frame<R: Read>(r: &mut R) -> Result<Vec<u8>, TransportError> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_LEN {
        return Err(TransportError::FrameTooLarge(len));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(body)
}

/// Serialize a request to bytes (postcard).
pub fn encode_request(req: &EnclaveRequest) -> Result<Vec<u8>, TransportError> {
    postcard::to_allocvec(req).map_err(|e| TransportError::Codec(e.to_string()))
}

/// Deserialize a request from bytes (postcard).
pub fn decode_request(bytes: &[u8]) -> Result<EnclaveRequest, TransportError> {
    Ok(decode_call(bytes)?.request)
}

/// Serialize a response to bytes (postcard).
pub fn encode_response(resp: &EnclaveResponse) -> Result<Vec<u8>, TransportError> {
    postcard::to_allocvec(resp).map_err(|e| TransportError::Codec(e.to_string()))
}

/// Deserialize a response from bytes (postcard).
pub fn decode_response(bytes: &[u8]) -> Result<EnclaveResponse, TransportError> {
    postcard::from_bytes(bytes).map_err(|e| TransportError::Codec(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{EnclaveRequest, EncryptedTributeOffer, WorldwideDay};
    use alloy_primitives::{Address, U256};

    #[test]
    fn common_context_envelope_preserves_request_and_rejects_trailing_bytes() {
        let ctx = EnclaveCallContextV1 {
            kind: EnclaveContextKindV1::Execution,
            chain_id: 424242,
            genesis_hash: alloy_primitives::B256::repeat_byte(0x42),
            block_number: 300,
            block_timestamp: 1700000000,
            protocol_version: 2,
        };
        for request in [
            EnclaveRequest::Health,
            EnclaveRequest::DkgStartDealer {
                ceremony_id: alloy_primitives::B256::ZERO,
            },
            EnclaveRequest::ProcessTributeOfferBatch { offers: Vec::new() },
        ] {
            let bytes = encode_call(ctx, &request).unwrap();
            let decoded = decode_call(&bytes).unwrap();
            assert_eq!(decoded.ctx, ctx);
            assert_eq!(decoded.request, request);
            let legacy = encode_request(&request).unwrap();
            assert_eq!(
                decode_call(&legacy).unwrap().ctx.kind,
                EnclaveContextKindV1::Legacy
            );
            assert_eq!(decode_request(&legacy).unwrap(), request);
            let mut trailing = bytes;
            trailing.push(0);
            assert!(decode_call(&trailing).is_err());
        }
    }

    #[test]
    fn frame_roundtrip() {
        let body = vec![1u8, 2, 3, 4, 5];
        let mut buf = Vec::new();
        write_frame(&mut buf, &body).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        assert_eq!(read_frame(&mut cursor).unwrap(), body);
    }

    #[test]
    fn request_codec_roundtrip() {
        let req = EnclaveRequest::ProcessTributeOfferBatch {
            offers: vec![EncryptedTributeOffer {
                owner: Address::repeat_byte(0xAB),
                cipher_text: vec![9, 9, 9],
                nonce: vec![1; 12],
                ephemeral_pubkey: U256::from(12345u64),
                worldwide_day: WorldwideDay::new(20250115),
                tribute_currency: 840,
                reference_currency: 840,
                exclude_from_intex_issuance: false,
                issuance_wwd_vwap_minor: U256::from(11u64),
                reference_wwd_vwap_minor: U256::from(12u64),
                reference_scurve_minor: U256::from(13u64),
                zk_context: None,
            }],
        };
        let bytes = encode_request(&req).unwrap();
        assert_eq!(decode_request(&bytes).unwrap(), req);
        assert!(decode_request(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn maximum_dcap_chunk_round_trips_below_the_frame_cap() {
        let req = EnclaveRequest::DcapVerificationChunkV1 {
            request_hash: alloy_primitives::B256::repeat_byte(0x51),
            offset: 0,
            bytes: vec![0x52; crate::dcap_protocol::MAX_DCAP_VERIFICATION_CHUNK_BYTES],
        };
        let plaintext = encode_request(&req).unwrap();
        assert!(plaintext.len() + 64 <= MAX_FRAME_LEN);
        assert_eq!(decode_request(&plaintext).unwrap(), req);
    }

    #[test]
    fn maximum_onboarding_proof_chunk_round_trips_below_the_frame_cap() {
        let req = EnclaveRequest::DcapOnboardingArtifactChunkV1 {
            request_hash: alloy_primitives::B256::repeat_byte(0x61),
            kind: crate::finalized_admission::FinalizedAdmissionRecordKindV1::Admission,
            offset: 0,
            bytes: vec![0x62; crate::finalized_admission::MAX_ONBOARDING_INGEST_CHUNK_BYTES],
        };
        let plaintext = encode_request(&req).unwrap();
        assert!(plaintext.len() + 64 <= MAX_FRAME_LEN);
        assert_eq!(decode_request(&plaintext).unwrap(), req);
    }

    #[test]
    fn frame_rejects_oversize() {
        let big = vec![0u8; MAX_FRAME_LEN + 1];
        let mut buf = Vec::new();
        assert!(matches!(
            write_frame(&mut buf, &big),
            Err(TransportError::FrameTooLarge(_))
        ));
    }

    /// Postcard encodes an enum as a varint of its variant declaration index, so
    /// the on-wire index of every existing variant is part of the protocol. New
    /// variants may be appended ONLY at the tail of `EnclaveRequest` /
    /// `EnclaveResponse`; inserting, reordering or removing a variant - or
    /// changing the fields of an existing wire struct - breaks a node and an
    /// enclave built from different revisions. These fixtures pin the indices of
    /// representative existing variants; if this test fails, the wire layout
    /// changed and the change must be reverted, not the fixture updated.
    #[test]
    fn request_wire_indices_are_pinned() {
        // Variant 0, unit-adjacent shape: GetQuote { nonce: [u8; 32] }.
        let bytes = encode_request(&EnclaveRequest::GetQuote {
            nonce: [0x11u8; 32],
        })
        .unwrap();
        assert_eq!(bytes[0], 0, "GetQuote must stay wire variant 0");
        assert_eq!(&bytes[1..33], &[0x11u8; 32]);

        // Field-free variant: GetPublicKeys is wire variant 6.
        let bytes = encode_request(&EnclaveRequest::GetPublicKeys).unwrap();
        assert_eq!(bytes, vec![6], "GetPublicKeys must stay wire variant 6");

        // Consensus hot path: ProcessTributeOfferBatch is wire variant 22.
        let bytes =
            encode_request(&EnclaveRequest::ProcessTributeOfferBatch { offers: Vec::new() })
                .unwrap();
        assert_eq!(
            bytes,
            vec![22, 0],
            "ProcessTributeOfferBatch must stay wire variant 22"
        );
    }

    #[test]
    fn health_request_and_response_round_trip() {
        let req = EnclaveRequest::Health;
        let bytes = encode_request(&req).unwrap();
        assert_eq!(decode_request(&bytes).unwrap(), req);

        let resp = crate::protocol::EnclaveResponse::HealthStatus {
            status: Box::new(crate::protocol::EnclaveHealthStatusV1 {
                uptime_s: 7,
                offer_key_ready: true,
                heap_current_bytes: 1024,
                heap_peak_bytes: 4096,
                requests_total: 10,
                requests_errored: 1,
                requests_denied: 2,
                class_initialized: 3,
                class_founding_keyless: 0,
                class_keyless_onboarding: 0,
                class_ready: 4,
                class_dev_source_seal: 0,
                class_dev_recipient_ingest: 0,
            }),
        };
        let bytes = encode_response(&resp).unwrap();
        assert_eq!(decode_response(&bytes).unwrap(), resp);
    }

    #[test]
    fn request_labels_are_unique_and_stable() {
        let labels = [
            EnclaveRequest::GetPublicKeys.label(),
            EnclaveRequest::ProcessTributeOfferBatch { offers: Vec::new() }.label(),
            EnclaveRequest::Health.label(),
        ];
        assert_eq!(labels[0], "get_public_keys");
        assert_eq!(labels[1], "process_tribute_offer_batch");
        assert_eq!(labels[2], "health");
    }

    #[test]
    fn idempotency_allowlist_is_pinned() {
        assert!(EnclaveRequest::GetPublicKeys.is_idempotent());
        assert!(EnclaveRequest::ProcessTributeOfferBatch { offers: Vec::new() }.is_idempotent());
        assert!(EnclaveRequest::Health.is_idempotent());
        assert!(!EnclaveRequest::OpenSession.is_idempotent());
        assert!(!EnclaveRequest::FinishDcapVerificationV1 {
            request_hash: alloy_primitives::B256::ZERO,
        }
        .is_idempotent());
        assert!(!EnclaveRequest::DkgStartDealer {
            ceremony_id: alloy_primitives::B256::ZERO,
        }
        .is_idempotent());
    }
}
