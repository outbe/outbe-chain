//! Authenticated block witness recording, comparison and socket-free replay.
mod authenticate;
mod codec;
mod state;
mod tape;

pub use crate::call_context::WitnessExecutionScope;
pub use codec::{
    compressed_witness_bound, decode_witness_payload_v1, encode_witness_payload_v1,
    WitnessCodecError,
};
pub use state::*;
pub use tape::{EnclaveWitnessEntryV1, EnclaveWitnessTapeV1, TxWitnessV1};

#[cfg(test)]
mod codec_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod tape_tests;

pub(crate) use authenticate::authenticate_request;
