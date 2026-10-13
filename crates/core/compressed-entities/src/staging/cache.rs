use super::*;

/// Benchmark-fixed bounds for the local candidate cache. The cache evicts no
/// entry implicitly, so a required pending-finalized candidate cannot disappear
/// under pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CandidateCacheLimits {
    pub max_candidates: usize,
    pub max_encoded_bytes: usize,
}

#[derive(Debug)]
pub struct CandidateCache {
    limits: CandidateCacheLimits,
    encoded_bytes: usize,
    candidates: BTreeMap<B256, Arc<StagedTreeBatch>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationOutcome {
    Published,
    AlreadyPublished,
}

impl CandidateCache {
    #[must_use]
    pub fn new(limits: CandidateCacheLimits) -> Self {
        Self {
            limits,
            encoded_bytes: 0,
            candidates: BTreeMap::new(),
        }
    }

    pub fn publish(&mut self, batch: StagedTreeBatch) -> Result<PublicationOutcome, StagingError> {
        batch.validate_encoded_size()?;
        if let Some(existing) = self.candidates.get(&batch.block_hash) {
            if existing.as_ref() == &batch {
                return Ok(PublicationOutcome::AlreadyPublished);
            }
            return Err(StagingError::ConflictingPublication {
                block_hash: batch.block_hash,
            });
        }

        let candidate_count = self
            .candidates
            .len()
            .checked_add(1)
            .ok_or(StagingError::CacheCapacity)?;
        let encoded_bytes = self
            .encoded_bytes
            .checked_add(batch.encoded_size)
            .ok_or(StagingError::CacheCapacity)?;
        if candidate_count > self.limits.max_candidates
            || encoded_bytes > self.limits.max_encoded_bytes
        {
            return Err(StagingError::CacheCapacity);
        }

        self.encoded_bytes = encoded_bytes;
        self.candidates.insert(batch.block_hash, Arc::new(batch));
        Ok(PublicationOutcome::Published)
    }

    #[must_use]
    pub fn get(&self, block_hash: B256) -> Option<Arc<StagedTreeBatch>> {
        self.candidates.get(&block_hash).cloned()
    }

    /// Removes one rejected speculative payload without disturbing competing
    /// candidates. Open readers retain their immutable `Arc`.
    pub fn remove(&mut self, block_hash: B256) -> Option<Arc<StagedTreeBatch>> {
        let removed = self.candidates.remove(&block_hash);
        if let Some(batch) = &removed {
            self.encoded_bytes = self.encoded_bytes.saturating_sub(batch.encoded_size);
        }
        removed
    }

    /// Removes the winning entry and every losing candidate at or below the
    /// committed height. Open readers keep their own `Arc` immutable batch.
    pub fn remove_finalized(&mut self, height: u64) {
        let removed: Vec<_> = self
            .candidates
            .iter()
            .filter_map(|(hash, batch)| (batch.block_number <= height).then_some(*hash))
            .collect();
        for hash in removed {
            self.remove(hash);
        }
    }

    /// Restart deliberately discards all speculative candidates.
    pub fn discard_all_on_restart(&mut self) {
        self.candidates.clear();
        self.encoded_bytes = 0;
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    #[must_use]
    pub const fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }
}
