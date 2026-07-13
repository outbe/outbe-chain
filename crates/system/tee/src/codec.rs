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
//! Messages are serialized with `postcard` — a compact binary serde format. The
//! offer ciphertext rides as a length-prefixed raw byte string (1x) rather than a
//! JSON number array (~4x under `serde_json`), and alloy `U256`/`Address`/`B256`
//! serialize as raw bytes (non-human-readable serde) instead of hex strings, so
//! many more offers fit under the 64 KiB Noise frame per `ProcessTributeOfferBatch`.
//! Both binaries are built from this crate, so encoder and decoder always agree;
//! the chain is from-genesis, so there is no legacy wire to stay compatible with.

use std::io::{Read, Write};

use crate::errors::TransportError;
use crate::protocol::{EnclaveRequest, EnclaveResponse};

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
    postcard::from_bytes(bytes).map_err(|e| TransportError::Codec(e.to_string()))
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
    use crate::protocol::{EnclaveRequest, EncryptedTributeOffer};
    use alloy_primitives::{Address, U256};

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
                reference_currency: 840,
                exclude_from_intex_issuance: false,
                tribute_price_minor: U256::from(1u64),
            }],
        };
        let bytes = encode_request(&req).unwrap();
        assert_eq!(decode_request(&bytes).unwrap(), req);
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
}
