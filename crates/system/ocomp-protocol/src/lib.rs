//! Consensus-facing binary and commitment primitives for OCOMP.
//!
//! This crate deliberately stops below typed protocol objects. It owns OCB1
//! framing, bounded canonical fields, registered hash domains and ordered-list
//! roots, but it is not a general-purpose serialization framework.

pub mod codec;
pub mod error;
pub mod hash;
pub mod list;
pub mod registry;

pub use codec::{
    decode_envelope, encode_envelope, ensure_strictly_increasing, require_canonical_reencoding,
    AllocationStats, CanonicalReader, CanonicalWriter, CodecLimits, DecodedEnvelope,
    OCB1_HEADER_LEN, OCB1_MAGIC, OCB1_SCHEMA_VERSION,
};
pub use error::ProtocolError;
pub use hash::{framed_preimage, hash_framed, verify_framed_hash};
pub use list::{ordered_list_root, OrderedListLimits};
pub use registry::{HashDomain, ListKind, ObjectKind};
