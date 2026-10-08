//! Public encrypted carrier with a private certified materialization witness.

use alloy_primitives::B256;

use crate::{
    codec::{require_canonical_reencoding, CanonicalReader, CanonicalWriter},
    common::BoundedBytes,
    schema::{require, NestedCodec},
    ProtocolError, SchemaLimits,
};

use super::MAX_NOD_MATERIALIZATION_ACTIONS;

const MAGIC: [u8; 4] = *b"NME2";
const VERSION: u16 = 2;

/// Canonical materialization input containing no plaintext NOD amounts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtectedNodMaterializationV2 {
    pub queue_sequence: u64,
    pub first_nod_ordinal: u32,
    pub encryption_binding: B256,
    pub encrypted_witness: BoundedBytes,
    pub encrypted_nods: Vec<BoundedBytes>,
}

impl ProtectedNodMaterializationV2 {
    pub fn encode_canonical(&self, limits: &SchemaLimits) -> Result<Vec<u8>, ProtocolError> {
        self.validate(limits)?;
        let mut output = CanonicalWriter::new(limits.codec);
        output.write_fixed(&MAGIC)?;
        output.write_u16(VERSION)?;
        output.write_u16(0)?;
        output.write_u64(self.queue_sequence)?;
        output.write_u32(self.first_nod_ordinal)?;
        output.write_b256(self.encryption_binding)?;
        self.encrypted_witness.encode_nested(&mut output, limits)?;
        output.write_vec(
            &self.encrypted_nods,
            MAX_NOD_MATERIALIZATION_ACTIONS,
            |writer, body| body.encode_nested(writer, limits),
        )?;
        Ok(output.into_bytes())
    }

    pub fn decode_canonical(encoded: &[u8], limits: &SchemaLimits) -> Result<Self, ProtocolError> {
        let mut input = CanonicalReader::new(encoded, limits.codec)?;
        let magic = input.read_fixed()?;
        if magic != MAGIC {
            return Err(ProtocolError::InvalidMagic(magic));
        }
        let version = input.read_u16()?;
        if version != VERSION {
            return Err(ProtocolError::UnsupportedSchemaVersion(version));
        }
        require(
            input.read_u16()? == 0,
            "protected materialization reserved bits",
        )?;
        let value = Self {
            queue_sequence: input.read_u64()?,
            first_nod_ordinal: input.read_u32()?,
            encryption_binding: input.read_b256()?,
            encrypted_witness: BoundedBytes::decode_nested(&mut input, limits)?,
            encrypted_nods: input.read_vec(MAX_NOD_MATERIALIZATION_ACTIONS, 4, |reader| {
                BoundedBytes::decode_nested(reader, limits)
            })?,
        };
        input.finish()?;
        value.validate(limits)?;
        require_canonical_reencoding(encoded, &value.encode_canonical(limits)?)?;
        Ok(value)
    }

    fn validate(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        require(
            self.queue_sequence != 0,
            "protected materialization queue sequence",
        )?;
        require(
            !self.encryption_binding.is_zero(),
            "protected materialization binding",
        )?;
        require(
            !self.encrypted_witness.0.is_empty(),
            "protected materialization witness",
        )?;
        self.encrypted_witness.validate(limits)?;
        require(
            !self.encrypted_nods.is_empty()
                && self.encrypted_nods.len() <= MAX_NOD_MATERIALIZATION_ACTIONS,
            "protected materialization action count",
        )?;
        for body in &self.encrypted_nods {
            require(!body.0.is_empty(), "protected materialization empty Nod")?;
            body.validate(limits)?;
        }
        Ok(())
    }
}
