use crate::ocomp::retention::*;

const JOURNAL_MAGIC: [u8; 8] = *b"OUTBPIN1";

const JOURNAL_VERSION: u16 = 6;

const PIN_RECORD_VERSION: u16 = 6;

const PIN_RECORD_MAX_BYTES: usize = 512;

/// The registry has no OCOMP product count limit. Its only cardinality ceiling
/// is the count width committed by the durable journal wire format.
pub(in crate::ocomp::retention) const JOURNAL_RECORD_COUNT_MAX: usize = u16::MAX as usize;

pub(in crate::ocomp::retention) const JOURNAL_MAX_BYTES: usize =
    (PIN_RECORD_MAX_BYTES + B256::len_bytes() + std::mem::size_of::<u16>())
        * JOURNAL_RECORD_COUNT_MAX
        + 8
        + std::mem::size_of::<u16>()
        + std::mem::size_of::<u64>()
        + B256::len_bytes()
        + std::mem::size_of::<u16>()
        + B256::len_bytes();

mod authority;
mod payload;
mod reader;
mod record;
mod registry;

use authority::{decode_export_authority, encode_export_authority, SourceAuthorityKind};
use reader::JournalReader;
use record::decode_record;

pub(in crate::ocomp::retention) use record::encode_record;
pub(in crate::ocomp::retention) use registry::{decode_registry, encode_registry};

fn append_checksum(mut encoded: Vec<u8>) -> Vec<u8> {
    let checksum = keccak256(&encoded);
    encoded.extend_from_slice(checksum.as_slice());
    encoded
}

fn checked_body<'a>(
    encoded: &'a [u8],
    minimum: usize,
    truncated: &'static str,
) -> Result<&'a [u8], RetentionError> {
    if encoded.len() < minimum {
        return Err(RetentionError::MalformedJournal(truncated));
    }
    let (body, checksum) = encoded.split_at(encoded.len() - 32);
    if keccak256(body).as_slice() != checksum {
        return Err(RetentionError::MalformedJournal("checksum mismatch"));
    }
    Ok(body)
}

fn read_version(reader: &mut JournalReader<'_>, expected: u16) -> Result<(), RetentionError> {
    if reader.take::<8>()? != JOURNAL_MAGIC {
        return Err(RetentionError::MalformedJournal("wrong magic"));
    }
    let actual = u16::from_be_bytes(reader.take::<2>()?);
    if actual != expected {
        return Err(RetentionError::UnsupportedJournalVersion { actual });
    }
    Ok(())
}

#[cfg(test)]
mod tests;

fn read_generation(
    reader: &mut JournalReader<'_>,
    zero_error: &'static str,
) -> Result<u64, RetentionError> {
    let generation = u64::from_be_bytes(reader.take::<8>()?);
    if generation == 0 {
        return Err(RetentionError::MalformedJournal(zero_error));
    }
    Ok(generation)
}
