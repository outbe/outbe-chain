use super::*;

pub(super) struct FinalizedFields {
    job_id: B256,
    finality_recorded_height: u64,
    open_height: u64,
    deadline_height: u64,
}
impl FinalizedFields {
    pub(super) fn read(reader: &mut JournalReader<'_>) -> Result<Self, RetentionError> {
        let job_id = B256::new(reader.take::<32>()?);
        let (finality_recorded_height, open_height, deadline_height) =
            decode_finalized_window(reader)?;
        Ok(Self {
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
        })
    }
    pub(super) fn finalized(self, candidate: CandidatePinV1) -> PinStateV1 {
        let Self {
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
        } = self;
        PinStateV1::Finalized {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
        }
    }
    pub(super) fn exported(
        self,
        candidate: CandidatePinV1,
        export: ExportAuthorityV1,
    ) -> PinStateV1 {
        let Self {
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
        } = self;
        PinStateV1::Exported {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            export,
        }
    }
}

pub(super) fn decode_released(
    reader: &mut JournalReader<'_>,
    candidate: CandidatePinV1,
) -> Result<PinStateV1, RetentionError> {
    let job_id = B256::new(reader.take::<32>()?);
    let (source_generation, export) = SourceAuthorityKind::Released.read(reader)?;
    Ok(PinStateV1::Released {
        candidate,
        job_id,
        source_generation,
        observed_height: u64::from_be_bytes(reader.take::<8>()?),
        export,
    })
}

pub(super) fn decode_terminal(
    reader: &mut JournalReader<'_>,
    candidate: CandidatePinV1,
    kind: authority::TerminalKind,
) -> Result<PinStateV1, RetentionError> {
    let FinalizedFields {
        job_id,
        finality_recorded_height,
        open_height,
        deadline_height,
    } = FinalizedFields::read(reader)?;
    let (source_generation, export) = kind.authority_kind().read(reader)?;
    let terminal_height = u64::from_be_bytes(reader.take::<8>()?);
    let release_height = u64::from_be_bytes(reader.take::<8>()?);
    if terminal_height.checked_add(RETAINED_EVIDENCE_WINDOW_BLOCKS) != Some(release_height) {
        return Err(RetentionError::MalformedJournal(
            kind.release_height_error(),
        ));
    }
    Ok(match kind {
        authority::TerminalKind::Terminal => PinStateV1::Terminal {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            source_generation,
            export,
            terminal_height,
            release_height,
        },
        authority::TerminalKind::GcPending => PinStateV1::GcPending {
            candidate,
            job_id,
            finality_recorded_height,
            open_height,
            deadline_height,
            source_generation,
            export,
            terminal_height,
            release_height,
        },
    })
}

fn decode_finalized_window(
    reader: &mut JournalReader<'_>,
) -> Result<(u64, u64, u64), RetentionError> {
    let finality_recorded_height = u64::from_be_bytes(reader.take::<8>()?);
    let open_height = u64::from_be_bytes(reader.take::<8>()?);
    let deadline_height = u64::from_be_bytes(reader.take::<8>()?);
    if finality_recorded_height
        .checked_add(outbe_ocomp_protocol::state::RESULT_VOTE_MIN_FINALITY_DEPTH)
        != Some(open_height)
        || open_height >= deadline_height
    {
        return Err(RetentionError::MalformedJournal(
            "invalid finalized response window",
        ));
    }
    Ok((finality_recorded_height, open_height, deadline_height))
}
