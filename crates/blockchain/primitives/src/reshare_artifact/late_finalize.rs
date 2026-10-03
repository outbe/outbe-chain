//! Canonical per-finalized-block credit batches.
use super::{
    ensure_count_fits_u16, LateFinalizeCreditsArtifact, PerBlockCredit, PrecompileError, Result,
    B256, LATE_FINALIZE_MAX_BATCHES, LATE_FINALIZE_MAX_BITMAP_LEN, LATE_FINALIZE_SIG_LEN,
    PER_BLOCK_CREDIT_FIXED_LEN,
};

pub(super) fn encode_payload(credits: &LateFinalizeCreditsArtifact) -> Result<Vec<u8>> {
    ensure_count_fits_u16("late finalize batches", credits.batches.len())?;
    if credits.batches.len() > LATE_FINALIZE_MAX_BATCHES {
        return Err(PrecompileError::Fatal(format!(
            "too many late finalize batches: {} > {LATE_FINALIZE_MAX_BATCHES}",
            credits.batches.len()
        )));
    }
    let mut payload = Vec::new();
    payload.extend_from_slice(&(credits.batches.len() as u16).to_be_bytes());
    let mut prev: Option<(u64, B256)> = None;
    for credit in &credits.batches {
        // Canonical order: strictly ascending (fb_number, fb_hash), one
        // record per target - deterministic bytes across all nodes.
        let key = (credit.fb_number, credit.fb_hash);
        if let Some(prev_key) = prev {
            if key <= prev_key {
                return Err(PrecompileError::Fatal(
                    "late finalize batches not in strictly ascending canonical order".into(),
                ));
            }
        }
        prev = Some(key);
        if credit.signer_bitmap.len() > LATE_FINALIZE_MAX_BITMAP_LEN {
            return Err(PrecompileError::Fatal(format!(
                "late finalize bitmap too long: {} > {LATE_FINALIZE_MAX_BITMAP_LEN}",
                credit.signer_bitmap.len()
            )));
        }
        payload.extend_from_slice(&credit.fb_number.to_be_bytes());
        payload.extend_from_slice(credit.fb_hash.as_slice());
        payload.extend_from_slice(&credit.epoch.to_be_bytes());
        payload.extend_from_slice(&credit.view.to_be_bytes());
        payload.extend_from_slice(&credit.parent_view.to_be_bytes());
        payload.extend_from_slice(credit.committee_set_hash.as_slice());
        payload.extend_from_slice(&(credit.signer_bitmap.len() as u16).to_be_bytes());
        payload.extend_from_slice(&credit.signer_bitmap);
        payload.extend_from_slice(&credit.aggregate_signature);
    }
    Ok(payload)
}

pub(super) fn decode_payload(payload: &[u8]) -> Result<LateFinalizeCreditsArtifact> {
    if payload.len() < 2 {
        return Err(PrecompileError::Fatal(
            "late finalize credits payload too short".into(),
        ));
    }
    let batch_count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    if batch_count > LATE_FINALIZE_MAX_BATCHES {
        return Err(PrecompileError::Fatal(format!(
            "too many late finalize batches: {batch_count} > {LATE_FINALIZE_MAX_BATCHES}"
        )));
    }
    let mut offset = 2usize;
    let mut batches = Vec::with_capacity(batch_count);
    let mut prev: Option<(u64, B256)> = None;

    let read_u64 = |buf: &[u8]| -> u64 {
        let mut b = [0u8; 8];
        b.copy_from_slice(buf);
        u64::from_be_bytes(b)
    };

    for _ in 0..batch_count {
        // Fixed prefix + the 2-byte bitmap length must be present before reading.
        if offset + PER_BLOCK_CREDIT_FIXED_LEN + 2 > payload.len() {
            return Err(PrecompileError::Fatal(
                "truncated late finalize credit prefix".into(),
            ));
        }
        let fb_number = read_u64(&payload[offset..offset + 8]);
        offset += 8;
        let fb_hash = B256::from_slice(&payload[offset..offset + 32]);
        offset += 32;
        let epoch = read_u64(&payload[offset..offset + 8]);
        offset += 8;
        let view = read_u64(&payload[offset..offset + 8]);
        offset += 8;
        let parent_view = read_u64(&payload[offset..offset + 8]);
        offset += 8;
        let committee_set_hash = B256::from_slice(&payload[offset..offset + 32]);
        offset += 32;
        let bitmap_len = u16::from_be_bytes([payload[offset], payload[offset + 1]]) as usize;
        offset += 2;
        if bitmap_len > LATE_FINALIZE_MAX_BITMAP_LEN {
            return Err(PrecompileError::Fatal(format!(
                "late finalize bitmap too long: {bitmap_len} > {LATE_FINALIZE_MAX_BITMAP_LEN}"
            )));
        }
        let body_end = offset
            .checked_add(bitmap_len)
            .and_then(|o| o.checked_add(LATE_FINALIZE_SIG_LEN))
            .ok_or_else(|| PrecompileError::Fatal("late finalize credit overflow".into()))?;
        if body_end > payload.len() {
            return Err(PrecompileError::Fatal(
                "truncated late finalize credit body".into(),
            ));
        }
        let signer_bitmap = payload[offset..offset + bitmap_len].to_vec();
        offset += bitmap_len;
        let mut aggregate_signature = [0u8; LATE_FINALIZE_SIG_LEN];
        aggregate_signature.copy_from_slice(&payload[offset..offset + LATE_FINALIZE_SIG_LEN]);
        offset += LATE_FINALIZE_SIG_LEN;

        // Canonical order: strictly ascending (fb_number, fb_hash); reject
        // out-of-order or duplicate targets for byte-deterministic decoding.
        let key = (fb_number, fb_hash);
        if let Some(prev_key) = prev {
            if key <= prev_key {
                return Err(PrecompileError::Fatal(
                    "late finalize batches not in strictly ascending canonical order".into(),
                ));
            }
        }
        prev = Some(key);

        batches.push(PerBlockCredit {
            fb_number,
            fb_hash,
            epoch,
            view,
            parent_view,
            committee_set_hash,
            signer_bitmap,
            aggregate_signature,
        });
    }

    if offset != payload.len() {
        return Err(PrecompileError::Fatal(
            "trailing bytes in late finalize credits payload".into(),
        ));
    }

    Ok(LateFinalizeCreditsArtifact { batches })
}
