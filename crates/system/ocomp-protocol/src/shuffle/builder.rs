//! Build bounded shuffle runs and stage their canonical CAS objects.

use alloy_primitives::{keccak256, Address, B256};

use crate::{
    control::CasObjectRefV1,
    error::ProtocolError,
    registry::ObjectKind,
    result::ContributorActionV1,
    schema::{require, SchemaLimits},
};

use super::{
    require_valid_run_span, ShuffleBucketRecordV1, ShufflePageSpanV1, ShuffleRunArtifactV1,
    ShuffleRunBuildContextV1, ShuffleRunKindV1, ShuffleRunPayloadV1, VerifiedShuffleRecordV1,
    MAX_SHUFFLE_LEAF_RECORDS,
};

trait ShuffleRunRecord {
    const KIND: ShuffleRunKindV1;

    fn into_verified(self) -> VerifiedShuffleRecordV1;
}

impl ShuffleRunRecord for ContributorActionV1 {
    const KIND: ShuffleRunKindV1 = ShuffleRunKindV1::Owner;

    fn into_verified(self) -> VerifiedShuffleRecordV1 {
        VerifiedShuffleRecordV1::Owner(self)
    }
}

impl ShuffleRunRecord for ShuffleBucketRecordV1 {
    const KIND: ShuffleRunKindV1 = ShuffleRunKindV1::Bucket;

    fn into_verified(self) -> VerifiedShuffleRecordV1 {
        VerifiedShuffleRecordV1::Bucket(self)
    }
}

pub fn build_owner_shuffle_run<I, S>(
    context: ShuffleRunBuildContextV1,
    records: I,
    limits: &SchemaLimits,
    stage: S,
) -> Result<ShuffleRunArtifactV1, ProtocolError>
where
    I: IntoIterator<Item = Result<ContributorActionV1, ProtocolError>>,
    S: FnMut(&[u8]) -> Result<CasObjectRefV1, ProtocolError>,
{
    build_typed_shuffle_run(context, records, limits, stage)
}

pub fn build_bucket_shuffle_run<I, S>(
    context: ShuffleRunBuildContextV1,
    records: I,
    limits: &SchemaLimits,
    stage: S,
) -> Result<ShuffleRunArtifactV1, ProtocolError>
where
    I: IntoIterator<Item = Result<ShuffleBucketRecordV1, ProtocolError>>,
    S: FnMut(&[u8]) -> Result<CasObjectRefV1, ProtocolError>,
{
    build_typed_shuffle_run(context, records, limits, stage)
}

fn build_typed_shuffle_run<I, S, T>(
    context: ShuffleRunBuildContextV1,
    records: I,
    limits: &SchemaLimits,
    stage: S,
) -> Result<ShuffleRunArtifactV1, ProtocolError>
where
    I: IntoIterator<Item = Result<T, ProtocolError>>,
    T: ShuffleRunRecord,
    S: FnMut(&[u8]) -> Result<CasObjectRefV1, ProtocolError>,
{
    let records = records
        .into_iter()
        .map(|record| record.map(T::into_verified));
    let mut builder = ShuffleRunBuilder::new(context, T::KIND, limits, stage)?;
    for record in records {
        builder.push(record?)?;
    }
    builder.finish()
}

struct BuiltShuffleSubtreeV1 {
    artifact: ShuffleRunArtifactV1,
    reference: CasObjectRefV1,
}

struct ShuffleRunBuilder<'a, S> {
    context: ShuffleRunBuildContextV1,
    kind: ShuffleRunKindV1,
    limits: &'a SchemaLimits,
    stage: S,
    frontier: Vec<Option<BuiltShuffleSubtreeV1>>,
    page: Vec<VerifiedShuffleRecordV1>,
    page_ordinal: u32,
    first_record_ordinal: u32,
    previous_owner: Option<(Address, B256)>,
    previous_bucket: Option<(B256, u32)>,
}

impl<'a, S> ShuffleRunBuilder<'a, S>
where
    S: FnMut(&[u8]) -> Result<CasObjectRefV1, ProtocolError>,
{
    fn new(
        context: ShuffleRunBuildContextV1,
        kind: ShuffleRunKindV1,
        limits: &'a SchemaLimits,
        stage: S,
    ) -> Result<Self, ProtocolError> {
        require_valid_run_span(&context.run_span)?;
        let job_identity = !context.protocol_bundle_hash.is_zero()
            && !context.job_id.is_zero()
            && !context.unit_id.is_zero();
        let source_coverage =
            !context.source_coverage_root.is_zero() && context.source_coverage_count > 0;
        require(job_identity && source_coverage, "shuffle build context")?;
        Ok(Self {
            context,
            kind,
            limits,
            stage,
            frontier: Vec::new(),
            page: Vec::with_capacity(MAX_SHUFFLE_LEAF_RECORDS),
            page_ordinal: 0,
            first_record_ordinal: 0,
            previous_owner: None,
            previous_bucket: None,
        })
    }

    fn push(&mut self, record: VerifiedShuffleRecordV1) -> Result<(), ProtocolError> {
        self.validate_record(&record)?;
        self.page.push(record);
        if self.page.len() == MAX_SHUFFLE_LEAF_RECORDS {
            let records = core::mem::take(&mut self.page);
            let leaf = self.build_leaf(records)?;
            self.push_subtree(leaf)?;
            self.page_ordinal =
                self.page_ordinal
                    .checked_add(1)
                    .ok_or(ProtocolError::IntegerOverflow {
                        what: "shuffle output page ordinal",
                    })?;
            self.first_record_ordinal = self
                .first_record_ordinal
                .checked_add(MAX_SHUFFLE_LEAF_RECORDS as u32)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "shuffle output record ordinal",
                })?;
            self.page = Vec::with_capacity(MAX_SHUFFLE_LEAF_RECORDS);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<ShuffleRunArtifactV1, ProtocolError> {
        if !self.page.is_empty()
            || (self.first_record_ordinal == 0 && self.kind == ShuffleRunKindV1::Owner)
        {
            let records = core::mem::take(&mut self.page);
            let leaf = self.build_leaf(records)?;
            self.push_subtree(leaf)?;
        } else if self.first_record_ordinal == 0 {
            return Err(ProtocolError::InvalidInvariant(
                "non-empty bucket shuffle run",
            ));
        }

        let mut root = None;
        for subtree in core::mem::take(&mut self.frontier).into_iter().flatten() {
            root = Some(match root {
                None => subtree,
                Some(right) => self.build_node(subtree, right)?,
            });
        }
        let root = root
            .ok_or(ProtocolError::InvalidInvariant("shuffle output root"))?
            .artifact;
        root.validate_root_semantics(self.limits)?;
        Ok(root)
    }

    fn validate_record(&mut self, record: &VerifiedShuffleRecordV1) -> Result<(), ProtocolError> {
        match record {
            VerifiedShuffleRecordV1::Owner(record) => {
                require(
                    self.kind == ShuffleRunKindV1::Owner,
                    "shuffle build record kind",
                )?;
                let key = (record.owner, record.source_tribute_id);
                require(
                    self.previous_owner
                        .is_none_or(|previous| previous.0 < key.0)
                        && self.previous_bucket.is_none(),
                    "global owner shuffle order",
                )?;
                self.previous_owner = Some(key);
            }
            VerifiedShuffleRecordV1::Bucket(record) => {
                require(
                    self.kind == ShuffleRunKindV1::Bucket,
                    "shuffle build record kind",
                )?;
                let key = (record.bucket_key, record.raw_ordinal);
                require(
                    self.previous_bucket.is_none_or(|previous| previous < key)
                        && self.previous_owner.is_none(),
                    "global bucket shuffle order",
                )?;
                self.previous_bucket = Some(key);
            }
        }
        Ok(())
    }

    fn build_leaf(
        &mut self,
        records: Vec<VerifiedShuffleRecordV1>,
    ) -> Result<BuiltShuffleSubtreeV1, ProtocolError> {
        let record_count =
            u32::try_from(records.len()).map_err(|_| ProtocolError::IntegerOverflow {
                what: "shuffle leaf record count",
            })?;
        let payload = match self.kind {
            ShuffleRunKindV1::Owner => ShuffleRunPayloadV1::OwnerLeaf(
                records
                    .into_iter()
                    .map(|record| match record {
                        VerifiedShuffleRecordV1::Owner(record) => Ok(record),
                        VerifiedShuffleRecordV1::Bucket(_) => Err(ProtocolError::InvalidInvariant(
                            "shuffle build owner record",
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            ShuffleRunKindV1::Bucket => ShuffleRunPayloadV1::BucketLeaf(
                records
                    .into_iter()
                    .map(|record| match record {
                        VerifiedShuffleRecordV1::Bucket(record) => Ok(record),
                        VerifiedShuffleRecordV1::Owner(_) => Err(ProtocolError::InvalidInvariant(
                            "shuffle build bucket record",
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        };
        let page_span = ShufflePageSpanV1 {
            start_page: self.page_ordinal,
            end_page: self
                .page_ordinal
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "shuffle leaf page span",
                })?,
        };
        let artifact = self
            .artifact(page_span, self.first_record_ordinal, record_count, payload)
            .with_recomputed_ordered_record_root(self.limits)?;
        self.stage_artifact(artifact)
    }

    fn push_subtree(&mut self, mut subtree: BuiltShuffleSubtreeV1) -> Result<(), ProtocolError> {
        let mut level = 0_usize;
        loop {
            if level == self.frontier.len() {
                self.frontier.push(Some(subtree));
                return Ok(());
            }
            let Some(left) = self.frontier[level].take() else {
                self.frontier[level] = Some(subtree);
                return Ok(());
            };
            subtree = self.build_node(left, subtree)?;
            level = level.checked_add(1).ok_or(ProtocolError::IntegerOverflow {
                what: "shuffle builder frontier level",
            })?;
        }
    }

    fn build_node(
        &mut self,
        left: BuiltShuffleSubtreeV1,
        right: BuiltShuffleSubtreeV1,
    ) -> Result<BuiltShuffleSubtreeV1, ProtocolError> {
        let page_span = ShufflePageSpanV1 {
            start_page: left.artifact.page_span.start_page,
            end_page: right.artifact.page_span.end_page,
        };
        let record_count = left
            .artifact
            .record_count
            .checked_add(right.artifact.record_count)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "shuffle node output count",
            })?;
        let first_record_ordinal = left.artifact.first_record_ordinal;
        let artifact = self
            .artifact(
                page_span,
                first_record_ordinal,
                record_count,
                ShuffleRunPayloadV1::Node {
                    left: child_summary(left),
                    right: child_summary(right),
                },
            )
            .with_recomputed_ordered_record_root(self.limits)?;
        self.stage_artifact(artifact)
    }

    fn artifact(
        &self,
        page_span: ShufflePageSpanV1,
        first_record_ordinal: u32,
        record_count: u32,
        payload: ShuffleRunPayloadV1,
    ) -> ShuffleRunArtifactV1 {
        ShuffleRunArtifactV1 {
            protocol_bundle_hash: self.context.protocol_bundle_hash,
            job_id: self.context.job_id,
            attempt: self.context.attempt,
            unit_id: self.context.unit_id,
            kind: self.kind,
            run_span: self.context.run_span.clone(),
            page_span,
            first_record_ordinal,
            record_count,
            source_coverage_root: self.context.source_coverage_root,
            source_coverage_count: self.context.source_coverage_count,
            ordered_record_root: B256::ZERO,
            payload,
        }
    }

    fn stage_artifact(
        &mut self,
        artifact: ShuffleRunArtifactV1,
    ) -> Result<BuiltShuffleSubtreeV1, ProtocolError> {
        let bytes = artifact.encode_canonical(self.limits)?;
        let reference = (self.stage)(&bytes)?;
        let encoded_bytes =
            u64::try_from(bytes.len()).map_err(|_| ProtocolError::IntegerOverflow {
                what: "staged shuffle object bytes",
            })?;
        require(
            reference.transport_digest == keccak256(&bytes)
                && reference.encoded_bytes == encoded_bytes
                && reference.expected_ocb1_kind == Some(ObjectKind::ShuffleRunArtifactV1.tag()),
            "staged shuffle object descriptor",
        )?;
        Ok(BuiltShuffleSubtreeV1 {
            artifact,
            reference,
        })
    }
}

fn child_summary(subtree: BuiltShuffleSubtreeV1) -> super::ShuffleRunChildV1 {
    super::ShuffleRunChildV1 {
        artifact_ref: subtree.reference,
        page_span: subtree.artifact.page_span,
        first_record_ordinal: subtree.artifact.first_record_ordinal,
        record_count: subtree.artifact.record_count,
        ordered_record_root: subtree.artifact.ordered_record_root,
    }
}
