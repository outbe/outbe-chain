use super::*;

#[derive(Default)]
pub(super) struct ManifestSemanticTotals {
    pub tribute_count: u32,
    pub tribute_nominal_total: U256,
    pub fidelity_count: u32,
    pub oracle_count: u32,
}

pub(super) fn observe_manifest_chunk(
    totals: &mut ManifestSemanticTotals,
    verified: &crate::input_ref_catalog::VerifiedInputChunkRefV1,
    verification: InputManifestVerification<'_>,
    on_progress: &impl Fn(),
) -> Result<(), InputArtifactError> {
    if verified.reference.kind != InputChunkKind::Tribute
        && verified.chunk.canonical_records_or_openings.len() != 1
    {
        return Err(InputArtifactError::Invariant(
            "one opening record per input chunk",
        ));
    }
    for record in &verified.chunk.canonical_records_or_openings {
        observe_manifest_record(totals, verified.reference.kind, &record.0, verification)?;
        on_progress();
    }
    Ok(())
}

fn observe_manifest_record(
    totals: &mut ManifestSemanticTotals,
    kind: InputChunkKind,
    record: &[u8],
    verification: InputManifestVerification<'_>,
) -> Result<(), InputArtifactError> {
    let InputManifestVerification {
        bundle,
        manifest,
        limits,
        ..
    } = verification;
    match kind {
        InputChunkKind::Tribute => {
            let tribute = outbe_tribute::record::decode_canonical(record)?.calculation_view()?;
            require(
                tribute.worldwide_day.value() == manifest.wwd,
                "Tribute WWD matches manifest",
            )?;
            totals.tribute_count = totals
                .tribute_count
                .checked_add(1)
                .ok_or(InputArtifactError::CountOverflow)?;
            totals.tribute_nominal_total = totals
                .tribute_nominal_total
                .checked_add(tribute.nominal_amount_minor)
                .ok_or(InputArtifactError::NominalTotalOverflow)?;
        }
        InputChunkKind::Fidelity | InputChunkKind::Oracle => {
            let opening = AuthenticatedOpeningV1::decode_canonical_record(record, limits)?;
            opening.validate_against_bundle(bundle, limits)?;
            let _ = opening.decode_and_validate_raw_opening(
                manifest.checkpoint.finalized_state_root,
                limits,
            )?;
            match kind {
                InputChunkKind::Fidelity => {
                    require(
                        opening.source_kind == OpeningSourceKind::Fidelity,
                        "Fidelity opening source",
                    )?;
                    totals.fidelity_count = totals
                        .fidelity_count
                        .checked_add(1)
                        .ok_or(InputArtifactError::CountOverflow)?;
                }
                InputChunkKind::Oracle => {
                    require(
                        opening.source_kind == OpeningSourceKind::Oracle,
                        "Oracle opening source",
                    )?;
                    totals.oracle_count = totals
                        .oracle_count
                        .checked_add(1)
                        .ok_or(InputArtifactError::CountOverflow)?;
                }
                InputChunkKind::Tribute => {
                    return Err(InputArtifactError::Invariant("opening chunk kind"));
                }
            }
        }
    }
    Ok(())
}
