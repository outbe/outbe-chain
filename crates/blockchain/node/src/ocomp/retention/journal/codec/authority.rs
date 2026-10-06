use super::*;

pub(super) fn encode_export_authority(encoded: &mut Vec<u8>, export: ExportAuthorityV1) {
    encoded.extend_from_slice(&export.source_generation.to_be_bytes());
    encoded.extend_from_slice(&export.lease_generation.to_be_bytes());
    encoded.extend_from_slice(export.manifest_hash.as_slice());
}

pub(super) fn decode_export_authority(
    reader: &mut JournalReader<'_>,
) -> Result<ExportAuthorityV1, RetentionError> {
    let export = ExportAuthorityV1 {
        source_generation: u64::from_be_bytes(reader.take::<8>()?),
        lease_generation: u64::from_be_bytes(reader.take::<8>()?),
        manifest_hash: B256::new(reader.take::<32>()?),
    };
    if export.source_generation == 0
        || export.lease_generation == 0
        || export.manifest_hash.is_zero()
    {
        return Err(RetentionError::MalformedJournal(
            "incomplete export authority",
        ));
    }
    Ok(export)
}

#[derive(Clone, Copy)]
pub(super) enum SourceAuthorityKind {
    Terminal,
    GcPending,
    Released,
}

impl SourceAuthorityKind {
    fn zero_generation(self) -> &'static str {
        match self {
            Self::Terminal => "zero terminal source generation",
            Self::GcPending => "zero GC source generation",
            Self::Released => "zero released source generation",
        }
    }
    fn invalid_flag(self) -> &'static str {
        match self {
            Self::Terminal => "invalid terminal export-authority flag",
            Self::GcPending => "invalid GC export-authority flag",
            Self::Released => "invalid export-authority flag",
        }
    }
    fn conflicting_generation(self) -> &'static str {
        match self {
            Self::Terminal => "terminal export authority has a conflicting source generation",
            Self::GcPending => "GC export authority has a conflicting source generation",
            Self::Released => "released record carries inconsistent authority",
        }
    }
    pub(super) fn read(
        self,
        reader: &mut JournalReader<'_>,
    ) -> Result<(u64, Option<ExportAuthorityV1>), RetentionError> {
        let source_generation = u64::from_be_bytes(reader.take::<8>()?);
        if source_generation == 0 {
            return Err(RetentionError::MalformedJournal(self.zero_generation()));
        }
        let export = match reader.take::<1>()?[0] {
            0 => None,
            1 => Some(decode_export_authority(reader)?),
            _ => return Err(RetentionError::MalformedJournal(self.invalid_flag())),
        };
        if export.is_some_and(|authority| authority.source_generation != source_generation) {
            return Err(RetentionError::MalformedJournal(
                self.conflicting_generation(),
            ));
        }
        Ok((source_generation, export))
    }
}

pub(super) fn encode_optional_export(encoded: &mut Vec<u8>, export: Option<ExportAuthorityV1>) {
    match export {
        Some(export) => {
            encoded.push(1);
            encode_export_authority(encoded, export);
        }
        None => encoded.push(0),
    }
}

#[derive(Clone, Copy)]
pub(super) enum TerminalKind {
    Terminal,
    GcPending,
}
impl TerminalKind {
    pub(super) fn authority_kind(self) -> SourceAuthorityKind {
        match self {
            Self::Terminal => SourceAuthorityKind::Terminal,
            Self::GcPending => SourceAuthorityKind::GcPending,
        }
    }
    pub(super) fn release_height_error(self) -> &'static str {
        match self {
            Self::Terminal => "release height is not terminal finality plus evidence window",
            Self::GcPending => "GC release height is not terminal finality plus evidence window",
        }
    }
}
