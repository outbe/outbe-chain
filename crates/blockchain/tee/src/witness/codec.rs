use alloy_primitives::Bytes;
use thiserror::Error;

use super::tape::EnclaveWitnessTapeV1;

const VERSION: u8 = 1;
const PREFIX_LEN: usize = 5;
const MIN_PAYLOAD_LEN: usize = 10;
const MAX_RAW_LEN: usize = 262_144;
const MAX_EXTRA_DATA_LEN: usize = 65_536;
const OART_AND_TLV_OVERHEAD: usize = 10;
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

#[derive(Debug, Error)]
pub enum WitnessCodecError {
    #[error("witness tape is empty or has noncanonical transaction segments")]
    InvalidTape,
    #[error("witness payload exceeds the raw-size limit")]
    RawTooLarge,
    #[error("witness payload exceeds the outer OART/TLV size limit")]
    PayloadTooLarge,
    #[error("invalid witness payload framing")]
    InvalidFrame,
    #[error("unsupported witness payload version")]
    UnsupportedVersion,
    #[error("invalid witness payload raw length")]
    InvalidRawLength,
    #[error("witness compression failed: {0}")]
    Compression(#[from] std::io::Error),
    #[error("witness serialization failed: {0}")]
    Serialize(#[from] postcard::Error),
}

/// Conservative size for the five-byte witness prefix plus one zstd frame.
pub fn compressed_witness_bound(raw_len: usize) -> usize {
    PREFIX_LEN + zstd::zstd_safe::compress_bound(raw_len)
}

fn validate_tape(tape: &EnclaveWitnessTapeV1) -> Result<(), WitnessCodecError> {
    if tape.prefix.is_empty() && tape.txs.is_empty() {
        return Err(WitnessCodecError::InvalidTape);
    }
    let mut previous = None;
    for tx in &tape.txs {
        if tx.entries.is_empty() || previous.is_some_and(|prev| tx.tx_index <= prev) {
            return Err(WitnessCodecError::InvalidTape);
        }
        previous = Some(tx.tx_index);
    }
    Ok(())
}

pub fn encode_witness_payload_v1(tape: &EnclaveWitnessTapeV1) -> Result<Bytes, WitnessCodecError> {
    validate_tape(tape)?;
    let raw = postcard::to_allocvec(tape)?;
    if raw.is_empty() || raw.len() > MAX_RAW_LEN {
        return Err(WitnessCodecError::RawTooLarge);
    }
    let frame = zstd::bulk::compress(&raw, 3)?;
    let payload_len = PREFIX_LEN + frame.len();
    if payload_len > u16::MAX as usize || payload_len + OART_AND_TLV_OVERHEAD > MAX_EXTRA_DATA_LEN {
        return Err(WitnessCodecError::PayloadTooLarge);
    }
    let mut payload = Vec::with_capacity(payload_len);
    payload.push(VERSION);
    payload.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    payload.extend_from_slice(&frame);
    Ok(Bytes::from(payload))
}

fn parse_witness_frame(payload: &[u8]) -> Result<(usize, &[u8]), WitnessCodecError> {
    if payload.len() < MIN_PAYLOAD_LEN {
        return Err(WitnessCodecError::InvalidFrame);
    }
    if payload.len() + OART_AND_TLV_OVERHEAD > MAX_EXTRA_DATA_LEN
        || payload.len() > u16::MAX as usize
    {
        return Err(WitnessCodecError::PayloadTooLarge);
    }
    if payload[0] != VERSION {
        return Err(WitnessCodecError::UnsupportedVersion);
    }
    let raw_len = u32::from_le_bytes(
        payload[1..PREFIX_LEN]
            .try_into()
            .expect("fixed-size prefix"),
    ) as usize;
    if raw_len == 0 || raw_len > MAX_RAW_LEN {
        return Err(WitnessCodecError::InvalidRawLength);
    }
    Ok((raw_len, &payload[PREFIX_LEN..]))
}

fn validate_zstd_single_frame(frame: &[u8], raw_len: usize) -> Result<(), WitnessCodecError> {
    let valid_header = frame.len() >= 5 && frame[..4] == ZSTD_MAGIC && frame[4] & 0x03 == 0;
    let exact_frame = zstd::zstd_safe::find_frame_compressed_size(frame).ok() == Some(frame.len());
    let exact_content = zstd::zstd_safe::get_frame_content_size(frame)
        .ok()
        .flatten()
        == Some(raw_len as u64);
    if !(valid_header && exact_frame && exact_content) {
        return Err(WitnessCodecError::InvalidFrame);
    }
    Ok(())
}

pub fn decode_witness_payload_v1(
    payload: &[u8],
) -> Result<EnclaveWitnessTapeV1, WitnessCodecError> {
    let (raw_len, frame) = parse_witness_frame(payload)?;
    validate_zstd_single_frame(frame, raw_len)?;
    let raw = zstd::bulk::decompress(frame, raw_len).map_err(WitnessCodecError::Compression)?;
    if raw.len() != raw_len {
        return Err(WitnessCodecError::InvalidRawLength);
    }
    let (tape, remainder) = postcard::take_from_bytes::<EnclaveWitnessTapeV1>(&raw)?;
    if !remainder.is_empty() {
        return Err(WitnessCodecError::InvalidFrame);
    }
    validate_tape(&tape)?;
    Ok(tape)
}
