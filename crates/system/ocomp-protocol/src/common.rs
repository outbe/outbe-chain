use crate::{
    codec::{CanonicalReader, CanonicalWriter},
    error::ProtocolError,
    schema::{require, NestedCodec, SchemaLimits},
};

/// Length-prefixed bytes governed by the bundle's ordinary field cap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedBytes(pub Vec<u8>);

/// Length-prefixed proof bytes governed by the stricter proof cap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProofBytes(pub Vec<u8>);

macro_rules! impl_capped_bytes_codec {
    ($type:ident, $cap:ident, $label:literal) => {
        impl NestedCodec for $type {
            fn validate(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
                require(self.0.len() <= limits.$cap, $label)
            }

            fn encode_nested(
                &self,
                output: &mut CanonicalWriter,
                limits: &SchemaLimits,
            ) -> Result<(), ProtocolError> {
                output.write_bounded_bytes(&self.0, limits.$cap)
            }

            fn decode_nested(
                input: &mut CanonicalReader<'_>,
                limits: &SchemaLimits,
            ) -> Result<Self, ProtocolError> {
                Ok(Self(input.read_bounded_bytes(limits.$cap)?.to_vec()))
            }
        }
    };
}

impl_capped_bytes_codec!(BoundedBytes, max_bounded_bytes, "bounded byte field cap");
impl_capped_bytes_codec!(ProofBytes, max_proof_bytes, "proof byte field cap");
