//! Bounded, Lysis-specific owner/bucket shuffle run artifacts.
//!
//! This closed schema is deliberately not a generic DAG or spill-file
//! framework. A `UnitArtifactV1` embeds one root object. Bounded descendants
//! are addressed by their verified CAS references.

use std::collections::VecDeque;

use alloy_primitives::{Address, B256};

use crate::{
    codec::{CanonicalReader, CanonicalWriter},
    control::CasObjectRefV1,
    error::ProtocolError,
    hash::hash_framed,
    list::StreamingOrderedListRoot,
    registry::{HashDomain, ListKind},
    result::ContributorActionV1,
    schema::{
        encode_nested_value, impl_nested_record_codec, impl_top_level_codec, require, wire_enum_u8,
        wire_struct, NestedCodec, SchemaLimits,
    },
    unit::CanonicalRunSpan,
};

/// One shuffle leaf never owns more records than one primary Lysis work shard.
pub const MAX_SHUFFLE_LEAF_RECORDS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShuffleSourceCoverageV1 {
    pub run_span: CanonicalRunSpan,
    pub root: B256,
    pub count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShuffleRunBuildContextV1 {
    pub protocol_bundle_hash: B256,
    pub job_id: B256,
    pub attempt: u32,
    pub unit_id: B256,
    pub run_span: CanonicalRunSpan,
    pub source_coverage_root: B256,
    pub source_coverage_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerifiedShuffleRecordV1 {
    Owner(ContributorActionV1),
    Bucket(ShuffleBucketRecordV1),
}

pub struct VerifiedShuffleRunIterV1<'a, R>
where
    R: FnMut(&CasObjectRefV1) -> Result<Vec<u8>, ProtocolError>,
{
    limits: &'a SchemaLimits,
    resolver: R,
    context: ShuffleRunContextV1,
    stack: Vec<ShuffleRunArtifactV1>,
    pending_records: VecDeque<VerifiedShuffleRecordV1>,
    next_page: u32,
    leaf_count: u32,
    previous_leaf_record_count: Option<u32>,
    yielded_record_count: u32,
    previous_owner: Option<(Address, B256)>,
    previous_bucket: Option<(B256, u32)>,
    finished: bool,
}

pub struct MergedVerifiedShuffleRunIterV1<L, R>
where
    L: Iterator<Item = Result<VerifiedShuffleRecordV1, ProtocolError>>,
    R: Iterator<Item = Result<VerifiedShuffleRecordV1, ProtocolError>>,
{
    kind: ShuffleRunKindV1,
    left: std::iter::Peekable<L>,
    right: std::iter::Peekable<R>,
    previous_key: Option<VerifiedShuffleRecordKeyV1>,
    finished: bool,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum VerifiedShuffleRecordKeyV1 {
    Owner(Address, B256),
    Bucket(B256, u32),
}

#[derive(Clone, Debug)]
struct ShuffleRunContextV1 {
    protocol_bundle_hash: B256,
    job_id: B256,
    attempt: u32,
    unit_id: B256,
    kind: ShuffleRunKindV1,
    run_span: CanonicalRunSpan,
    root_page_end: u32,
    root_record_count: u32,
    source_coverage_root: B256,
    source_coverage_count: u32,
}

pub fn verified_shuffle_run_records<'a, R>(
    root: ShuffleRunArtifactV1,
    limits: &'a SchemaLimits,
    resolver: R,
) -> Result<VerifiedShuffleRunIterV1<'a, R>, ProtocolError>
where
    R: FnMut(&CasObjectRefV1) -> Result<Vec<u8>, ProtocolError>,
{
    root.validate_root_semantics(limits)?;
    let context = ShuffleRunContextV1 {
        protocol_bundle_hash: root.protocol_bundle_hash,
        job_id: root.job_id,
        attempt: root.attempt,
        unit_id: root.unit_id,
        kind: root.kind,
        run_span: root.run_span.clone(),
        root_page_end: root.page_span.end_page,
        root_record_count: root.record_count,
        source_coverage_root: root.source_coverage_root,
        source_coverage_count: root.source_coverage_count,
    };
    Ok(VerifiedShuffleRunIterV1 {
        limits,
        resolver,
        context,
        stack: vec![root],
        pending_records: VecDeque::new(),
        next_page: 0,
        leaf_count: 0,
        previous_leaf_record_count: None,
        yielded_record_count: 0,
        previous_owner: None,
        previous_bucket: None,
        finished: false,
    })
}

enum ShufflePageRecords {
    Owner(Vec<ContributorActionV1>),
    Bucket(Vec<ShuffleBucketRecordV1>),
}

impl ShufflePageRecords {
    fn into_verified(self) -> Vec<VerifiedShuffleRecordV1> {
        match self {
            Self::Owner(records) => records
                .into_iter()
                .map(VerifiedShuffleRecordV1::Owner)
                .collect(),
            Self::Bucket(records) => records
                .into_iter()
                .map(VerifiedShuffleRecordV1::Bucket)
                .collect(),
        }
    }
}

struct ShufflePageSlice<'a> {
    page_span: &'a ShufflePageSpanV1,
    first_record_ordinal: u32,
    record_count: u32,
    page_ordinal: u32,
}

fn selected_shuffle_child(
    left: ShuffleRunChildV1,
    right: ShuffleRunChildV1,
    page_ordinal: u32,
) -> Result<ShuffleRunChildV1, ProtocolError> {
    if page_ordinal >= left.page_span.start_page && page_ordinal < left.page_span.end_page {
        Ok(left)
    } else if page_ordinal >= right.page_span.start_page && page_ordinal < right.page_span.end_page
    {
        Ok(right)
    } else {
        Err(ProtocolError::InvalidInvariant(
            "shuffle page child coverage",
        ))
    }
}

/// Opens one canonical 256-record page from an already admitted shuffle root.
///
/// The lookup follows only the unique authenticated child path for
/// `page_ordinal`. It never scans the preceding population. A request beyond
/// the exact record stream returns the canonical empty slice.
pub fn verified_shuffle_run_page<R>(
    root: ShuffleRunArtifactV1,
    page_ordinal: u32,
    limits: &SchemaLimits,
    mut resolver: R,
) -> Result<Vec<VerifiedShuffleRecordV1>, ProtocolError>
where
    R: FnMut(&CasObjectRefV1) -> Result<Vec<u8>, ProtocolError>,
{
    root.validate_root_semantics(limits)?;
    let root_page_end = exact_shuffle_page_count(root.kind, root.record_count)?;
    require(
        root.page_span.end_page == root_page_end,
        "shuffle root exact page count",
    )?;
    if page_ordinal >= root_page_end {
        return Ok(Vec::new());
    }
    let context = ShuffleRunContextV1 {
        protocol_bundle_hash: root.protocol_bundle_hash,
        job_id: root.job_id,
        attempt: root.attempt,
        unit_id: root.unit_id,
        kind: root.kind,
        run_span: root.run_span.clone(),
        root_page_end,
        root_record_count: root.record_count,
        source_coverage_root: root.source_coverage_root,
        source_coverage_count: root.source_coverage_count,
    };
    let mut artifact = root;
    loop {
        require_shuffle_context(&context, &artifact, limits)?;
        let records = match artifact.payload {
            ShuffleRunPayloadV1::Node { left, right } => {
                let expected = selected_shuffle_child(left, right, page_ordinal)?;
                artifact = resolve_shuffle_child(&context, &expected, limits, &mut resolver)?;
                continue;
            }
            ShuffleRunPayloadV1::OwnerLeaf(records) => {
                require(
                    context.kind == ShuffleRunKindV1::Owner,
                    "shuffle page owner kind",
                )?;
                ShufflePageRecords::Owner(records)
            }
            ShuffleRunPayloadV1::BucketLeaf(records) => {
                require(
                    context.kind == ShuffleRunKindV1::Bucket,
                    "shuffle page bucket kind",
                )?;
                ShufflePageRecords::Bucket(records)
            }
        };
        require_shuffle_page_leaf(
            &context,
            &ShufflePageSlice {
                page_span: &artifact.page_span,
                first_record_ordinal: artifact.first_record_ordinal,
                record_count: artifact.record_count,
                page_ordinal,
            },
        )?;
        return Ok(records.into_verified());
    }
}

pub fn merge_verified_shuffle_runs<L, R>(
    kind: ShuffleRunKindV1,
    left: L,
    right: R,
) -> MergedVerifiedShuffleRunIterV1<L::IntoIter, R::IntoIter>
where
    L: IntoIterator<Item = Result<VerifiedShuffleRecordV1, ProtocolError>>,
    R: IntoIterator<Item = Result<VerifiedShuffleRecordV1, ProtocolError>>,
{
    MergedVerifiedShuffleRunIterV1 {
        kind,
        left: left.into_iter().peekable(),
        right: right.into_iter().peekable(),
        previous_key: None,
        finished: false,
    }
}

mod builder;
mod verifier;
pub use builder::{build_bucket_shuffle_run, build_owner_shuffle_run};
use verifier::{
    exact_shuffle_page_count, require_shuffle_context, require_shuffle_page_leaf,
    require_valid_run_span, resolve_shuffle_child, validate_bucket_records, validate_child,
    validate_owner_records, validate_shuffle_run_artifact,
};

impl<R> Iterator for VerifiedShuffleRunIterV1<'_, R>
where
    R: FnMut(&CasObjectRefV1) -> Result<Vec<u8>, ProtocolError>,
{
    type Item = Result<VerifiedShuffleRecordV1, ProtocolError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(record) = self.pending_records.pop_front() {
                return Some(self.validate_next_record(record));
            }
            let Some(artifact) = self.stack.pop() else {
                if self.finished {
                    return None;
                }
                self.finished = true;
                return self.finish().err().map(Err);
            };
            if let Err(error) = self.open_artifact(artifact) {
                self.finished = true;
                return Some(Err(error));
            }
        }
    }
}

impl<L, R> Iterator for MergedVerifiedShuffleRunIterV1<L, R>
where
    L: Iterator<Item = Result<VerifiedShuffleRecordV1, ProtocolError>>,
    R: Iterator<Item = Result<VerifiedShuffleRecordV1, ProtocolError>>,
{
    type Item = Result<VerifiedShuffleRecordV1, ProtocolError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let side = match self.select_side() {
            Ok(Some(side)) => side,
            Ok(None) => return None,
            Err(error) => {
                self.finished = true;
                return Some(Err(error));
            }
        };
        let item = match side {
            MergeSideV1::Left => self.left.next(),
            MergeSideV1::Right => self.right.next(),
        }?;
        let result = item.and_then(|record| self.accept_record(record));
        if result.is_err() {
            self.finished = true;
        }
        Some(result)
    }
}

impl<L, R> MergedVerifiedShuffleRunIterV1<L, R>
where
    L: Iterator<Item = Result<VerifiedShuffleRecordV1, ProtocolError>>,
    R: Iterator<Item = Result<VerifiedShuffleRecordV1, ProtocolError>>,
{
    fn select_side(&mut self) -> Result<Option<MergeSideV1>, ProtocolError> {
        let side = match (self.left.peek(), self.right.peek()) {
            (Some(Err(_)), _) => MergeSideV1::Left,
            (_, Some(Err(_))) => MergeSideV1::Right,
            (Some(Ok(left)), Some(Ok(right))) => {
                let left_key = shuffle_record_key(self.kind, left)?;
                let right_key = shuffle_record_key(self.kind, right)?;
                if left_key <= right_key {
                    MergeSideV1::Left
                } else {
                    MergeSideV1::Right
                }
            }
            (Some(Ok(_)), None) => MergeSideV1::Left,
            (None, Some(Ok(_))) => MergeSideV1::Right,
            (None, None) => return Ok(None),
        };
        Ok(Some(side))
    }

    fn accept_record(
        &mut self,
        record: VerifiedShuffleRecordV1,
    ) -> Result<VerifiedShuffleRecordV1, ProtocolError> {
        let key = shuffle_record_key(self.kind, &record)?;
        if self
            .previous_key
            .is_some_and(|previous| !strict_shuffle_key_order(previous, key))
        {
            return Err(ProtocolError::InvalidInvariant(
                "strict merged shuffle order",
            ));
        }
        self.previous_key = Some(key);
        Ok(record)
    }
}

#[derive(Clone, Copy)]
enum MergeSideV1 {
    Left,
    Right,
}

fn shuffle_record_key(
    kind: ShuffleRunKindV1,
    record: &VerifiedShuffleRecordV1,
) -> Result<VerifiedShuffleRecordKeyV1, ProtocolError> {
    match (kind, record) {
        (ShuffleRunKindV1::Owner, VerifiedShuffleRecordV1::Owner(record)) => Ok(
            VerifiedShuffleRecordKeyV1::Owner(record.owner, record.source_tribute_id),
        ),
        (ShuffleRunKindV1::Bucket, VerifiedShuffleRecordV1::Bucket(record)) => Ok(
            VerifiedShuffleRecordKeyV1::Bucket(record.bucket_key, record.raw_ordinal),
        ),
        _ => Err(ProtocolError::InvalidInvariant(
            "merged shuffle record kind",
        )),
    }
}

fn strict_shuffle_key_order(
    previous: VerifiedShuffleRecordKeyV1,
    current: VerifiedShuffleRecordKeyV1,
) -> bool {
    match (previous, current) {
        (
            VerifiedShuffleRecordKeyV1::Owner(previous_owner, _),
            VerifiedShuffleRecordKeyV1::Owner(current_owner, _),
        ) => previous_owner < current_owner,
        (
            VerifiedShuffleRecordKeyV1::Bucket(previous_bucket, previous_ordinal),
            VerifiedShuffleRecordKeyV1::Bucket(current_bucket, current_ordinal),
        ) => (previous_bucket, previous_ordinal) < (current_bucket, current_ordinal),
        _ => false,
    }
}

impl<R> VerifiedShuffleRunIterV1<'_, R>
where
    R: FnMut(&CasObjectRefV1) -> Result<Vec<u8>, ProtocolError>,
{
    fn open_artifact(&mut self, artifact: ShuffleRunArtifactV1) -> Result<(), ProtocolError> {
        self.require_context(&artifact)?;
        match artifact.payload {
            ShuffleRunPayloadV1::OwnerLeaf(records) => {
                self.open_leaf(
                    artifact.page_span,
                    artifact.first_record_ordinal,
                    artifact.record_count,
                )?;
                self.pending_records
                    .extend(records.into_iter().map(VerifiedShuffleRecordV1::Owner));
            }
            ShuffleRunPayloadV1::BucketLeaf(records) => {
                self.open_leaf(
                    artifact.page_span,
                    artifact.first_record_ordinal,
                    artifact.record_count,
                )?;
                self.pending_records
                    .extend(records.into_iter().map(VerifiedShuffleRecordV1::Bucket));
            }
            ShuffleRunPayloadV1::Node { left, right } => {
                let right = self.resolve_child(&right)?;
                let left = self.resolve_child(&left)?;
                self.stack.push(right);
                self.stack.push(left);
            }
        }
        Ok(())
    }

    fn resolve_child(
        &mut self,
        expected: &ShuffleRunChildV1,
    ) -> Result<ShuffleRunArtifactV1, ProtocolError> {
        resolve_shuffle_child(&self.context, expected, self.limits, &mut self.resolver)
    }

    fn require_context(&self, artifact: &ShuffleRunArtifactV1) -> Result<(), ProtocolError> {
        require_shuffle_context(&self.context, artifact, self.limits)
    }

    fn open_leaf(
        &mut self,
        page_span: ShufflePageSpanV1,
        first_record_ordinal: u32,
        record_count: u32,
    ) -> Result<(), ProtocolError> {
        if let Some(previous) = self.previous_leaf_record_count {
            require(
                usize::try_from(previous).ok() == Some(MAX_SHUFFLE_LEAF_RECORDS),
                "full non-final shuffle leaf",
            )?;
        }
        require(
            page_span.start_page == self.next_page
                && page_span.end_page
                    == self
                        .next_page
                        .checked_add(1)
                        .ok_or(ProtocolError::IntegerOverflow {
                            what: "shuffle traversal page",
                        })?
                && first_record_ordinal == self.yielded_record_count,
            "shuffle traversal leaf adjacency",
        )?;
        self.next_page = page_span.end_page;
        self.leaf_count = self
            .leaf_count
            .checked_add(1)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "shuffle traversal leaf count",
            })?;
        self.previous_leaf_record_count = Some(record_count);
        Ok(())
    }

    fn validate_next_record(
        &mut self,
        record: VerifiedShuffleRecordV1,
    ) -> Result<VerifiedShuffleRecordV1, ProtocolError> {
        match &record {
            VerifiedShuffleRecordV1::Owner(record) => {
                require(
                    self.context.kind == ShuffleRunKindV1::Owner,
                    "shuffle traversal record kind",
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
                    self.context.kind == ShuffleRunKindV1::Bucket,
                    "shuffle traversal record kind",
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
        self.yielded_record_count =
            self.yielded_record_count
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "shuffle traversal record count",
                })?;
        Ok(record)
    }

    fn finish(&self) -> Result<(), ProtocolError> {
        require(
            self.leaf_count == self.context.root_page_end
                && self.next_page == self.context.root_page_end
                && self.yielded_record_count == self.context.root_record_count,
            "shuffle traversal exact counts",
        )?;
        if self.context.root_record_count == 0 {
            require(
                self.context.kind == ShuffleRunKindV1::Owner
                    && self.leaf_count == 1
                    && self.previous_leaf_record_count == Some(0),
                "canonical empty owner shuffle run",
            )
        } else {
            require(
                self.previous_leaf_record_count.is_some_and(|count| {
                    count > 0
                        && usize::try_from(count)
                            .is_ok_and(|count| count <= MAX_SHUFFLE_LEAF_RECORDS)
                }),
                "non-empty final shuffle leaf",
            )
        }
    }
}

impl ShuffleSourceCoverageV1 {
    pub fn leaf(
        run_span: CanonicalRunSpan,
        raw_coverage_root: B256,
        raw_coverage_count: u32,
        limits: &SchemaLimits,
    ) -> Result<Self, ProtocolError> {
        require_valid_run_span(&run_span)?;
        require(
            !raw_coverage_root.is_zero() && raw_coverage_count > 0,
            "shuffle leaf source coverage",
        )?;
        let mut payload = CanonicalWriter::new(limits.codec);
        payload.write_u8(1)?;
        payload.write_u32(run_span.start_run)?;
        payload.write_u32(run_span.end_run)?;
        payload.write_b256(raw_coverage_root)?;
        payload.write_u32(raw_coverage_count)?;
        Ok(Self {
            run_span,
            root: hash_framed(HashDomain::ShuffleSourceCoverage, payload.as_slice())?,
            count: raw_coverage_count,
        })
    }

    pub fn merge(left: &Self, right: &Self, limits: &SchemaLimits) -> Result<Self, ProtocolError> {
        require_valid_run_span(&left.run_span)?;
        require_valid_run_span(&right.run_span)?;
        require(
            left.run_span.end_run == right.run_span.start_run,
            "adjacent shuffle source coverage",
        )?;
        require(
            !left.root.is_zero() && !right.root.is_zero() && left.count > 0 && right.count > 0,
            "shuffle producer source coverage",
        )?;
        let count = left
            .count
            .checked_add(right.count)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "shuffle source coverage count",
            })?;
        let run_span = CanonicalRunSpan {
            start_run: left.run_span.start_run,
            end_run: right.run_span.end_run,
        };
        let mut payload = CanonicalWriter::new(limits.codec);
        payload.write_u8(2)?;
        payload.write_u32(run_span.start_run)?;
        payload.write_u32(run_span.end_run)?;
        payload.write_b256(left.root)?;
        payload.write_u32(left.count)?;
        payload.write_b256(right.root)?;
        payload.write_u32(right.count)?;
        Ok(Self {
            run_span,
            root: hash_framed(HashDomain::ShuffleSourceCoverage, payload.as_slice())?,
            count,
        })
    }
}

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
        pub tribute_id: B256,
        pub nod_id: B256,
    }
}

impl_nested_record_codec!(ShuffleBucketRecordV1, encode_only);

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
