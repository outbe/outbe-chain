use super::{
    decode_retirement, encode_retirement,
    model::{CapacityForfeitureDetail, TerminalReceiptCommon, WwdTerminalReceipt},
};
use crate::{errors::storage_corruption_message, schema::terminal_outcome};
use alloy_primitives::{B256, U256};
use outbe_primitives::{error::Result, time::WorldwideDay};

const MAGIC: &[u8; 4] = b"OMTR";
const VERSION: u16 = 1;
pub(crate) const COMMON_LEN: usize = 116;
pub(crate) const CAPACITY_LEN: usize = 208;

pub(crate) fn encode(receipt: &WwdTerminalReceipt) -> Vec<u8> {
    let common = receipt.common();
    let mut bytes = Vec::with_capacity(CAPACITY_LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_be_bytes());
    bytes.push(receipt.outcome());
    bytes.extend_from_slice(&common.worldwide_day.value().to_be_bytes());
    bytes.extend_from_slice(&common.value_routed.to_be_bytes::<32>());
    bytes.extend_from_slice(&common.carry_over_before.to_be_bytes::<32>());
    bytes.extend_from_slice(&common.carry_over_after.to_be_bytes::<32>());
    bytes.push(encode_retirement(common.retirement));
    bytes.extend_from_slice(&common.block_number.to_be_bytes());
    if let WwdTerminalReceipt::CapacityForfeiture { detail, .. } = receipt {
        bytes.extend_from_slice(&detail.max_retained_wwds.to_be_bytes());
        bytes.extend_from_slice(&detail.retained_count_before.to_be_bytes());
        bytes.extend_from_slice(detail.sealed_collection_root.as_slice());
        bytes.extend_from_slice(&detail.forfeited_count.to_be_bytes());
        bytes.extend_from_slice(&detail.forfeited_nominal.to_be_bytes::<32>());
        bytes.extend_from_slice(&detail.source_generation.to_be_bytes());
        bytes.extend_from_slice(&detail.retired_generation.to_be_bytes());
    }
    bytes
}

pub(crate) fn decode(bytes: &[u8], worldwide_day: WorldwideDay) -> Result<WwdTerminalReceipt> {
    let mut reader = Reader { bytes, offset: 0 };
    if reader.take::<4>()? != *MAGIC || u16::from_be_bytes(reader.take()?) != VERSION {
        return Err(storage_corruption_message(
            "terminal receipt magic/version mismatch",
        ));
    }
    let outcome = reader.take::<1>()?[0];
    let stored_day = WorldwideDay::new(u32::from_be_bytes(reader.take()?));
    if stored_day != worldwide_day {
        return Err(storage_corruption_message(
            "terminal receipt WorldwideDay/key mismatch",
        ));
    }
    let common = TerminalReceiptCommon {
        worldwide_day,
        value_routed: U256::from_be_bytes(reader.take::<32>()?),
        carry_over_before: U256::from_be_bytes(reader.take::<32>()?),
        carry_over_after: U256::from_be_bytes(reader.take::<32>()?),
        retirement: decode_retirement(reader.take::<1>()?[0])?,
        block_number: u64::from_be_bytes(reader.take()?),
    };
    let receipt = match outcome {
        terminal_outcome::MISSED_OFFERING => WwdTerminalReceipt::MissedOffering(common),
        terminal_outcome::METADOSIS_FAILURE => WwdTerminalReceipt::MetadosisFailure(common),
        terminal_outcome::CAPACITY_FORFEITURE => WwdTerminalReceipt::CapacityForfeiture {
            common,
            detail: CapacityForfeitureDetail {
                max_retained_wwds: u32::from_be_bytes(reader.take()?),
                retained_count_before: u32::from_be_bytes(reader.take()?),
                sealed_collection_root: B256::from(reader.take::<32>()?),
                forfeited_count: u32::from_be_bytes(reader.take()?),
                forfeited_nominal: U256::from_be_bytes(reader.take::<32>()?),
                source_generation: u64::from_be_bytes(reader.take()?),
                retired_generation: u64::from_be_bytes(reader.take()?),
            },
        },
        _ => {
            return Err(storage_corruption_message(
                "Metadosis WWD has an unknown terminal receipt outcome",
            ))
        }
    };
    if reader.offset != bytes.len() {
        return Err(storage_corruption_message(
            "terminal receipt has trailing bytes",
        ));
    }
    Ok(receipt)
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self.offset + N;
        let field = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| storage_corruption_message("truncated terminal receipt"))?;
        self.offset = end;
        field
            .try_into()
            .map_err(|_| storage_corruption_message("invalid terminal receipt field width"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_compressed_entities::RetirementOutcome;

    fn common() -> TerminalReceiptCommon {
        TerminalReceiptCommon {
            worldwide_day: WorldwideDay::new(42),
            value_routed: U256::from(5),
            carry_over_before: U256::from(2),
            carry_over_after: U256::from(7),
            retirement: RetirementOutcome::NotPresent,
            block_number: 9,
        }
    }

    #[test]
    fn common_wire_vector_is_pinned_and_round_trips_both_outcomes() {
        let mut expected = vec![0; 116];
        expected[..11].copy_from_slice(&[b'O', b'M', b'T', b'R', 0, 1, 1, 0, 0, 0, 42]);
        expected[42] = 5;
        expected[74] = 2;
        expected[106] = 7;
        expected[107] = 1;
        expected[115] = 9;
        assert_eq!(
            encode(&WwdTerminalReceipt::MissedOffering(common())),
            expected
        );
        for receipt in [
            WwdTerminalReceipt::MissedOffering(common()),
            WwdTerminalReceipt::MetadosisFailure(common()),
        ] {
            assert_eq!(
                decode(&encode(&receipt), WorldwideDay::new(42)).unwrap(),
                receipt
            );
        }
    }

    #[test]
    fn capacity_vector_has_one_common_record_and_fixed_detail() {
        let receipt = WwdTerminalReceipt::CapacityForfeiture {
            common: common(),
            detail: CapacityForfeitureDetail {
                max_retained_wwds: 2,
                retained_count_before: 2,
                sealed_collection_root: B256::repeat_byte(3),
                forfeited_count: 4,
                forfeited_nominal: U256::from(5),
                source_generation: 0,
                retired_generation: 1,
            },
        };
        let encoded = encode(&receipt);
        assert_eq!(encoded.len(), 208);
        assert_eq!(&encoded[116..124], &[0, 0, 0, 2, 0, 0, 0, 2]);
        assert_eq!(&encoded[124..156], &[3; 32]);
        assert_eq!(encoded[159], 4);
        assert_eq!(encoded[191], 5);
        assert_eq!(encoded[207], 1);
        assert_eq!(decode(&encoded, WorldwideDay::new(42)).unwrap(), receipt);
        for length in 0..encoded.len() {
            assert!(decode(&encoded[..length], WorldwideDay::new(42)).is_err());
        }
    }

    #[test]
    fn rejects_corrupt_header_tag_retirement_key_and_trailing_bytes() {
        let encoded = encode(&WwdTerminalReceipt::MissedOffering(common()));
        for (offset, value) in [(0, 0), (5, 2), (6, 0), (6, 7), (107, 0), (107, 3)] {
            let mut corrupt = encoded.clone();
            corrupt[offset] = value;
            assert!(decode(&corrupt, WorldwideDay::new(42)).is_err());
        }
        assert!(decode(&encoded, WorldwideDay::new(43)).is_err());
        let mut trailing = encoded;
        trailing.push(0);
        assert!(decode(&trailing, WorldwideDay::new(42)).is_err());
    }
}
