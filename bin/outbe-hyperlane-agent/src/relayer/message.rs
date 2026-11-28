//! The Hyperlane message that the origin Mailbox dispatches.
//!
//! Ported from hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/hyperlane-core/src/types/message.rs` (Apache-2.0, see
//! `LICENSE-APACHE`). Changes: alloy types, read only.

use alloy_primitives::{keccak256, B256};

use crate::errors::RelayError;

/// Length of the fixed fields: version, nonce, origin, sender, destination
/// and recipient.
const HEADER_LEN: usize = 1 + 4 + 4 + 32 + 4 + 32;

/// The fields that the relayer reads. The body stays in the raw bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub nonce: u32,
    pub origin: u32,
    pub destination: u32,
    pub recipient: B256,
}

impl Message {
    /// Reads the header of `raw`, the `message` bytes of a `Dispatch` event.
    pub fn decode(raw: &[u8]) -> Result<Self, RelayError> {
        if raw.len() < HEADER_LEN {
            return Err(RelayError::ShortMessage(raw.len()));
        }
        let u32_at = |at: usize| u32::from_be_bytes(raw[at..at + 4].try_into().expect("4 bytes"));
        Ok(Self {
            nonce: u32_at(1),
            origin: u32_at(5),
            destination: u32_at(41),
            recipient: B256::from_slice(&raw[45..77]),
        })
    }
}

/// The message id: the leaf of the origin `MerkleTreeHook`.
pub fn message_id(raw: &[u8]) -> B256 {
    keccak256(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_reads_the_header() {
        let mut raw = vec![3u8];
        raw.extend_from_slice(&7u32.to_be_bytes());
        raw.extend_from_slice(&54322345u32.to_be_bytes());
        raw.extend_from_slice(B256::repeat_byte(0xaa).as_slice());
        raw.extend_from_slice(&97u32.to_be_bytes());
        raw.extend_from_slice(B256::repeat_byte(0xbb).as_slice());
        // A message without a body is valid.
        assert_eq!(
            Message::decode(&raw).unwrap(),
            Message {
                nonce: 7,
                origin: 54322345,
                destination: 97,
                recipient: B256::repeat_byte(0xbb),
            }
        );
        raw.extend_from_slice(b"body");
        assert_eq!(Message::decode(&raw).unwrap().destination, 97);
        assert_eq!(message_id(&raw), keccak256(&raw));
        assert!(Message::decode(&raw[..76]).is_err());
    }
}
