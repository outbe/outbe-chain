use alloy_primitives::B256;
use outbe_ocomp_protocol::registry::HashDomain;
use outbe_ocomp_protocol::unit::UnitPhase;
use outbe_ocomp_protocol::{hash_framed, CanonicalReader, CanonicalWriter, SchemaLimits};

use super::{
    GratisPrefixDownOutputV1, GratisSummaryCoverageV1, LysisArtifactErrorV1,
    GRATIS_PREFIX_DOWN_MAGIC, GRATIS_SUMMARY_MAGIC,
};
use crate::program_v1::phases::{GratisIncomingV1, GratisLeafPrefixV1, GratisSegmentSummaryV1};

pub fn encode_gratis_segment_summary(
    summary: &GratisSegmentSummaryV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, LysisArtifactErrorV1> {
    validate_gratis_segment_summary(summary)?;
    let mut encoded = CanonicalWriter::new(limits.codec);
    encoded.write_fixed(&GRATIS_SUMMARY_MAGIC)?;
    encoded.write_u32(summary.start_ordinal)?;
    encoded.write_u32(summary.end_ordinal)?;
    encoded.write_u256(summary.checked_segment_gratis_total)?;
    Ok(encoded.into_bytes())
}

pub fn decode_gratis_segment_summary(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<GratisSegmentSummaryV1, LysisArtifactErrorV1> {
    let mut input = CanonicalReader::new(encoded, limits.codec)?;
    if input.read_fixed::<4>()? != GRATIS_SUMMARY_MAGIC {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Gratis summary header",
        ));
    }
    let summary = GratisSegmentSummaryV1 {
        start_ordinal: input.read_u32()?,
        end_ordinal: input.read_u32()?,
        checked_segment_gratis_total: input.read_u256()?,
    };
    input.finish()?;
    validate_gratis_segment_summary(&summary)?;
    if encode_gratis_segment_summary(&summary, limits)? != encoded {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Gratis summary canonical re-encoding",
        ));
    }
    Ok(summary)
}

pub fn encode_gratis_prefix_down_output(
    output: &GratisPrefixDownOutputV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, LysisArtifactErrorV1> {
    validate_gratis_prefix_down_output(output)?;
    let mut encoded = CanonicalWriter::new(limits.codec);
    encoded.write_fixed(&GRATIS_PREFIX_DOWN_MAGIC)?;
    match output {
        GratisPrefixDownOutputV1::Branch(children) => {
            encoded.write_u8(1)?;
            for child in children {
                encoded.write_option(child.as_ref(), |writer, incoming| {
                    writer.write_u32(incoming.start_ordinal)?;
                    writer.write_u32(incoming.end_ordinal)?;
                    writer
                        .write_option(incoming.incoming_remaining.as_ref(), |writer, remaining| {
                            writer.write_u256(*remaining)
                        })
                })?;
            }
        }
        GratisPrefixDownOutputV1::Leaf(prefix) => {
            encoded.write_u8(2)?;
            encoded.write_u32(prefix.segment_ordinal)?;
            encoded.write_u256(prefix.incoming_remaining)?;
            encoded.write_u256(prefix.outgoing_remaining)?;
            encoded.write_option(prefix.first_error_ordinal.as_ref(), |writer, ordinal| {
                writer.write_u32(*ordinal)
            })?;
        }
    }
    Ok(encoded.into_bytes())
}

pub fn decode_gratis_prefix_down_output(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<GratisPrefixDownOutputV1, LysisArtifactErrorV1> {
    let mut input = CanonicalReader::new(encoded, limits.codec)?;
    if input.read_fixed::<4>()? != GRATIS_PREFIX_DOWN_MAGIC {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Gratis prefix-down header",
        ));
    }
    let output = match input.read_u8()? {
        1 => {
            let mut child = || {
                input.read_option(|reader| {
                    Ok(GratisIncomingV1 {
                        start_ordinal: reader.read_u32()?,
                        end_ordinal: reader.read_u32()?,
                        incoming_remaining: reader.read_option(|reader| reader.read_u256())?,
                    })
                })
            };
            GratisPrefixDownOutputV1::Branch([child()?, child()?])
        }
        2 => GratisPrefixDownOutputV1::Leaf(GratisLeafPrefixV1 {
            segment_ordinal: input.read_u32()?,
            incoming_remaining: input.read_u256()?,
            outgoing_remaining: input.read_u256()?,
            first_error_ordinal: input.read_option(|reader| reader.read_u32())?,
        }),
        _ => {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "Gratis prefix-down variant",
            ))
        }
    };
    input.finish()?;
    validate_gratis_prefix_down_output(&output)?;
    if encode_gratis_prefix_down_output(&output, limits)? != encoded {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Gratis prefix-down canonical re-encoding",
        ));
    }
    Ok(output)
}

pub fn gratis_summary_coverage(
    prefix_interval_commitment: B256,
    children: [Option<(B256, u32)>; 2],
) -> Result<GratisSummaryCoverageV1, LysisArtifactErrorV1> {
    if prefix_interval_commitment.is_zero()
        || children[0].is_none()
        || children
            .iter()
            .flatten()
            .any(|(root, count)| root.is_zero() || *count == 0)
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Gratis summary coverage inputs",
        ));
    }
    let count = children
        .iter()
        .flatten()
        .try_fold(0_u32, |total, (_, count)| total.checked_add(*count))
        .ok_or(LysisArtifactErrorV1::LengthOverflow)?;
    let mut payload = Vec::with_capacity(1 + 32 + (32 * 2) + (4 * 2));
    payload.push(UnitPhase::GratisPrefix as u8);
    payload.extend_from_slice(prefix_interval_commitment.as_slice());
    for child in children {
        let (root, _) = child.unwrap_or((B256::ZERO, 0));
        payload.extend_from_slice(root.as_slice());
    }
    for child in children {
        let (_, count) = child.unwrap_or((B256::ZERO, 0));
        payload.extend_from_slice(&count.to_be_bytes());
    }
    Ok(GratisSummaryCoverageV1 {
        root: hash_framed(HashDomain::UnitCoverage, &payload)?,
        count,
    })
}

fn validate_gratis_segment_summary(
    summary: &GratisSegmentSummaryV1,
) -> Result<(), LysisArtifactErrorV1> {
    if summary.start_ordinal >= summary.end_ordinal
        || summary.checked_segment_gratis_total.is_zero()
    {
        return Err(LysisArtifactErrorV1::InvalidEncoding(
            "Gratis summary fields",
        ));
    }
    Ok(())
}

fn validate_gratis_prefix_down_output(
    output: &GratisPrefixDownOutputV1,
) -> Result<(), LysisArtifactErrorV1> {
    match output {
        GratisPrefixDownOutputV1::Branch([Some(left), right]) => {
            if left.start_ordinal >= left.end_ordinal
                || right.as_ref().is_some_and(|right| {
                    right.start_ordinal >= right.end_ordinal
                        || left.end_ordinal != right.start_ordinal
                })
            {
                return Err(LysisArtifactErrorV1::InvalidEncoding(
                    "Gratis prefix-down branch",
                ));
            }
        }
        GratisPrefixDownOutputV1::Branch([None, _]) => {
            return Err(LysisArtifactErrorV1::InvalidEncoding(
                "Gratis prefix-down empty left branch",
            ));
        }
        GratisPrefixDownOutputV1::Leaf(prefix) => {
            if prefix.incoming_remaining.is_zero()
                || prefix.incoming_remaining <= prefix.outgoing_remaining
                || prefix.first_error_ordinal.is_some()
            {
                return Err(LysisArtifactErrorV1::InvalidEncoding(
                    "Gratis prefix-down leaf",
                ));
            }
        }
    }
    Ok(())
}
