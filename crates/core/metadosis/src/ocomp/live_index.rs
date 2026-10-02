use super::codec::FixedReader;
use crate::errors::storage_corruption_message;
use alloy_primitives::B256;
use outbe_primitives::{error::Result, time::WorldwideDay};
use std::collections::BTreeSet;

const MAGIC: [u8; 4] = *b"OMLI";
const VERSION: u16 = 2;
pub(super) const LIVE_INDEX_HEADER_LEN: usize = 8;
pub(super) const LIVE_INDEX_KEY_LEN: usize = 36;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct LiveIndexKey {
    pub worldwide_day: WorldwideDay,
    pub intent_id: B256,
}

pub(super) fn encode_live_scheduler_index(index: &[LiveIndexKey]) -> Result<Vec<u8>> {
    validate(index)?;
    if index.is_empty() {
        return Ok(Vec::new());
    }
    let count = u16::try_from(index.len())
        .map_err(|_| storage_corruption_message("OCOMP live index count exceeds u16"))?;
    let mut encoded = Vec::with_capacity(LIVE_INDEX_HEADER_LEN + index.len() * LIVE_INDEX_KEY_LEN);
    encoded.extend_from_slice(&MAGIC);
    encoded.extend_from_slice(&VERSION.to_be_bytes());
    encoded.extend_from_slice(&count.to_be_bytes());
    for key in index {
        encoded.extend_from_slice(&key.worldwide_day.value().to_be_bytes());
        encoded.extend_from_slice(key.intent_id.as_slice());
    }
    Ok(encoded)
}

pub(super) fn decode_live_scheduler_index(encoded: &[u8]) -> Result<Vec<LiveIndexKey>> {
    if encoded.is_empty() {
        return Ok(Vec::new());
    }
    let mut reader = FixedReader::new(encoded);
    if reader.take::<4>()? != MAGIC || u16::from_be_bytes(reader.take::<2>()?) != VERSION {
        return Err(storage_corruption_message(
            "OCOMP live index magic/version mismatch",
        ));
    }
    let count = usize::from(u16::from_be_bytes(reader.take::<2>()?));
    if count == 0 {
        return Err(storage_corruption_message(
            "OCOMP live index must use empty bytes for zero jobs",
        ));
    }
    if encoded.len() != LIVE_INDEX_HEADER_LEN + count * LIVE_INDEX_KEY_LEN {
        return Err(storage_corruption_message(
            "OCOMP live index has non-canonical length",
        ));
    }
    let mut index = Vec::with_capacity(count);
    for _ in 0..count {
        index.push(LiveIndexKey {
            worldwide_day: WorldwideDay::new(reader.u32()?),
            intent_id: B256::from(reader.take::<32>()?),
        });
    }
    reader.finish()?;
    validate(&index)?;
    Ok(index)
}

fn validate(index: &[LiveIndexKey]) -> Result<()> {
    if index.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(storage_corruption_message(
            "OCOMP live index is not in strict canonical order",
        ));
    }
    let mut days = BTreeSet::new();
    let mut intents = BTreeSet::new();
    for key in index {
        if key.intent_id.is_zero() {
            return Err(storage_corruption_message(
                "OCOMP live index contains a non-live state",
            ));
        }
        if !days.insert(key.worldwide_day) || !intents.insert(key.intent_id) {
            return Err(storage_corruption_message(
                "OCOMP live index contains a duplicate job",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(day: u32, intent: u8) -> LiveIndexKey {
        LiveIndexKey {
            worldwide_day: WorldwideDay::new(day),
            intent_id: B256::repeat_byte(intent),
        }
    }

    #[test]
    fn pins_key_only_wire_vector_and_empty_representation() {
        let mut expected = b"OMLI\x00\x02\x00\x01\x00\x00\x00\x2a".to_vec();
        expected.extend_from_slice(&[3; 32]);
        assert_eq!(
            encode_live_scheduler_index(&[key(42, 3)]).unwrap(),
            expected
        );
        assert_eq!(
            decode_live_scheduler_index(&expected).unwrap(),
            vec![key(42, 3)]
        );
        assert_eq!(encode_live_scheduler_index(&[]).unwrap(), Vec::<u8>::new());
        assert!(decode_live_scheduler_index(&[]).unwrap().is_empty());
    }

    #[test]
    fn rejects_duplicate_day_intent_zero_identity_and_unordered_keys() {
        for index in [
            vec![key(1, 1), key(1, 2)],
            vec![key(1, 1), key(2, 1)],
            vec![key(1, 0)],
            vec![key(2, 2), key(1, 1)],
        ] {
            assert!(encode_live_scheduler_index(&index).is_err());
        }
        let good = encode_live_scheduler_index(&[key(1, 1), key(2, 2)]).unwrap();
        let mut duplicate_day = good.clone();
        duplicate_day[44..48].copy_from_slice(&1_u32.to_be_bytes());
        let mut duplicate_intent = good.clone();
        duplicate_intent[48..80].fill(1);
        for corrupt in [duplicate_day, duplicate_intent] {
            assert!(decode_live_scheduler_index(&corrupt).is_err());
        }
    }

    #[test]
    fn rejects_versions_count_truncation_and_trailing_bytes() {
        let encoded = encode_live_scheduler_index(&[key(42, 3)]).unwrap();
        for n in 1..encoded.len() {
            assert!(decode_live_scheduler_index(&encoded[..n]).is_err());
        }
        for (offset, value) in [(0, 0), (5, 1), (7, 0), (7, 2)] {
            let mut corrupt = encoded.clone();
            corrupt[offset] = value;
            assert!(decode_live_scheduler_index(&corrupt).is_err());
        }
        let mut trailing = encoded;
        trailing.push(0);
        assert!(decode_live_scheduler_index(&trailing).is_err());
    }

    #[test]
    fn largest_count_round_trips_and_next_count_is_rejected() {
        let index: Vec<_> = (1..=u16::MAX as u32)
            .map(|n| LiveIndexKey {
                worldwide_day: WorldwideDay::new(n),
                intent_id: B256::from(alloy_primitives::U256::from(n).to_be_bytes::<32>()),
            })
            .collect();
        assert_eq!(
            decode_live_scheduler_index(&encode_live_scheduler_index(&index).unwrap()).unwrap(),
            index
        );
        let mut too_many = index;
        too_many.push(LiveIndexKey {
            worldwide_day: WorldwideDay::new(65536),
            intent_id: B256::from(alloy_primitives::U256::from(65536).to_be_bytes::<32>()),
        });
        assert!(encode_live_scheduler_index(&too_many).is_err());
    }
}
