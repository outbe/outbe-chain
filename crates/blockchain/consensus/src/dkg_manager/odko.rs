//! The ODKO boundary-outcome record: the public DKG output a boundary artifact
//! carries, with the epoch it activates and whether it came from a full DKG.
//!
//! Wire layout: `"ODKO" || version(1) || epoch(8 BE) || is_full_dkg(1) ||
//! output_len(4 BE) || Output`. One encoder and one strict decoder serve the
//! proposer, the follower, the enclave and the engine's durable snapshots.

use std::{fmt, num::NonZeroU32};

use alloy_primitives::Bytes;
use commonware_codec::Read as _;
use commonware_consensus::types::Epoch;
use commonware_cryptography::bls12381::{
    self,
    dkg::feldman_desmedt::Output,
    primitives::{sharing::ModeVersion, variant::MinSig},
};

const MAGIC: &[u8; 4] = b"ODKO";
const VERSION: u8 = 0x02;
const HEADER_LEN: usize = 4 + 1 + 8 + 1 + 4;

/// A decoded ODKO boundary outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OdkoOutcome {
    pub epoch: Epoch,
    pub is_full_dkg: bool,
    pub output: Output<MinSig, bls12381::PublicKey>,
}

/// Why bytes are not a canonical ODKO record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OdkoDecodeError {
    TooShort(usize),
    InvalidMagic,
    UnsupportedVersion(u8),
    InvalidFullDkgFlag(u8),
    LengthMismatch { declared: u64, actual: u64 },
    InvalidOutput(String),
    TrailingOutputBytes,
}

impl fmt::Display for OdkoDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort(len) => {
                write!(f, "DKG boundary outcome too short: {len} < {HEADER_LEN}")
            }
            Self::InvalidMagic => f.write_str("DKG boundary outcome has invalid magic"),
            Self::UnsupportedVersion(version) => {
                write!(f, "DKG boundary outcome version {version} is unsupported")
            }
            Self::InvalidFullDkgFlag(flag) => {
                write!(f, "DKG boundary outcome full-DKG flag {flag} is not 0 or 1")
            }
            Self::LengthMismatch { declared, actual } => write!(
                f,
                "DKG boundary outcome length mismatch: declared output {declared} bytes, carried {actual}"
            ),
            Self::InvalidOutput(error) => write!(f, "invalid DKG output in boundary outcome: {error}"),
            Self::TrailingOutputBytes => f.write_str("trailing bytes after DKG output in boundary outcome"),
        }
    }
}

impl std::error::Error for OdkoDecodeError {}

impl OdkoOutcome {
    pub fn encode(&self) -> Bytes {
        let output_bytes = commonware_codec::Encode::encode(&self.output);
        let mut buf = Vec::with_capacity(HEADER_LEN + output_bytes.len());
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&self.epoch.get().to_be_bytes());
        buf.push(u8::from(self.is_full_dkg));
        // An `Output` holds at most `MAX_VALIDATORS` (256) players and a
        // polynomial of that degree: a few tens of KiB, far below `u32::MAX`.
        buf.extend_from_slice(&(output_bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(output_bytes.as_ref());
        Bytes::from(buf)
    }

    /// Decode a canonical ODKO record. Deterministic and panic-free.
    pub fn decode(bytes: &[u8]) -> Result<Self, OdkoDecodeError> {
        if bytes.len() < HEADER_LEN {
            return Err(OdkoDecodeError::TooShort(bytes.len()));
        }
        if &bytes[0..4] != MAGIC {
            return Err(OdkoDecodeError::InvalidMagic);
        }
        if bytes[4] != VERSION {
            return Err(OdkoDecodeError::UnsupportedVersion(bytes[4]));
        }
        let mut epoch = [0u8; 8];
        epoch.copy_from_slice(&bytes[5..13]);
        let is_full_dkg = match bytes[13] {
            0 => false,
            1 => true,
            flag => return Err(OdkoDecodeError::InvalidFullDkgFlag(flag)),
        };
        let mut len = [0u8; 4];
        len.copy_from_slice(&bytes[14..HEADER_LEN]);
        let declared = u64::from(u32::from_be_bytes(len));
        let mut reader = &bytes[HEADER_LEN..];
        let actual = reader.len() as u64;
        if declared != actual {
            return Err(OdkoDecodeError::LengthMismatch { declared, actual });
        }
        let max = NonZeroU32::new(crate::bls::MAX_VALIDATORS)
            .ok_or_else(|| OdkoDecodeError::InvalidOutput("zero validator cap".into()))?;
        let output =
            Output::<MinSig, bls12381::PublicKey>::read_cfg(&mut reader, &(max, ModeVersion::v0()))
                .map_err(|error| OdkoDecodeError::InvalidOutput(error.to_string()))?;
        if !reader.is_empty() {
            return Err(OdkoDecodeError::TrailingOutputBytes);
        }
        Ok(Self {
            epoch: Epoch::new(u64::from_be_bytes(epoch)),
            is_full_dkg,
            output,
        })
    }
}
