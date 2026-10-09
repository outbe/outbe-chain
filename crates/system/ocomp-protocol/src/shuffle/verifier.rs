//! Validate canonical shuffle objects and authenticated child references.

use std::collections::BTreeSet;

use alloy_primitives::keccak256;

use crate::{
    control::CasObjectRefV1,
    error::ProtocolError,
    registry::ObjectKind,
    result::ContributorActionV1,
    schema::{require, NestedCodec, SchemaLimits},
    unit::CanonicalRunSpan,
};

use super::{
    ShuffleBucketRecordV1, ShufflePageSlice, ShufflePageSpanV1, ShuffleRunArtifactV1,
    ShuffleRunChildV1, ShuffleRunContextV1, ShuffleRunKindV1, ShuffleRunPayloadV1,
    MAX_SHUFFLE_LEAF_RECORDS,
};

pub(super) fn exact_shuffle_page_count(
    kind: ShuffleRunKindV1,
    record_count: u32,
) -> Result<u32, ProtocolError> {
    if record_count == 0 {
        return if kind == ShuffleRunKindV1::Owner {
            Ok(1)
        } else {
            Err(ProtocolError::InvalidInvariant(
                "non-empty bucket shuffle run",
            ))
        };
    }
    Ok(record_count.div_ceil(MAX_SHUFFLE_LEAF_RECORDS as u32))
}

pub(super) fn require_shuffle_page_leaf(
    context: &ShuffleRunContextV1,
    slice: &ShufflePageSlice<'_>,
) -> Result<(), ProtocolError> {
    let page_ordinal = slice.page_ordinal;
    let page_end = page_ordinal
        .checked_add(1)
        .ok_or(ProtocolError::IntegerOverflow {
            what: "shuffle requested page end",
        })?;
    let expected_first = page_ordinal
        .checked_mul(MAX_SHUFFLE_LEAF_RECORDS as u32)
        .ok_or(ProtocolError::IntegerOverflow {
            what: "shuffle requested first record",
        })?;
    let expected_end = if page_end == context.root_page_end {
        context.root_record_count
    } else {
        page_end
            .checked_mul(MAX_SHUFFLE_LEAF_RECORDS as u32)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "shuffle requested record end",
            })?
    };
    require(
        slice.page_span.start_page == page_ordinal
            && slice.page_span.end_page == page_end
            && slice.first_record_ordinal == expected_first
            && slice
                .first_record_ordinal
                .checked_add(slice.record_count)
                .is_some_and(|actual| actual == expected_end),
        "shuffle exact page slice",
    )
}

pub(super) fn resolve_shuffle_child<R>(
    context: &ShuffleRunContextV1,
    expected: &ShuffleRunChildV1,
    limits: &SchemaLimits,
    resolver: &mut R,
) -> Result<ShuffleRunArtifactV1, ProtocolError>
where
    R: FnMut(&CasObjectRefV1) -> Result<Vec<u8>, ProtocolError>,
{
    let bytes = resolver(&expected.artifact_ref)?;
    let encoded_bytes = u64::try_from(bytes.len()).map_err(|_| ProtocolError::IntegerOverflow {
        what: "shuffle child encoded bytes",
    })?;
    require(
        encoded_bytes == expected.artifact_ref.encoded_bytes
            && keccak256(&bytes) == expected.artifact_ref.transport_digest,
        "shuffle child transport descriptor",
    )?;
    let child = ShuffleRunArtifactV1::decode_canonical(&bytes, limits)?;
    require_shuffle_context(context, &child, limits)?;
    require(
        child.page_span == expected.page_span
            && child.first_record_ordinal == expected.first_record_ordinal
            && child.record_count == expected.record_count
            && child.ordered_record_root == expected.ordered_record_root,
        "shuffle child summary",
    )?;
    Ok(child)
}

pub(super) fn require_shuffle_context(
    context: &ShuffleRunContextV1,
    artifact: &ShuffleRunArtifactV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    artifact.validate_semantics(limits)?;
    let job_identity = artifact.protocol_bundle_hash == context.protocol_bundle_hash
        && artifact.job_id == context.job_id
        && artifact.attempt == context.attempt
        && artifact.unit_id == context.unit_id;
    let run_identity = artifact.kind == context.kind && artifact.run_span == context.run_span;
    let source_coverage = artifact.source_coverage_root == context.source_coverage_root
        && artifact.source_coverage_count == context.source_coverage_count;
    require(
        job_identity && run_identity && source_coverage,
        "shuffle descendant context",
    )
}

pub(super) fn require_valid_run_span(run_span: &CanonicalRunSpan) -> Result<(), ProtocolError> {
    require(
        run_span.start_run < run_span.end_run,
        "non-empty shuffle run span",
    )
}

pub(super) fn validate_child(
    child: &ShuffleRunChildV1,
    _limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
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

pub(super) fn validate_shuffle_run_artifact(
    artifact: &ShuffleRunArtifactV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    validate_artifact_header(artifact)?;
    validate_artifact_payload(artifact)?;
    require(
        artifact.ordered_record_root == artifact.recompute_ordered_record_root(limits)?,
        "shuffle ordered record root",
    )
}

fn validate_artifact_header(artifact: &ShuffleRunArtifactV1) -> Result<(), ProtocolError> {
    require(
        !artifact.protocol_bundle_hash.is_zero()
            && !artifact.job_id.is_zero()
            && !artifact.unit_id.is_zero(),
        "shuffle artifact identity",
    )?;
    require_valid_run_span(&artifact.run_span)?;
    require(
        artifact.source_coverage_count > 0 && !artifact.source_coverage_root.is_zero(),
        "shuffle source coverage",
    )?;
    require(
        !artifact.ordered_record_root.is_zero(),
        "shuffle ordered record root",
    )?;
    checked_record_end(artifact.first_record_ordinal, artifact.record_count)?;

    Ok(())
}

fn validate_artifact_payload(artifact: &ShuffleRunArtifactV1) -> Result<(), ProtocolError> {
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
            validate_node_shape(artifact, page_width, left, right)
        }
    }
}

fn validate_node_shape(
    artifact: &ShuffleRunArtifactV1,
    page_width: u32,
    left: &ShuffleRunChildV1,
    right: &ShuffleRunChildV1,
) -> Result<(), ProtocolError> {
    require(page_width > 1, "shuffle node page width")?;
    require(artifact.record_count > 0, "non-empty shuffle node")?;
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

pub(super) fn validate_owner_records(
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

pub(super) fn validate_bucket_records(
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
