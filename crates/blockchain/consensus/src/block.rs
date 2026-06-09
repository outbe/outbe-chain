//! Block wrapper for Commonware consensus.
//!
//! Wraps Reth's `SealedBlock` so it can be used as a Commonware consensus block.

use alloy_primitives::B256;
use bytes::{Buf, BufMut};
use commonware_codec::{EncodeSize, Read, Write};
use commonware_consensus::{types::Height, Heightable};
use commonware_cryptography::{Committable, Digestible};
use outbe_primitives::OutbeBlock;
use reth_ethereum::primitives::{AlloyBlockHeader, Block as RethBlockTrait, SealedBlock};

use crate::digest::Digest;

/// Consensus-aware block wrapper.
///
/// Wraps a sealed Ethereum block and implements Commonware's consensus traits
/// so blocks can flow through the Simplex engine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[repr(transparent)]
pub struct ConsensusBlock(pub SealedBlock<OutbeBlock>);

impl ConsensusBlock {
    /// Create from a Reth sealed block.
    pub fn from_sealed(block: SealedBlock<OutbeBlock>) -> Self {
        Self(block)
    }

    /// Unwrap into the inner sealed block.
    pub fn into_inner(self) -> SealedBlock<OutbeBlock> {
        self.0
    }

    /// Block hash.
    pub fn block_hash(&self) -> B256 {
        self.0.hash()
    }

    /// Block number.
    pub fn number(&self) -> u64 {
        self.0.number()
    }

    /// Block timestamp.
    pub fn timestamp(&self) -> u64 {
        self.0.timestamp()
    }

    /// Block timestamp in milliseconds.
    pub fn timestamp_millis(&self) -> u64 {
        self.0.header().timestamp_millis()
    }

    /// Parent block hash.
    pub fn parent_hash(&self) -> B256 {
        self.0.parent_hash()
    }

    /// Digest (block hash wrapped).
    pub fn digest(&self) -> Digest {
        Digest(self.block_hash())
    }

    /// Parent digest.
    pub fn parent_digest(&self) -> Digest {
        Digest(self.parent_hash())
    }
}

impl std::ops::Deref for ConsensusBlock {
    type Target = SealedBlock<OutbeBlock>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// --- Commonware trait implementations  ---

impl Write for ConsensusBlock {
    fn write(&self, buf: &mut impl BufMut) {
        use alloy_rlp::Encodable as _;
        self.0.encode(buf);
    }
}

impl Read for ConsensusBlock {
    type Cfg = ();

    fn read_cfg(buf: &mut impl Buf, _cfg: &Self::Cfg) -> Result<Self, commonware_codec::Error> {
        let header = alloy_rlp::Header::decode(&mut buf.chunk()).map_err(|rlp_err| {
            commonware_codec::Error::Wrapped("reading RLP header", rlp_err.into())
        })?;

        if header.length_with_payload() > buf.remaining() {
            return Err(commonware_codec::Error::EndOfBuffer);
        }
        let bytes = buf.copy_to_bytes(header.length_with_payload());

        let inner: OutbeBlock =
            alloy_rlp::Decodable::decode(&mut bytes.as_ref()).map_err(|rlp_err| {
                commonware_codec::Error::Wrapped("reading RLP encoded block", rlp_err.into())
            })?;

        Ok(Self::from_sealed(inner.seal_slow()))
    }
}

impl EncodeSize for ConsensusBlock {
    fn encode_size(&self) -> usize {
        use alloy_rlp::Encodable as _;
        self.0.length()
    }
}

impl Committable for ConsensusBlock {
    type Commitment = Digest;

    fn commitment(&self) -> Self::Commitment {
        self.digest()
    }
}

impl Digestible for ConsensusBlock {
    type Digest = Digest;

    fn digest(&self) -> Self::Digest {
        self.digest()
    }
}

impl Heightable for ConsensusBlock {
    fn height(&self) -> Height {
        Height::new(self.number())
    }
}

impl commonware_consensus::Block for ConsensusBlock {
    fn parent(&self) -> Digest {
        self.parent_digest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Bytes;
    use commonware_codec::Encode as _;
    use outbe_primitives::OutbeHeader;
    use reth_ethereum::Block;

    fn sample_block(number: u64, extra: &[u8]) -> ConsensusBlock {
        let mut block = Block::default();
        block.header.number = number;
        block.header.extra_data = Bytes::copy_from_slice(extra);
        let block = block.map_header(OutbeHeader::new);
        ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
    }

    /// marshal-2: the marshal genesis anchor is a `ConsensusBlock` whose
    /// commitment the marshal re-derives via THIS codec round-trip (RLP encode
    /// -> RLP decode -> `seal_slow`) and compares in `ensure_genesis_anchor`,
    /// which panics on mismatch at the SECOND boot. A header/extra_data encoding
    /// change that did not round-trip losslessly would compile clean but crash a
    /// restarted node. This pins that `digest() == commitment() == block_hash()`
    /// survives the round-trip, including non-empty `extra_data` (genesis carries
    /// it via `OutbeBlockArtifacts`).
    #[test]
    fn consensus_block_codec_round_trip_preserves_commitment() {
        for (number, extra) in [
            (0u64, b"genesis-anchor".as_slice()),
            (7, b"".as_slice()),
            (12_345, [0xEEu8; 64].as_slice()),
        ] {
            let original = sample_block(number, extra);

            // digest == commitment == sealed block hash.
            assert_eq!(original.digest().0, original.block_hash());
            assert_eq!(original.commitment(), original.digest());

            let encoded = original.encode();
            let mut buf = encoded.as_ref();
            let decoded = ConsensusBlock::read_cfg(&mut buf, &())
                .expect("genesis-anchor ConsensusBlock must RLP round-trip");

            assert_eq!(
                decoded.digest(),
                original.digest(),
                "codec round-trip must preserve digest — marshal ensure_genesis_anchor \
                 compares this commitment and panics on mismatch (second boot)"
            );
            assert_eq!(decoded.commitment(), original.commitment());
            assert_eq!(decoded.block_hash(), original.block_hash());
            assert_eq!(decoded.number(), original.number());
            assert!(
                buf.is_empty(),
                "decode must consume exactly the encoded block bytes"
            );
        }
    }
}
