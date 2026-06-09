//! Participation bitmap encoding/decoding for block `extra_data`.
//!
//! Re-exports from [`outbe_primitives::participation`] — the canonical
//! implementation lives there to avoid circular dependencies between
//! `outbe-consensus` and `outbe-evm`.

pub use outbe_primitives::consensus::ParticipationData;
pub use outbe_primitives::participation::{
    decode_participation, decode_participation_extended, encode_participation,
    encode_participation_extended, DecodedParticipation,
};
