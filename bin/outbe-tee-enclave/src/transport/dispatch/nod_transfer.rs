//! Bounded temporary transfer buffers, never economic authority or key caches.
use crate::{keys::EnclaveKeys, transport::SharedTributeOfferKey};
use alloy_primitives::{keccak256, B256};
use outbe_tee::{
    nod_transport::{MAX_TRANSFER_BYTES, TRANSFER_CHUNK_BYTES},
    protocol::{EnclaveRequest, EnclaveResponse},
};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
struct Transfer {
    total: usize,
    input: Vec<u8>,
    output: Option<Vec<u8>>,
    touched: Instant,
}
static TRANSFERS: OnceLock<Mutex<BTreeMap<B256, Transfer>>> = OnceLock::new();
fn store() -> std::sync::MutexGuard<'static, BTreeMap<B256, Transfer>> {
    TRANSFERS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}
fn error(message: &str) -> EnclaveResponse {
    EnclaveResponse::Error {
        message: message.into(),
    }
}
fn check_chunk_bounds(total: usize, offset: usize, bytes: &[u8]) -> Result<(), EnclaveResponse> {
    if total == 0 || total > MAX_TRANSFER_BYTES || bytes.is_empty() {
        return Err(error("invalid NOD transfer bounds"));
    }
    if bytes.len() > TRANSFER_CHUNK_BYTES
        || offset
            .checked_add(bytes.len())
            .is_none_or(|end| end > total)
    {
        return Err(error("invalid NOD transfer chunk bounds"));
    }
    Ok(())
}
pub(super) fn append(id: B256, total: u32, offset: u32, bytes: &[u8]) -> EnclaveResponse {
    let total = total as usize;
    let offset = offset as usize;
    if let Err(error) = check_chunk_bounds(total, offset, bytes) {
        return error;
    }
    let mut transfers = store();
    transfers.retain(|_, entry| entry.touched.elapsed() < Duration::from_secs(120));
    if !transfers.contains_key(&id) {
        if offset != 0 || transfers.len() >= 2 {
            return error("NOD transfer unavailable");
        }
        transfers.insert(
            id,
            Transfer {
                total,
                input: Vec::new(),
                output: None,
                touched: Instant::now(),
            },
        );
    }
    let Some(entry) = transfers.get_mut(&id) else {
        return error("NOD transfer missing after allocation");
    };
    if entry.total != total || entry.output.is_some() {
        return error("NOD transfer state mismatch");
    }
    if offset == entry.input.len() {
        entry.input.extend_from_slice(bytes);
    } else if offset + bytes.len() > entry.input.len()
        || &entry.input[offset..offset + bytes.len()] != bytes
    {
        return error("NOD transfer chunk mismatch");
    }
    entry.touched = Instant::now();
    EnclaveResponse::NodTransferAckV2 {
        id,
        next_offset: (offset + bytes.len()) as u32,
    }
}
pub(super) fn execute(
    id: B256,
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
) -> EnclaveResponse {
    let mut transfers = store();
    let Some(entry) = transfers.get_mut(&id) else {
        return error("NOD transfer missing");
    };
    if let Some(output) = &entry.output {
        return EnclaveResponse::NodTransferReadyV2 {
            id,
            total: output.len() as u32,
            digest: keccak256(output),
        };
    }
    if entry.input.len() != entry.total || keccak256(&entry.input) != id {
        return error("NOD transfer request mismatch");
    }
    let response = match dispatch_request(&entry.input, keys, offer, chain) {
        Ok(response) => response,
        Err(error) => return error,
    };
    let output = match serde_json::to_vec(&response) {
        Ok(bytes) if bytes.len() <= MAX_TRANSFER_BYTES => bytes,
        _ => return error("NOD transfer response exceeds bound"),
    };
    entry.input.clear();
    entry.input.shrink_to_fit();
    entry.touched = Instant::now();
    let ready = EnclaveResponse::NodTransferReadyV2 {
        id,
        total: output.len() as u32,
        digest: keccak256(&output),
    };
    entry.output = Some(output);
    ready
}
fn dispatch_request(
    input: &[u8],
    keys: &EnclaveKeys,
    offer: &SharedTributeOfferKey,
    chain: B256,
) -> Result<EnclaveResponse, EnclaveResponse> {
    let request: EnclaveRequest = match serde_json::from_slice(input) {
        Ok(request) => request,
        Err(_) => return Err(error("NOD transfer request encoding")),
    };
    match request {
        EnclaveRequest::PrepareEncryptedNodsV2 { request } => {
            Ok(super::nod::prepare(keys, offer, chain, &request))
        }
        EnclaveRequest::OpenEncryptedNodsV2 { authority, carrier } => {
            Ok(super::nod::open(keys, offer, chain, &authority, &carrier))
        }
        _ => Err(error("NOD transfer command not permitted")),
    }
}
pub(super) fn read(id: B256, offset: u32) -> EnclaveResponse {
    let mut transfers = store();
    let Some(entry) = transfers.get_mut(&id) else {
        return error("NOD transfer missing");
    };
    let Some(output) = &entry.output else {
        return error("NOD transfer result unavailable");
    };
    let start = offset as usize;
    if start >= output.len() {
        return error("NOD transfer offset outside result");
    }
    entry.touched = Instant::now();
    EnclaveResponse::NodTransferOutputV2 {
        id,
        offset,
        bytes: output[start..(start + TRANSFER_CHUNK_BYTES).min(output.len())].to_vec(),
    }
}
pub(super) fn discard(id: B256) -> EnclaveResponse {
    store().remove(&id);
    EnclaveResponse::NodTransferDiscardedV2 { id }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_tee::nod_materialization::{
        NodMaterializationAuthorityV2, PrepareEncryptedNodsRequestV2,
    };
    use std::sync::Arc;

    #[test]
    fn multipart_bounds_replay_and_large_request_are_checked() {
        let invalid = B256::repeat_byte(0x65);
        assert!(matches!(
            append(invalid, (MAX_TRANSFER_BYTES + 1) as u32, 0, &[1]),
            EnclaveResponse::Error { .. }
        ));
        assert!(matches!(
            append(invalid, 100_000, 0, &vec![1; TRANSFER_CHUNK_BYTES + 1]),
            EnclaveResponse::Error { .. }
        ));
        let request = EnclaveRequest::PrepareEncryptedNodsV2 {
            request: Box::new(PrepareEncryptedNodsRequestV2 {
                authority: NodMaterializationAuthorityV2 {
                    chain_id: 1,
                    head: vec![255; 70_000],
                    subtree_height: 3,
                    sealed_tribute_root: B256::ZERO,
                },
                batch: vec![],
                sources: vec![],
            }),
        };
        let bytes = serde_json::to_vec(&request).unwrap();
        assert!(bytes.len() > 65_535);
        let id = keccak256(&bytes);
        for (index, chunk) in bytes.chunks(TRANSFER_CHUNK_BYTES).enumerate() {
            let offset = (index * TRANSFER_CHUNK_BYTES) as u32;
            assert!(matches!(
                append(id, bytes.len() as u32, offset, chunk),
                EnclaveResponse::NodTransferAckV2 { .. }
            ));
            assert!(matches!(
                append(id, bytes.len() as u32, offset, chunk),
                EnclaveResponse::NodTransferAckV2 { .. }
            ));
        }
        assert!(matches!(
            append(id, bytes.len() as u32, 0, &[0]),
            EnclaveResponse::Error { .. }
        ));
        let keys = EnclaveKeys::new([0x43; 32], None).unwrap();
        let offer: SharedTributeOfferKey = Arc::new(OnceLock::new());
        let (total, digest) = match execute(id, &keys, &offer, B256::ZERO) {
            EnclaveResponse::NodTransferReadyV2 { total, digest, .. } => (total, digest),
            response => panic!("expected transfer result, got {response:?}"),
        };
        assert!(
            matches!(execute(id,&keys,&offer,B256::ZERO),EnclaveResponse::NodTransferReadyV2 {total:t,digest:d,..} if t==total&&d==digest)
        );
        let output = match read(id, 0) {
            EnclaveResponse::NodTransferOutputV2 { bytes, .. } => bytes,
            response => panic!("expected output chunk, got {response:?}"),
        };
        assert_eq!(keccak256(&output), digest);
        assert!(matches!(
            serde_json::from_slice::<EnclaveResponse>(&output).unwrap(),
            EnclaveResponse::Error { .. }
        ));
        assert!(matches!(read(id, total), EnclaveResponse::Error { .. }));
        discard(id);
        assert!(matches!(read(id, 0), EnclaveResponse::Error { .. }));
    }
}
