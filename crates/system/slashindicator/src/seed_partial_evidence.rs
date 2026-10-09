//! Evidence wire format for VRF seed-partial equivocation slashing.
//!
//! A validator that identity-signs two DIFFERENT `bls_seed_partial`s for the
//! same `(round, vrf_material_version)` has equivocated on its VRF
//! contribution. A rider signature binds each partial to the validator's MinPk
//! identity key (see `outbe_consensus::proof::seed_partial`). Thus the two
//! identity signatures alone self-authenticate the offense. No committee
//! polynomial is needed. This mirrors the existing double-sign /
//! conflicting-vote evidence. An honest validator produces exactly one partial
//! per `(round, version)`. It never identity-signs a second distinct one. Thus
//! a valid pair cannot frame an honest node.
//!
//! Wire (fixed 365 bytes, big-endian scalars):
//! ```text
//! magic[4]=b"SPE1" | version[1]=0x01 | round_epoch[8] | round_view[8] |
//! vrf_version[8] | signer_pubkey[48] | partial_1[48] | identity_sig_1[96] |
//! partial_2[48] | identity_sig_2[96]
//! ```

use alloy_primitives::{keccak256, B256};
use outbe_primitives::error::{PrecompileError, Result};

pub(crate) const SPE1_MAGIC: &[u8; 4] = b"SPE1";
pub(crate) const SPE1_VERSION: u8 = 0x01;
/// Fixed wire length: 4 + 1 + 8 + 8 + 8 + 48 + 48 + 96 + 48 + 96.
pub(crate) const SPE1_LEN: usize = 365;

/// Reads fixed-size big-endian fields from the front of an evidence body.
///
/// Every decoder checks the full length first, so a read past the end is
/// impossible for accepted input. The reader still returns a revert instead
/// of a panic when a read is short.
struct WireReader<'a> {
    rest: &'a [u8],
}

impl<'a> WireReader<'a> {
    fn new(body: &'a [u8]) -> Self {
        Self { rest: body }
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let (field, rest) = self
            .rest
            .split_first_chunk::<N>()
            .ok_or_else(|| PrecompileError::Revert("evidence field is truncated".into()))?;
        self.rest = rest;
        Ok(*field)
    }

    fn u64(&mut self) -> Result<u64> {
        self.bytes::<8>().map(u64::from_be_bytes)
    }

    fn u32(&mut self) -> Result<u32> {
        self.bytes::<4>().map(u32::from_be_bytes)
    }

    fn remaining(&self) -> &'a [u8] {
        self.rest
    }
}

/// Decoded seed-partial equivocation evidence.
pub(crate) struct SeedPartialEquivocationEvidence {
    pub round_epoch: u64,
    pub round_view: u64,
    pub vrf_version: u64,
    pub signer_pubkey: [u8; 48],
    pub partial_1: [u8; 48],
    pub identity_sig_1: [u8; 96],
    pub partial_2: [u8; 48],
    pub identity_sig_2: [u8; 96],
}

impl SeedPartialEquivocationEvidence {
    /// Decode the fixed-length wire form. Rejects any wrong length (no trailing
    /// bytes), wrong magic, or wrong version.
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() != SPE1_LEN {
            return Err(PrecompileError::Revert(format!(
                "SPE1 evidence must be exactly {SPE1_LEN} bytes, got {}",
                data.len()
            )));
        }
        if &data[0..4] != SPE1_MAGIC.as_slice() {
            return Err(PrecompileError::Revert("SPE1 evidence bad magic".into()));
        }
        if data[4] != SPE1_VERSION {
            return Err(PrecompileError::Revert("SPE1 evidence bad version".into()));
        }
        let mut wire = WireReader::new(&data[5..]);
        let round_epoch = wire.u64()?;
        let round_view = wire.u64()?;
        let vrf_version = wire.u64()?;
        // Remaining fixed-size byte fields.
        let signer_pubkey = wire.bytes::<48>()?;
        let partial_1 = wire.bytes::<48>()?;
        let identity_sig_1 = wire.bytes::<96>()?;
        let partial_2 = wire.bytes::<48>()?;
        let identity_sig_2 = wire.bytes::<96>()?;
        debug_assert!(wire.remaining().is_empty());

        Ok(Self {
            round_epoch,
            round_view,
            vrf_version,
            signer_pubkey,
            partial_1,
            identity_sig_1,
            partial_2,
            identity_sig_2,
        })
    }

    /// keccak256 of the signer's 48-byte identity pubkey, for ValidatorSet
    /// reverse lookup.
    pub fn pubkey_hash(&self) -> B256 {
        keccak256(self.signer_pubkey)
    }

    /// Canonical dedup key. It is order-independent in the two partials. Thus
    /// the same equivocation, submitted with the partials in either order, maps
    /// to one slash. The key binds the round and material version, so distinct
    /// equivocations are distinct keys.
    pub fn dedup_hash(&self) -> B256 {
        let (lo, hi) = if self.partial_1 <= self.partial_2 {
            (&self.partial_1, &self.partial_2)
        } else {
            (&self.partial_2, &self.partial_1)
        };
        let mut buf = Vec::with_capacity(4 + 24 + 48 + 48 + 48);
        buf.extend_from_slice(SPE1_MAGIC);
        buf.extend_from_slice(&self.round_epoch.to_be_bytes());
        buf.extend_from_slice(&self.round_view.to_be_bytes());
        buf.extend_from_slice(&self.vrf_version.to_be_bytes());
        buf.extend_from_slice(&self.signer_pubkey);
        buf.extend_from_slice(lo);
        buf.extend_from_slice(hi);
        keccak256(buf)
    }
}

// =============================================================================
// Invalid-partial evidence (IPE1). It slashes a single identity-signed partial
// that fails verification against the committee's full VRF polynomial.
// =============================================================================

pub(crate) const IPE1_MAGIC: &[u8; 4] = b"IPE1";
pub(crate) const IPE1_VERSION: u8 = 0x01;
/// Fixed prefix before the variable-length polynomial commitment:
/// magic(4)+version(1)+committee_set_hash(32)+round_epoch(8)+round_view(8)
/// +vrf_version(8)+signer_index(4)+signer_pubkey(48)+partial(48)+identity_sig(96)
/// +commitment_len(4).
pub(crate) const IPE1_PREFIX_LEN: usize = 4 + 1 + 32 + 8 + 8 + 8 + 4 + 48 + 48 + 96 + 4;

/// Decoded invalid-seed-partial evidence.
pub(crate) struct InvalidSeedPartialEvidence {
    pub committee_set_hash: B256,
    pub round_epoch: u64,
    pub round_view: u64,
    pub vrf_version: u64,
    pub signer_index: u32,
    pub signer_pubkey: [u8; 48],
    pub partial: [u8; 48],
    pub identity_sig: [u8; 96],
    /// `commonware_codec::Encode(Sharing<MinSig>)` of the committee polynomial.
    pub commitment: Vec<u8>,
}

impl InvalidSeedPartialEvidence {
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < IPE1_PREFIX_LEN {
            return Err(PrecompileError::Revert(format!(
                "IPE1 evidence too short: need at least {IPE1_PREFIX_LEN} bytes, got {}",
                data.len()
            )));
        }
        if &data[0..4] != IPE1_MAGIC.as_slice() {
            return Err(PrecompileError::Revert("IPE1 evidence bad magic".into()));
        }
        if data[4] != IPE1_VERSION {
            return Err(PrecompileError::Revert("IPE1 evidence bad version".into()));
        }
        let mut wire = WireReader::new(&data[5..]);
        let committee_set_hash = B256::from(wire.bytes::<32>()?);
        let round_epoch = wire.u64()?;
        let round_view = wire.u64()?;
        let vrf_version = wire.u64()?;
        let signer_index = wire.u32()?;
        let signer_pubkey = wire.bytes::<48>()?;
        let partial = wire.bytes::<48>()?;
        let identity_sig = wire.bytes::<96>()?;
        let commitment_len = wire.u32()? as usize;
        let pos = data.len() - wire.remaining().len();
        debug_assert_eq!(pos, IPE1_PREFIX_LEN);
        let commitment = data
            .get(pos..pos + commitment_len)
            .ok_or_else(|| PrecompileError::Revert("IPE1 commitment length exceeds input".into()))?
            .to_vec();
        if pos + commitment_len != data.len() {
            return Err(PrecompileError::Revert(
                "IPE1 evidence has trailing bytes".into(),
            ));
        }
        Ok(Self {
            committee_set_hash,
            round_epoch,
            round_view,
            vrf_version,
            signer_index,
            signer_pubkey,
            partial,
            identity_sig,
            commitment,
        })
    }

    pub fn pubkey_hash(&self) -> B256 {
        keccak256(self.signer_pubkey)
    }

    /// Dedup key: one slash per `(round, version, signer, partial)`.
    pub fn dedup_hash(&self) -> B256 {
        let mut buf = Vec::with_capacity(4 + 8 + 8 + 8 + 48 + 48);
        buf.extend_from_slice(IPE1_MAGIC);
        buf.extend_from_slice(&self.round_epoch.to_be_bytes());
        buf.extend_from_slice(&self.round_view.to_be_bytes());
        buf.extend_from_slice(&self.vrf_version.to_be_bytes());
        buf.extend_from_slice(&self.signer_pubkey);
        buf.extend_from_slice(&self.partial);
        keccak256(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bytes() -> Vec<u8> {
        let mut data = Vec::with_capacity(SPE1_LEN);
        data.extend_from_slice(SPE1_MAGIC);
        data.push(SPE1_VERSION);
        data.extend_from_slice(&7u64.to_be_bytes()); // round_epoch
        data.extend_from_slice(&9u64.to_be_bytes()); // round_view
        data.extend_from_slice(&2u64.to_be_bytes()); // vrf_version
        data.extend_from_slice(&[0xAA; 48]); // signer_pubkey
        data.extend_from_slice(&[0x01; 48]); // partial_1
        data.extend_from_slice(&[0xB1; 96]); // identity_sig_1
        data.extend_from_slice(&[0x02; 48]); // partial_2
        data.extend_from_slice(&[0xB2; 96]); // identity_sig_2
        data
    }

    #[test]
    fn decode_roundtrip_fields() {
        let ev = SeedPartialEquivocationEvidence::decode(&sample_bytes()).unwrap();
        assert_eq!(ev.round_epoch, 7);
        assert_eq!(ev.round_view, 9);
        assert_eq!(ev.vrf_version, 2);
        assert_eq!(ev.signer_pubkey, [0xAA; 48]);
        assert_eq!(ev.partial_1, [0x01; 48]);
        assert_eq!(ev.identity_sig_1, [0xB1; 96]);
        assert_eq!(ev.partial_2, [0x02; 48]);
        assert_eq!(ev.identity_sig_2, [0xB2; 96]);
    }

    #[test]
    fn decode_rejects_wrong_length_magic_version() {
        let mut short = sample_bytes();
        short.pop();
        assert!(SeedPartialEquivocationEvidence::decode(&short).is_err());

        let mut trailing = sample_bytes();
        trailing.push(0);
        assert!(SeedPartialEquivocationEvidence::decode(&trailing).is_err());

        let mut bad_magic = sample_bytes();
        bad_magic[0] = b'X';
        assert!(SeedPartialEquivocationEvidence::decode(&bad_magic).is_err());

        let mut bad_version = sample_bytes();
        bad_version[4] = 0x09;
        assert!(SeedPartialEquivocationEvidence::decode(&bad_version).is_err());
    }

    fn ipe1_sample(commitment_len: usize) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(IPE1_MAGIC);
        d.push(IPE1_VERSION);
        d.extend_from_slice(&[0xCC; 32]); // committee_set_hash
        d.extend_from_slice(&3u64.to_be_bytes()); // round_epoch
        d.extend_from_slice(&5u64.to_be_bytes()); // round_view
        d.extend_from_slice(&0u64.to_be_bytes()); // vrf_version
        d.extend_from_slice(&2u32.to_be_bytes()); // signer_index
        d.extend_from_slice(&[0xAA; 48]); // signer_pubkey
        d.extend_from_slice(&[0x11; 48]); // partial
        d.extend_from_slice(&[0xB1; 96]); // identity_sig
        d.extend_from_slice(&(commitment_len as u32).to_be_bytes());
        d.extend_from_slice(&vec![0x77u8; commitment_len]);
        d
    }

    #[test]
    fn ipe1_decode_roundtrip_and_rejects_trailing() {
        let ev = InvalidSeedPartialEvidence::decode(&ipe1_sample(100)).unwrap();
        assert_eq!(ev.round_epoch, 3);
        assert_eq!(ev.round_view, 5);
        assert_eq!(ev.signer_index, 2);
        assert_eq!(ev.commitment.len(), 100);

        let mut trailing = ipe1_sample(100);
        trailing.push(0);
        assert!(InvalidSeedPartialEvidence::decode(&trailing).is_err());

        let mut bad_len = ipe1_sample(100);
        // claim a commitment longer than present
        let off = IPE1_PREFIX_LEN - 4;
        bad_len[off..off + 4].copy_from_slice(&9999u32.to_be_bytes());
        assert!(InvalidSeedPartialEvidence::decode(&bad_len).is_err());

        let mut bad_magic = ipe1_sample(8);
        bad_magic[0] = b'X';
        assert!(InvalidSeedPartialEvidence::decode(&bad_magic).is_err());
    }

    #[test]
    fn dedup_hash_is_partial_order_independent() {
        let a = SeedPartialEquivocationEvidence::decode(&sample_bytes()).unwrap();
        // Swap partial_1/partial_2 (and their sigs) -> same dedup hash.
        let mut swapped = sample_bytes();
        // partial_1 at offset 5+24+48 = 77, identity_sig_1 at 125, partial_2 at 221, sig_2 at 269.
        let p1_off = 5 + 24 + 48;
        let s1_off = p1_off + 48;
        let p2_off = s1_off + 96;
        let s2_off = p2_off + 48;
        swapped[p1_off..p1_off + 48].copy_from_slice(&[0x02; 48]);
        swapped[s1_off..s1_off + 96].copy_from_slice(&[0xB2; 96]);
        swapped[p2_off..p2_off + 48].copy_from_slice(&[0x01; 48]);
        swapped[s2_off..s2_off + 96].copy_from_slice(&[0xB1; 96]);
        let b = SeedPartialEquivocationEvidence::decode(&swapped).unwrap();
        assert_eq!(a.dedup_hash(), b.dedup_hash());
    }
    /// The revert reason of a decode result, or `None` for success or another
    /// error kind.
    fn revert_reason<T>(result: Result<T>) -> Option<String> {
        match result {
            Err(PrecompileError::Revert(reason)) => Some(reason),
            _ => None,
        }
    }

    /// Bytes `start, start + 1, ...` (wrapping), so each field is distinct.
    fn ramp<const N: usize>(start: u8) -> [u8; N] {
        core::array::from_fn(|i| start.wrapping_add(i as u8))
    }

    #[test]
    fn spe1_decode_reads_each_field_at_its_offset() {
        let mut data = Vec::with_capacity(SPE1_LEN);
        data.extend_from_slice(SPE1_MAGIC);
        data.push(SPE1_VERSION);
        data.extend_from_slice(&0x0102_0304_0506_0708u64.to_be_bytes());
        data.extend_from_slice(&0x1112_1314_1516_1718u64.to_be_bytes());
        data.extend_from_slice(&0x2122_2324_2526_2728u64.to_be_bytes());
        data.extend_from_slice(&ramp::<48>(0x30));
        data.extend_from_slice(&ramp::<48>(0x60));
        data.extend_from_slice(&ramp::<96>(0x90));
        data.extend_from_slice(&ramp::<48>(0xF0));
        data.extend_from_slice(&ramp::<96>(0x20));
        assert_eq!(data.len(), SPE1_LEN);

        let ev = SeedPartialEquivocationEvidence::decode(&data).unwrap();
        assert_eq!(ev.round_epoch, 0x0102_0304_0506_0708);
        assert_eq!(ev.round_view, 0x1112_1314_1516_1718);
        assert_eq!(ev.vrf_version, 0x2122_2324_2526_2728);
        assert_eq!(ev.signer_pubkey, ramp::<48>(0x30));
        assert_eq!(ev.partial_1, ramp::<48>(0x60));
        assert_eq!(ev.identity_sig_1, ramp::<96>(0x90));
        assert_eq!(ev.partial_2, ramp::<48>(0xF0));
        assert_eq!(ev.identity_sig_2, ramp::<96>(0x20));
    }

    #[test]
    fn spe1_decode_reports_exact_reverts_in_check_order() {
        let mut short = sample_bytes();
        short.pop();
        short[0] = b'X';
        assert_eq!(
            revert_reason(SeedPartialEquivocationEvidence::decode(&short)),
            Some("SPE1 evidence must be exactly 365 bytes, got 364".to_string())
        );

        let mut trailing = sample_bytes();
        trailing.push(0);
        assert_eq!(
            revert_reason(SeedPartialEquivocationEvidence::decode(&trailing)),
            Some("SPE1 evidence must be exactly 365 bytes, got 366".to_string())
        );

        let mut bad_magic = sample_bytes();
        bad_magic[0] = b'X';
        bad_magic[4] = 0x09;
        assert_eq!(
            revert_reason(SeedPartialEquivocationEvidence::decode(&bad_magic)),
            Some("SPE1 evidence bad magic".to_string())
        );

        let mut bad_version = sample_bytes();
        bad_version[4] = 0x09;
        assert_eq!(
            revert_reason(SeedPartialEquivocationEvidence::decode(&bad_version)),
            Some("SPE1 evidence bad version".to_string())
        );
    }

    #[test]
    fn ipe1_decode_reads_each_field_at_its_offset() {
        let commitment: Vec<u8> = (0..37u8).collect();
        let mut data = Vec::new();
        data.extend_from_slice(IPE1_MAGIC);
        data.push(IPE1_VERSION);
        data.extend_from_slice(&ramp::<32>(0x40));
        data.extend_from_slice(&0x0102_0304_0506_0708u64.to_be_bytes());
        data.extend_from_slice(&0x1112_1314_1516_1718u64.to_be_bytes());
        data.extend_from_slice(&0x2122_2324_2526_2728u64.to_be_bytes());
        data.extend_from_slice(&0x3132_3334u32.to_be_bytes());
        data.extend_from_slice(&ramp::<48>(0x50));
        data.extend_from_slice(&ramp::<48>(0x80));
        data.extend_from_slice(&ramp::<96>(0xB0));
        data.extend_from_slice(&(commitment.len() as u32).to_be_bytes());
        assert_eq!(data.len(), IPE1_PREFIX_LEN);
        data.extend_from_slice(&commitment);

        let ev = InvalidSeedPartialEvidence::decode(&data).unwrap();
        assert_eq!(ev.committee_set_hash, B256::from(ramp::<32>(0x40)));
        assert_eq!(ev.round_epoch, 0x0102_0304_0506_0708);
        assert_eq!(ev.round_view, 0x1112_1314_1516_1718);
        assert_eq!(ev.vrf_version, 0x2122_2324_2526_2728);
        assert_eq!(ev.signer_index, 0x3132_3334);
        assert_eq!(ev.signer_pubkey, ramp::<48>(0x50));
        assert_eq!(ev.partial, ramp::<48>(0x80));
        assert_eq!(ev.identity_sig, ramp::<96>(0xB0));
        assert_eq!(ev.commitment, commitment);

        let empty = InvalidSeedPartialEvidence::decode(&ipe1_sample(0)).unwrap();
        assert!(empty.commitment.is_empty());
    }

    #[test]
    fn ipe1_decode_reports_exact_reverts_in_check_order() {
        let mut short = ipe1_sample(0);
        short.pop();
        short[0] = b'X';
        assert_eq!(
            revert_reason(InvalidSeedPartialEvidence::decode(&short)),
            Some(format!(
                "IPE1 evidence too short: need at least {IPE1_PREFIX_LEN} bytes, got {}",
                IPE1_PREFIX_LEN - 1
            ))
        );

        let mut bad_magic = ipe1_sample(4);
        bad_magic[0] = b'X';
        bad_magic[4] = 0x09;
        assert_eq!(
            revert_reason(InvalidSeedPartialEvidence::decode(&bad_magic)),
            Some("IPE1 evidence bad magic".to_string())
        );

        let mut bad_version = ipe1_sample(4);
        bad_version[4] = 0x09;
        assert_eq!(
            revert_reason(InvalidSeedPartialEvidence::decode(&bad_version)),
            Some("IPE1 evidence bad version".to_string())
        );

        let mut overlong = ipe1_sample(4);
        overlong.truncate(IPE1_PREFIX_LEN + 3);
        assert_eq!(
            revert_reason(InvalidSeedPartialEvidence::decode(&overlong)),
            Some("IPE1 commitment length exceeds input".to_string())
        );

        let mut trailing = ipe1_sample(4);
        trailing.push(0);
        assert_eq!(
            revert_reason(InvalidSeedPartialEvidence::decode(&trailing)),
            Some("IPE1 evidence has trailing bytes".to_string())
        );
    }
}
