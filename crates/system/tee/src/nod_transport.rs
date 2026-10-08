//! Bounded multipart transfer for protected materialization across Noise frames.
use crate::{
    protocol::{EnclaveRequest, EnclaveResponse},
    EnclaveSession, TransportError,
};
use alloy_primitives::{keccak256, B256};
pub const MAX_TRANSFER_BYTES: usize = 16 * 1024 * 1024;
pub const TRANSFER_CHUNK_BYTES: usize = 8 * 1024;

pub(crate) fn request(
    request: EnclaveRequest,
) -> Result<([u8; 32], EnclaveResponse), TransportError> {
    let bytes = serde_json::to_vec(&request).map_err(|e| TransportError::Codec(e.to_string()))?;
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(TransportError::NodMaterializationCapacityExceeded(
            "protected NOD request exceeds transfer bound".into(),
        ));
    }
    let id = keccak256(&bytes);
    crate::try_with_enclave(|client| {
        let key = client.attestation_pub();
        let result = upload(client, id, &bytes).and_then(|()| download(client, id));
        let _ = client.request(&EnclaveRequest::DiscardNodTransferV2 { id });
        result.map(|response| (key, response))
    })
    .ok_or_else(|| TransportError::EnclaveError("TEE sidecar unavailable".into()))?
}

fn upload(client: &mut EnclaveSession, id: B256, bytes: &[u8]) -> Result<(), TransportError> {
    for (index, chunk) in bytes.chunks(TRANSFER_CHUNK_BYTES).enumerate() {
        let offset = index * TRANSFER_CHUNK_BYTES;
        match client.request(&EnclaveRequest::NodTransferChunkV2 {
            id,
            total: bytes.len() as u32,
            offset: offset as u32,
            bytes: chunk.to_vec(),
        })? {
            EnclaveResponse::NodTransferAckV2 {
                id: response_id,
                next_offset,
            } if response_id == id && next_offset as usize == offset + chunk.len() => {}
            other => return Err(response_error(other)),
        }
    }
    Ok(())
}

fn download(client: &mut EnclaveSession, id: B256) -> Result<EnclaveResponse, TransportError> {
    let (total, digest) = match client.request(&EnclaveRequest::ExecuteNodTransferV2 { id })? {
        EnclaveResponse::NodTransferReadyV2 {
            id: response_id,
            total,
            digest,
        } if response_id == id && total as usize <= MAX_TRANSFER_BYTES => (total as usize, digest),
        other => return Err(response_error(other)),
    };
    let mut output = Vec::with_capacity(total);
    while output.len() < total {
        output.extend_from_slice(&read_chunk(client, id, output.len(), total)?);
    }
    if keccak256(&output) != digest {
        return Err(TransportError::Codec(
            "protected NOD response digest mismatch".into(),
        ));
    }
    serde_json::from_slice(&output).map_err(|e| TransportError::Codec(e.to_string()))
}

fn read_chunk(
    client: &mut EnclaveSession,
    id: B256,
    offset: usize,
    total: usize,
) -> Result<Vec<u8>, TransportError> {
    match client.request(&EnclaveRequest::ReadNodTransferV2 {
        id,
        offset: offset as u32,
    })? {
        EnclaveResponse::NodTransferOutputV2 {
            id: response_id,
            offset: response_offset,
            bytes,
        } if response_id == id && response_offset as usize == offset => {
            if bytes.is_empty()
                || bytes.len() > TRANSFER_CHUNK_BYTES
                || bytes.len() > total - offset
            {
                return Err(TransportError::UnexpectedResponse);
            }
            Ok(bytes)
        }
        other => Err(response_error(other)),
    }
}

fn response_error(response: EnclaveResponse) -> TransportError {
    match response {
        EnclaveResponse::Error { message } => TransportError::EnclaveError(message),
        _ => TransportError::UnexpectedResponse,
    }
}
