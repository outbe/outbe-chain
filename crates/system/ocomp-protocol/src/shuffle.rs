//! Bounded, Lysis-specific owner/bucket shuffle run artifacts.
//!
//! This closed schema is deliberately not a generic DAG or spill-file
//! framework. A `UnitArtifactV1` embeds one root object; bounded descendants
//! are addressed by their verified CAS references.

use std::collections::BTreeSet;

use alloy_primitives::B256;

use crate::{
    codec::{CanonicalReader, CanonicalWriter},
    common::EntityId36,
    control::CasObjectRefV1,
    error::ProtocolError,
    hash::hash_framed,
    list::StreamingOrderedListRoot,
    registry::{HashDomain, ListKind, ObjectKind},
    result::ContributorActionV1,
    schema::{
        encode_nested_value, impl_top_level_codec, require, wire_enum_u8, wire_struct, NestedCodec,
        SchemaLimits,
    },
    unit::CanonicalRunSpan,
};

/// One shuffle leaf never owns more records than one primary Lysis work shard.
pub const MAX_SHUFFLE_LEAF_RECORDS: usize = 256;

wire_enum_u8! {
    pub enum ShuffleRunKindV1 {
        Owner = 1,
        Bucket = 2,
    }
}

wire_struct! {
    pub struct ShufflePageSpanV1 {
        pub start_page: u32,
        pub end_page: u32,
    }
    validate = validate_page_span;
}

wire_struct! {
    pub struct ShuffleBucketRecordV1 {
        pub bucket_key: B256,
        pub raw_ordinal: u32,
        pub tribute_id: EntityId36,
        pub nod_id: EntityId36,
    }
}

wire_struct! {
    /// Authenticated summary of one child object. The referenced child repeats
    /// the root's immutable identity and source-coverage fields.
    pub struct ShuffleRunChildV1 {
        pub artifact_ref: CasObjectRefV1,
        pub page_span: ShufflePageSpanV1,
        pub first_record_ordinal: u32,
        pub record_count: u32,
        pub ordered_record_root: B256,
    }
    validate = validate_child;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShuffleRunPayloadV1 {
    OwnerLeaf(Vec<ContributorActionV1>),
    BucketLeaf(Vec<ShuffleBucketRecordV1>),
    Node {
        left: ShuffleRunChildV1,
        right: ShuffleRunChildV1,
    },
}

impl NestedCodec for ShuffleRunPayloadV1 {
    fn validate(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        match self {
            Self::OwnerLeaf(records) => validate_owner_records(records, limits),
            Self::BucketLeaf(records) => validate_bucket_records(records, limits),
            Self::Node { left, right } => {
                left.validate(limits)?;
                right.validate(limits)
            }
        }
    }

    fn encode_nested(
        &self,
        output: &mut CanonicalWriter,
        limits: &SchemaLimits,
    ) -> Result<(), ProtocolError> {
        output.write_u8(match self {
            Self::OwnerLeaf(_) => 1,
            Self::BucketLeaf(_) => 2,
            Self::Node { .. } => 3,
        })?;
        match self {
            Self::OwnerLeaf(records) => records.encode_nested(output, limits),
            Self::BucketLeaf(records) => records.encode_nested(output, limits),
            Self::Node { left, right } => {
                left.encode_nested(output, limits)?;
                right.encode_nested(output, limits)
            }
        }
    }

    fn decode_nested(
        input: &mut CanonicalReader<'_>,
        limits: &SchemaLimits,
    ) -> Result<Self, ProtocolError> {
        match input.read_u8()? {
            1 => Ok(Self::OwnerLeaf(Vec::<ContributorActionV1>::decode_nested(
                input, limits,
            )?)),
            2 => Ok(Self::BucketLeaf(
                Vec::<ShuffleBucketRecordV1>::decode_nested(input, limits)?,
            )),
            3 => Ok(Self::Node {
                left: ShuffleRunChildV1::decode_nested(input, limits)?,
                right: ShuffleRunChildV1::decode_nested(input, limits)?,
            }),
            value => Err(ProtocolError::UnknownEnum {
                width: 8,
                value: u16::from(value),
            }),
        }
    }
}

wire_struct! {
    pub struct ShuffleRunArtifactV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub unit_id: B256,
        pub kind: ShuffleRunKindV1,
        pub run_span: CanonicalRunSpan,
        pub page_span: ShufflePageSpanV1,
        pub first_record_ordinal: u32,
        pub record_count: u32,
        pub source_coverage_root: B256,
        pub source_coverage_count: u32,
        pub ordered_record_root: B256,
        pub payload: ShuffleRunPayloadV1,
    }
    validate = validate_shuffle_run_artifact;
}
impl_top_level_codec!(ShuffleRunArtifactV1, ShuffleRunArtifactV1);

impl ShuffleRunArtifactV1 {
    /// Binds the canonical leaf-list or binary-node commitment after all other
    /// fields and child summaries have been selected.
    pub fn with_recomputed_ordered_record_root(
        mut self,
        limits: &SchemaLimits,
    ) -> Result<Self, ProtocolError> {
        self.ordered_record_root = self.recompute_ordered_record_root(limits)?;
        Ok(self)
    }

    pub fn recompute_ordered_record_root(
        &self,
        limits: &SchemaLimits,
    ) -> Result<B256, ProtocolError> {
        match &self.payload {
            ShuffleRunPayloadV1::OwnerLeaf(records) => {
                validate_owner_records(records, limits)?;
                ordered_leaf_root(
                    ListKind::ContributorActions,
                    records
                        .iter()
                        .map(|record| encode_nested_value(record, limits)),
                    records.len(),
                    limits,
                )
            }
            ShuffleRunPayloadV1::BucketLeaf(records) => {
                validate_bucket_records(records, limits)?;
                ordered_leaf_root(
                    ListKind::BucketRecords,
                    records
                        .iter()
                        .map(|record| encode_nested_value(record, limits)),
                    records.len(),
                    limits,
                )
            }
            ShuffleRunPayloadV1::Node { left, right } => {
                left.validate(limits)?;
                right.validate(limits)?;
                let mut payload = CanonicalWriter::new(limits.codec);
                payload.write_u8(self.kind as u8)?;
                payload.write_u32(self.page_span.start_page)?;
                payload.write_u32(self.page_span.end_page)?;
                payload.write_u32(self.first_record_ordinal)?;
                payload.write_u32(self.record_count)?;
                payload.write_b256(left.ordered_record_root)?;
                payload.write_b256(right.ordered_record_root)?;
                hash_framed(HashDomain::ShuffleRunNode, payload.as_slice())
            }
        }
    }

    /// Validates one root embedded by a producing `UnitArtifactV1`.
    pub fn validate_root_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        self.validate_semantics(limits)?;
        require(self.page_span.start_page == 0, "shuffle root first page")?;
        require(
            self.first_record_ordinal == 0,
            "shuffle root first record ordinal",
        )
    }

    /// Validates the fields that are self-contained in this object. A consumer
    /// additionally opens child references and compares their repeated fields.
    pub fn validate_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        <Self as NestedCodec>::validate(self, limits)
    }
}

fn validate_page_span(
    span: &ShufflePageSpanV1,
    _limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    require(
        span.start_page < span.end_page,
        "non-empty shuffle page span",
    )
}

fn validate_child(child: &ShuffleRunChildV1, _limits: &SchemaLimits) -> Result<(), ProtocolError> {
    require(
        !child.artifact_ref.transport_digest.is_zero()
            && child.artifact_ref.encoded_bytes > 0
            && child.artifact_ref.expected_ocb1_kind
                == Some(ObjectKind::ShuffleRunArtifactV1.tag()),
        "typed shuffle child CAS reference",
    )?;
    require(
        !child.ordered_record_root.is_zero(),
        "shuffle child ordered record root",
    )?;
    checked_record_end(child.first_record_ordinal, child.record_count)?;
    Ok(())
}

fn validate_shuffle_run_artifact(
    artifact: &ShuffleRunArtifactV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    require(
        !artifact.protocol_bundle_hash.is_zero()
            && !artifact.job_id.is_zero()
            && !artifact.unit_id.is_zero(),
        "shuffle artifact identity",
    )?;
    require(
        artifact.run_span.start_run < artifact.run_span.end_run,
        "non-empty shuffle run span",
    )?;
    require(
        artifact.source_coverage_count > 0 && !artifact.source_coverage_root.is_zero(),
        "shuffle source coverage",
    )?;
    require(
        !artifact.ordered_record_root.is_zero(),
        "shuffle ordered record root",
    )?;
    checked_record_end(artifact.first_record_ordinal, artifact.record_count)?;

    let page_width = artifact
        .page_span
        .end_page
        .checked_sub(artifact.page_span.start_page)
        .ok_or(ProtocolError::IntegerOverflow {
            what: "shuffle page width",
        })?;
    match &artifact.payload {
        ShuffleRunPayloadV1::OwnerLeaf(records) => {
            require(
                artifact.kind == ShuffleRunKindV1::Owner,
                "shuffle payload kind binding",
            )?;
            validate_leaf_shape(artifact, page_width, records.len())
        }
        ShuffleRunPayloadV1::BucketLeaf(records) => {
            require(
                artifact.kind == ShuffleRunKindV1::Bucket,
                "shuffle payload kind binding",
            )?;
            require(!records.is_empty(), "non-empty bucket shuffle leaf")?;
            validate_leaf_shape(artifact, page_width, records.len())
        }
        ShuffleRunPayloadV1::Node { left, right } => {
            require(page_width > 1, "shuffle node page width")?;
            if artifact.kind == ShuffleRunKindV1::Bucket {
                require(
                    artifact.record_count > 0 && left.record_count > 0 && right.record_count > 0,
                    "non-empty bucket shuffle node",
                )?;
            }
            require(
                left.artifact_ref.transport_digest != right.artifact_ref.transport_digest,
                "shuffle node distinct child objects",
            )?;

            let split = canonical_page_split(&artifact.page_span)?;
            require(
                left.page_span.start_page == artifact.page_span.start_page
                    && left.page_span.end_page == split
                    && right.page_span.start_page == split
                    && right.page_span.end_page == artifact.page_span.end_page,
                "shuffle node canonical page split",
            )?;
            let right_first = checked_record_end(left.first_record_ordinal, left.record_count)?;
            require(
                left.first_record_ordinal == artifact.first_record_ordinal
                    && right.first_record_ordinal == right_first,
                "shuffle node record adjacency",
            )?;
            let child_count = left.record_count.checked_add(right.record_count).ok_or(
                ProtocolError::IntegerOverflow {
                    what: "shuffle node record count",
                },
            )?;
            require(
                child_count == artifact.record_count,
                "shuffle node record count",
            )
        }
    }?;
    require(
        artifact.ordered_record_root == artifact.recompute_ordered_record_root(limits)?,
        "shuffle ordered record root",
    )
}

fn validate_leaf_shape(
    artifact: &ShuffleRunArtifactV1,
    page_width: u32,
    actual_records: usize,
) -> Result<(), ProtocolError> {
    require(page_width == 1, "shuffle leaf page width")?;
    require(
        actual_records <= MAX_SHUFFLE_LEAF_RECORDS,
        "shuffle leaf record cap",
    )?;
    require(
        usize::try_from(artifact.record_count).ok() == Some(actual_records),
        "shuffle leaf record count",
    )
}

fn validate_owner_records(
    records: &[ContributorActionV1],
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    require(
        records.len() <= limits.max_chunk_items && records.len() <= MAX_SHUFFLE_LEAF_RECORDS,
        "shuffle leaf record cap",
    )?;
    for record in records {
        record.validate(limits)?;
    }
    for pair in records.windows(2) {
        require(
            (pair[0].owner, pair[0].source_tribute_id) < (pair[1].owner, pair[1].source_tribute_id)
                && pair[0].owner != pair[1].owner,
            "owner shuffle records strictly ordered",
        )?;
    }
    Ok(())
}

fn validate_bucket_records(
    records: &[ShuffleBucketRecordV1],
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    require(
        records.len() <= limits.max_chunk_items && records.len() <= MAX_SHUFFLE_LEAF_RECORDS,
        "shuffle leaf record cap",
    )?;
    let mut raw_ordinals = BTreeSet::new();
    let mut tribute_ids = BTreeSet::new();
    let mut nod_ids = BTreeSet::new();
    for record in records {
        record.validate(limits)?;
        require(
            raw_ordinals.insert(record.raw_ordinal)
                && tribute_ids.insert(record.tribute_id)
                && nod_ids.insert(record.nod_id),
            "unique bucket shuffle records",
        )?;
    }
    for pair in records.windows(2) {
        require(
            (pair[0].bucket_key, pair[0].raw_ordinal) < (pair[1].bucket_key, pair[1].raw_ordinal),
            "bucket shuffle records strictly ordered",
        )?;
    }
    Ok(())
}

fn canonical_page_split(span: &ShufflePageSpanV1) -> Result<u32, ProtocolError> {
    let width =
        span.end_page
            .checked_sub(span.start_page)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "shuffle page width",
            })?;
    require(width > 1, "shuffle node page width")?;
    let left_width = 1_u32 << (31 - (width - 1).leading_zeros());
    span.start_page
        .checked_add(left_width)
        .ok_or(ProtocolError::IntegerOverflow {
            what: "shuffle page split",
        })
}

fn checked_record_end(start: u32, count: u32) -> Result<u32, ProtocolError> {
    start
        .checked_add(count)
        .ok_or(ProtocolError::IntegerOverflow {
            what: "shuffle record interval",
        })
}

fn ordered_leaf_root(
    kind: ListKind,
    records: impl IntoIterator<Item = Result<Vec<u8>, ProtocolError>>,
    record_count: usize,
    limits: &SchemaLimits,
) -> Result<B256, ProtocolError> {
    let expected_count =
        u32::try_from(record_count).map_err(|_| ProtocolError::IntegerOverflow {
            what: "shuffle leaf record count",
        })?;
    let mut root = StreamingOrderedListRoot::new(kind, expected_count)?;
    for record in records {
        root.push(&record?, limits.max_bounded_bytes)?;
    }
    root.finish()
}
