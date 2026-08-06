//! Bounded, authenticated Tribute body stream for the LYSIS_V1 exporter.
//!
//! Mongo is used only to discover candidate identities and transport canonical
//! body bytes. Every candidate is reconciled with the exact CE partition view,
//! and final completeness closes against CE count plus JobIntent nominal total.

use std::collections::VecDeque;

use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{
    body_commitment, decode_tribute_v1, tribute_partition_root_from_leaves,
    AuthenticatedTributePartition, CanonicalBodyError, EntityId36, IdPageRequest, TributeBodyV1,
    ACTIVE_COMMITMENT_SCHEME, BODY_SCHEMA_V1,
};
use outbe_offchain_data::{
    read_projection_state, ProjectionConfig, ProjectionError, ProjectionState,
};
use outbe_offchain_storage::{StorageReaderHandle, MAX_SCAN_ENTRIES};
use outbe_tribute::{
    RetainedTributeCursor, RetainedTributePin, RetainedTributeReader, RetainedTributeRef,
    TributeRepositoryError, TributeRepositoryReader,
};
use thiserror::Error;

#[derive(Clone)]
pub struct FinalizedTributeSource {
    storage: StorageReaderHandle,
    current: TributeRepositoryReader,
    retained: RetainedTributeReader,
    page_limit: usize,
}

impl FinalizedTributeSource {
    pub fn new(
        storage: StorageReaderHandle,
        page_limit: usize,
    ) -> Result<Self, FinalizedTributeError> {
        if !(1..=MAX_SCAN_ENTRIES).contains(&page_limit) {
            return Err(FinalizedTributeError::InvalidPageLimit(page_limit));
        }
        Ok(Self {
            storage: storage.clone(),
            current: TributeRepositoryReader::new(storage.clone()),
            retained: RetainedTributeReader::new(storage),
            page_limit,
        })
    }

    /// Reads projector progress through the same read-only storage capability
    /// used for body discovery.
    ///
    /// The checkpoint is availability evidence only. It cannot authorize a
    /// body; every accepted body still closes against the exact CE snapshot.
    pub fn projection_state(
        &self,
        config: ProjectionConfig,
    ) -> Result<Option<ProjectionState>, FinalizedTributeError> {
        read_projection_state(config, self.storage.clone()).map_err(Into::into)
    }

    pub fn stream<'source, 'view>(
        &'source self,
        pin: RetainedTributePin,
        partition: &'view AuthenticatedTributePartition<'view>,
        expected_nominal_total: U256,
    ) -> Result<AuthenticatedTributeStream<'source, 'view>, FinalizedTributeError> {
        Ok(AuthenticatedTributeStream {
            current: CurrentPager::new(&self.current, pin.worldwide_day, self.page_limit)?,
            retained: RetainedPager::new(&self.retained, pin, self.page_limit),
            retained_reader: &self.retained,
            pin,
            partition,
            expected_count: partition.exact_leaf_count,
            expected_nominal_total,
            previous_id: None,
            record_count: 0,
            nominal_total: U256::ZERO,
            exact_body_bytes: 0,
            exhausted: false,
        })
    }

    /// Reconstructs and authenticates one finalized WWD partition from the
    /// service-owned receipt projection. Bodies remain transport data until
    /// their canonical commitments close to the independently sealed root in
    /// the finalized JobIntent.
    pub fn reconstruct_partition(
        &self,
        pin: RetainedTributePin,
        expected_collection_root: B256,
        expected_count: u32,
        expected_nominal_total: U256,
        max_items: usize,
    ) -> Result<ReconstructedTributePartition, FinalizedTributeError> {
        let expected_count_usize = usize::try_from(expected_count).map_err(|_| {
            FinalizedTributeError::CountOutsideProfile {
                count: expected_count,
                limit: max_items,
            }
        })?;
        if expected_count_usize > max_items {
            return Err(FinalizedTributeError::CountOutsideProfile {
                count: expected_count,
                limit: max_items,
            });
        }
        let mut current = CurrentPager::new(&self.current, pin.worldwide_day, self.page_limit)?;
        let mut retained = RetainedPager::new(&self.retained, pin, self.page_limit);
        let mut previous_id = None;
        let mut records = Vec::with_capacity(expected_count_usize);
        let mut leaves = Vec::with_capacity(expected_count_usize);
        let mut nominal_total = U256::ZERO;
        let mut exact_body_bytes = 0_u64;

        loop {
            let candidate = match (current.peek()?, retained.peek()?) {
                (None, None) => break,
                (Some(tribute_id), None) => {
                    current.pop();
                    BodyCandidate {
                        tribute_id,
                        retained_commitment: None,
                    }
                }
                (None, Some(item)) => {
                    retained.pop();
                    BodyCandidate {
                        tribute_id: item.tribute_id,
                        retained_commitment: Some(item.body_commitment),
                    }
                }
                (Some(tribute_id), Some(item)) if tribute_id < item.tribute_id => {
                    current.pop();
                    BodyCandidate {
                        tribute_id,
                        retained_commitment: None,
                    }
                }
                (Some(tribute_id), Some(item)) if tribute_id > item.tribute_id => {
                    retained.pop();
                    BodyCandidate {
                        tribute_id: item.tribute_id,
                        retained_commitment: Some(item.body_commitment),
                    }
                }
                (Some(tribute_id), Some(item)) => {
                    current.pop();
                    retained.pop();
                    BodyCandidate {
                        tribute_id,
                        retained_commitment: Some(item.body_commitment),
                    }
                }
            };
            if previous_id.is_some_and(|previous| candidate.tribute_id <= previous) {
                return Err(FinalizedTributeError::NonAscendingUnion);
            }
            previous_id = Some(candidate.tribute_id);
            if records.len() >= expected_count_usize {
                return Err(FinalizedTributeError::CountMismatch {
                    expected: expected_count,
                    actual: u32::try_from(records.len() + 1).unwrap_or(u32::MAX),
                });
            }
            let stored = match candidate.retained_commitment {
                Some(commitment) => {
                    self.retained
                        .get_current_or_retained(pin, candidate.tribute_id, commitment)?
                }
                None => self.current.get_stored_body(candidate.tribute_id)?,
            }
            .ok_or(FinalizedTributeError::MissingBody(candidate.tribute_id))?;
            if stored.schema_version() != BODY_SCHEMA_V1 {
                return Err(FinalizedTributeError::UnsupportedBodySchema {
                    tribute_id: candidate.tribute_id,
                    actual: stored.schema_version(),
                });
            }
            let body = decode_tribute_v1(stored.payload())?;
            if body.tribute_id != candidate.tribute_id || body.worldwide_day != pin.worldwide_day {
                return Err(FinalizedTributeError::BodyIdentityMismatch(
                    candidate.tribute_id,
                ));
            }
            let commitment = body_commitment(
                ACTIVE_COMMITMENT_SCHEME,
                BODY_SCHEMA_V1,
                candidate.tribute_id,
                stored.payload(),
            )?;
            let commitment = B256::from(*commitment.as_bytes());
            if candidate
                .retained_commitment
                .is_some_and(|retained| retained != commitment)
            {
                return Err(FinalizedTributeError::RetainedIndexCommitmentMismatch(
                    candidate.tribute_id,
                ));
            }
            nominal_total = nominal_total
                .checked_add(body.nominal_amount_minor)
                .ok_or(FinalizedTributeError::NominalOverflow)?;
            exact_body_bytes = exact_body_bytes
                .checked_add(
                    u64::try_from(stored.payload().len())
                        .map_err(|_| FinalizedTributeError::BodyBytesOverflow)?,
                )
                .ok_or(FinalizedTributeError::BodyBytesOverflow)?;
            let canonical_body = stored.payload().to_vec();
            leaves.push((
                candidate.tribute_id,
                outbe_compressed_entities::Commitment::try_from(commitment.0)?,
            ));
            records.push(AuthenticatedTributeRecord {
                tribute_id: candidate.tribute_id,
                commitment,
                canonical_body,
                body,
            });
        }
        let actual_count =
            u32::try_from(records.len()).map_err(|_| FinalizedTributeError::CountOverflow)?;
        if actual_count != expected_count {
            return Err(FinalizedTributeError::CountMismatch {
                expected: expected_count,
                actual: actual_count,
            });
        }
        if nominal_total != expected_nominal_total {
            return Err(FinalizedTributeError::NominalMismatch {
                expected: expected_nominal_total,
                actual: nominal_total,
            });
        }
        let collection_root = tribute_partition_root_from_leaves(pin.worldwide_day, leaves)
            .map_err(|error| FinalizedTributeError::Ce(error.to_string()))?;
        if collection_root != expected_collection_root {
            return Err(FinalizedTributeError::CollectionRootMismatch {
                expected: expected_collection_root,
                actual: collection_root,
            });
        }
        Ok(ReconstructedTributePartition {
            records,
            summary: TributeStreamSummary {
                record_count: actual_count,
                nominal_total,
                exact_body_bytes,
            },
            collection_root,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconstructedTributePartition {
    pub records: Vec<AuthenticatedTributeRecord>,
    pub summary: TributeStreamSummary,
    pub collection_root: B256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedTributeRecord {
    pub tribute_id: EntityId36,
    pub commitment: B256,
    pub canonical_body: Vec<u8>,
    pub body: TributeBodyV1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TributeStreamSummary {
    pub record_count: u32,
    pub nominal_total: U256,
    pub exact_body_bytes: u64,
}

pub struct AuthenticatedTributeStream<'source, 'view> {
    current: CurrentPager<'source>,
    retained: RetainedPager<'source>,
    retained_reader: &'source RetainedTributeReader,
    pin: RetainedTributePin,
    partition: &'view AuthenticatedTributePartition<'view>,
    expected_count: u32,
    expected_nominal_total: U256,
    previous_id: Option<EntityId36>,
    record_count: u32,
    nominal_total: U256,
    exact_body_bytes: u64,
    exhausted: bool,
}

impl AuthenticatedTributeStream<'_, '_> {
    pub fn next_record(
        &mut self,
    ) -> Result<Option<AuthenticatedTributeRecord>, FinalizedTributeError> {
        if self.exhausted {
            return Ok(None);
        }
        let Some(candidate) = self.next_candidate()? else {
            self.exhausted = true;
            return Ok(None);
        };
        if self
            .previous_id
            .is_some_and(|previous| candidate.tribute_id <= previous)
        {
            return Err(FinalizedTributeError::NonAscendingUnion);
        }
        self.previous_id = Some(candidate.tribute_id);

        let commitment = self
            .partition
            .tribute_commitment(candidate.tribute_id)
            .map_err(|error| FinalizedTributeError::Ce(error.to_string()))?
            .ok_or(FinalizedTributeError::UnauthenticatedCandidate(
                candidate.tribute_id,
            ))?;
        let commitment = B256::from(*commitment.as_bytes());
        if candidate
            .retained_commitment
            .is_some_and(|retained| retained != commitment)
        {
            return Err(FinalizedTributeError::RetainedIndexCommitmentMismatch(
                candidate.tribute_id,
            ));
        }
        let stored = self
            .retained_reader
            .get_current_or_retained(self.pin, candidate.tribute_id, commitment)?
            .ok_or(FinalizedTributeError::MissingBody(candidate.tribute_id))?;
        if stored.schema_version() != BODY_SCHEMA_V1 {
            return Err(FinalizedTributeError::UnsupportedBodySchema {
                tribute_id: candidate.tribute_id,
                actual: stored.schema_version(),
            });
        }
        let body = decode_tribute_v1(stored.payload())?;
        if body.tribute_id != candidate.tribute_id || body.worldwide_day != self.pin.worldwide_day {
            return Err(FinalizedTributeError::BodyIdentityMismatch(
                candidate.tribute_id,
            ));
        }

        self.record_count = self
            .record_count
            .checked_add(1)
            .ok_or(FinalizedTributeError::CountOverflow)?;
        if self.record_count > self.expected_count {
            return Err(FinalizedTributeError::CountMismatch {
                expected: self.expected_count,
                actual: self.record_count,
            });
        }
        self.nominal_total = self
            .nominal_total
            .checked_add(body.nominal_amount_minor)
            .ok_or(FinalizedTributeError::NominalOverflow)?;
        if self.nominal_total > self.expected_nominal_total {
            return Err(FinalizedTributeError::NominalMismatch {
                expected: self.expected_nominal_total,
                actual: self.nominal_total,
            });
        }
        let body_len = u64::try_from(stored.payload().len())
            .map_err(|_| FinalizedTributeError::BodyBytesOverflow)?;
        self.exact_body_bytes = self
            .exact_body_bytes
            .checked_add(body_len)
            .ok_or(FinalizedTributeError::BodyBytesOverflow)?;

        Ok(Some(AuthenticatedTributeRecord {
            tribute_id: candidate.tribute_id,
            commitment,
            canonical_body: stored.payload().to_vec(),
            body,
        }))
    }

    pub fn finish(self) -> Result<TributeStreamSummary, FinalizedTributeError> {
        if !self.exhausted {
            return Err(FinalizedTributeError::StreamNotExhausted);
        }
        if self.record_count != self.expected_count {
            return Err(FinalizedTributeError::CountMismatch {
                expected: self.expected_count,
                actual: self.record_count,
            });
        }
        if self.nominal_total != self.expected_nominal_total {
            return Err(FinalizedTributeError::NominalMismatch {
                expected: self.expected_nominal_total,
                actual: self.nominal_total,
            });
        }
        Ok(TributeStreamSummary {
            record_count: self.record_count,
            nominal_total: self.nominal_total,
            exact_body_bytes: self.exact_body_bytes,
        })
    }

    fn next_candidate(&mut self) -> Result<Option<BodyCandidate>, FinalizedTributeError> {
        let current = self.current.peek()?;
        let retained = self.retained.peek()?;
        match (current, retained) {
            (None, None) => Ok(None),
            (Some(tribute_id), None) => {
                self.current.pop();
                Ok(Some(BodyCandidate {
                    tribute_id,
                    retained_commitment: None,
                }))
            }
            (None, Some(retained)) => {
                self.retained.pop();
                Ok(Some(BodyCandidate {
                    tribute_id: retained.tribute_id,
                    retained_commitment: Some(retained.body_commitment),
                }))
            }
            (Some(current), Some(retained)) if current < retained.tribute_id => {
                self.current.pop();
                Ok(Some(BodyCandidate {
                    tribute_id: current,
                    retained_commitment: None,
                }))
            }
            (Some(current), Some(retained)) if current > retained.tribute_id => {
                self.retained.pop();
                Ok(Some(BodyCandidate {
                    tribute_id: retained.tribute_id,
                    retained_commitment: Some(retained.body_commitment),
                }))
            }
            (Some(current), Some(retained)) => {
                self.current.pop();
                self.retained.pop();
                Ok(Some(BodyCandidate {
                    tribute_id: current,
                    retained_commitment: Some(retained.body_commitment),
                }))
            }
        }
    }
}

#[derive(Clone, Copy)]
struct BodyCandidate {
    tribute_id: EntityId36,
    retained_commitment: Option<B256>,
}

struct CurrentPager<'a> {
    reader: &'a TributeRepositoryReader,
    day: outbe_common::WorldwideDay,
    limit: u32,
    cursor: Option<EntityId36>,
    buffered: VecDeque<EntityId36>,
    complete: bool,
}

impl<'a> CurrentPager<'a> {
    fn new(
        reader: &'a TributeRepositoryReader,
        day: outbe_common::WorldwideDay,
        page_limit: usize,
    ) -> Result<Self, FinalizedTributeError> {
        Ok(Self {
            reader,
            day,
            limit: u32::try_from(page_limit)
                .map_err(|_| FinalizedTributeError::InvalidPageLimit(page_limit))?,
            cursor: None,
            buffered: VecDeque::new(),
            complete: false,
        })
    }

    fn peek(&mut self) -> Result<Option<EntityId36>, FinalizedTributeError> {
        self.fill()?;
        Ok(self.buffered.front().copied())
    }

    fn pop(&mut self) {
        let _ = self.buffered.pop_front();
    }

    fn fill(&mut self) -> Result<(), FinalizedTributeError> {
        if !self.buffered.is_empty() || self.complete {
            return Ok(());
        }
        let before = self.cursor;
        let page = self.reader.list_ids_by_day(
            self.day,
            IdPageRequest {
                after: self.cursor,
                limit: self.limit,
            },
        )?;
        if page.ids.is_empty() && page.next_after.is_some() {
            return Err(FinalizedTributeError::PaginationCycle);
        }
        if page.next_after.is_some_and(|after| Some(after) <= before) {
            return Err(FinalizedTributeError::PaginationCycle);
        }
        self.cursor = page.next_after;
        self.complete = self.cursor.is_none();
        self.buffered = page.ids.into();
        Ok(())
    }
}

struct RetainedPager<'a> {
    reader: &'a RetainedTributeReader,
    pin: RetainedTributePin,
    limit: usize,
    cursor: Option<RetainedTributeCursor>,
    buffered: VecDeque<RetainedTributeRef>,
    complete: bool,
}

impl<'a> RetainedPager<'a> {
    const fn new(reader: &'a RetainedTributeReader, pin: RetainedTributePin, limit: usize) -> Self {
        Self {
            reader,
            pin,
            limit,
            cursor: None,
            buffered: VecDeque::new(),
            complete: false,
        }
    }

    fn peek(&mut self) -> Result<Option<RetainedTributeRef>, FinalizedTributeError> {
        self.fill()?;
        Ok(self.buffered.front().copied())
    }

    fn pop(&mut self) {
        let _ = self.buffered.pop_front();
    }

    fn fill(&mut self) -> Result<(), FinalizedTributeError> {
        if !self.buffered.is_empty() || self.complete {
            return Ok(());
        }
        let before = self.cursor;
        let page = self.reader.list_by_day(self.pin, self.cursor, self.limit)?;
        if page.records.is_empty() && page.next_after.is_some() {
            return Err(FinalizedTributeError::PaginationCycle);
        }
        if page.next_after.is_some_and(|after| {
            before.is_some_and(|before| {
                (after.tribute_id, after.body_commitment)
                    <= (before.tribute_id, before.body_commitment)
            })
        }) {
            return Err(FinalizedTributeError::PaginationCycle);
        }
        self.cursor = page.next_after;
        self.complete = self.cursor.is_none();
        self.buffered = page.records.into();
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum FinalizedTributeError {
    #[error(transparent)]
    Projection(#[from] ProjectionError),
    #[error(transparent)]
    Repository(#[from] TributeRepositoryError),
    #[error(transparent)]
    CanonicalBody(#[from] CanonicalBodyError),
    #[error(transparent)]
    Commitment(#[from] outbe_compressed_entities::CommitmentError),
    #[error("export page limit {0} is outside the storage bound")]
    InvalidPageLimit(usize),
    #[error("Tribute source pagination did not advance")]
    PaginationCycle,
    #[error("current/retained Tribute union is not strictly ascending")]
    NonAscendingUnion,
    #[error("candidate Tribute {0} has no authenticated CE leaf")]
    UnauthenticatedCandidate(EntityId36),
    #[error("retained index commitment differs from exact CE leaf for Tribute {0}")]
    RetainedIndexCommitmentMismatch(EntityId36),
    #[error("canonical current/retained body is missing for Tribute {0}")]
    MissingBody(EntityId36),
    #[error("Tribute {tribute_id} uses unsupported stored-body schema {actual}")]
    UnsupportedBodySchema { tribute_id: EntityId36, actual: u32 },
    #[error("canonical body identity/day differs from candidate Tribute {0}")]
    BodyIdentityMismatch(EntityId36),
    #[error("authenticated CE read failed: {0}")]
    Ce(String),
    #[error("Tribute record count overflow")]
    CountOverflow,
    #[error("Tribute count {count} exceeds profile limit {limit}")]
    CountOutsideProfile { count: u32, limit: usize },
    #[error("Tribute nominal total overflow")]
    NominalOverflow,
    #[error("Tribute canonical body byte count overflow")]
    BodyBytesOverflow,
    #[error("Tribute count mismatch: expected {expected}, got {actual}")]
    CountMismatch { expected: u32, actual: u32 },
    #[error("reconstructed Tribute collection root mismatch: expected {expected}, got {actual}")]
    CollectionRootMismatch { expected: B256, actual: B256 },
    #[error("Tribute nominal mismatch: expected {expected}, got {actual}")]
    NominalMismatch { expected: U256, actual: U256 },
    #[error("authenticated Tribute stream must be exhausted before closure")]
    StreamNotExhausted,
}
