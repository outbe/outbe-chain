//! DCAP (ECDSA) SGX quote measurement parsing shared by the enclave and host.
//!
//! This module models only the layout needed to extract
//! MRENCLAVE/MRSIGNER/ISVSVN/report_data. This module parses fields. Cryptographic
//! quote verification belongs to the enclave-resident native QVL path.

/// SGX report body offset inside a DCAP ECDSA quote (after the 48-byte header).
/// A standalone local SGX report has its body at offset 0. Prepend this many bytes
/// to it, and [`parse_quote_measurements`] can parse it at the same offsets.
pub const REPORT_BODY_OFFSET: usize = 48;
const RB_MISC_SELECT: usize = 16;
const RB_ATTRIBUTES: usize = 48;
const RB_MRENCLAVE: usize = 64;
const RB_MRSIGNER: usize = 128;
const RB_ISV_PROD_ID: usize = 256;
const RB_ISV_SVN: usize = 258;
const RB_REPORT_DATA: usize = 320;
/// Minimum quote length to contain a full report body + report_data.
pub const MIN_QUOTE_LEN: usize = REPORT_BODY_OFFSET + RB_REPORT_DATA + 64;

/// SGX `ATTRIBUTES.FLAGS` bit 1 (`0x02`): enclave runs in debug mode (`SGX_FLAGS_DEBUG`).
pub const SGX_FLAGS_DEBUG: u8 = 0x02;

/// Measurements parsed out of a DCAP quote's embedded SGX report body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportMeasurements {
    pub misc_select: u32,
    pub attributes: [u8; 16],
    pub mrenclave: [u8; 32],
    pub mrsigner: [u8; 32],
    pub isv_prod_id: u16,
    pub isv_svn: u16,
    pub report_data: [u8; 64],
}

impl ReportMeasurements {
    /// Returns `true` if `SGX_FLAGS_DEBUG` (`0x02`) is set in `ATTRIBUTES.FLAGS`.
    #[must_use]
    pub const fn is_debug(&self) -> bool {
        (self.attributes[0] & SGX_FLAGS_DEBUG) != 0
    }
}

use crate::errors::QuoteParseError;

/// Parse MRENCLAVE/MRSIGNER/ISVSVN/report_data from a real DCAP quote.
pub fn parse_quote_measurements(quote: &[u8]) -> Result<ReportMeasurements, QuoteParseError> {
    if quote.len() < MIN_QUOTE_LEN {
        return Err(QuoteParseError::TooShort {
            actual: quote.len(),
            min: MIN_QUOTE_LEN,
        });
    }
    let base = REPORT_BODY_OFFSET;
    let misc_select = u32::from_le_bytes([
        quote[base + RB_MISC_SELECT],
        quote[base + RB_MISC_SELECT + 1],
        quote[base + RB_MISC_SELECT + 2],
        quote[base + RB_MISC_SELECT + 3],
    ]);
    let mut attributes = [0u8; 16];
    attributes.copy_from_slice(&quote[base + RB_ATTRIBUTES..base + RB_ATTRIBUTES + 16]);
    let mut mrenclave = [0u8; 32];
    mrenclave.copy_from_slice(&quote[base + RB_MRENCLAVE..base + RB_MRENCLAVE + 32]);
    let mut mrsigner = [0u8; 32];
    mrsigner.copy_from_slice(&quote[base + RB_MRSIGNER..base + RB_MRSIGNER + 32]);
    let isv_prod_id = u16::from_le_bytes([
        quote[base + RB_ISV_PROD_ID],
        quote[base + RB_ISV_PROD_ID + 1],
    ]);
    let isv_svn = u16::from_le_bytes([quote[base + RB_ISV_SVN], quote[base + RB_ISV_SVN + 1]]);
    let mut report_data = [0u8; 64];
    report_data.copy_from_slice(&quote[base + RB_REPORT_DATA..base + RB_REPORT_DATA + 64]);
    Ok(ReportMeasurements {
        misc_select,
        attributes,
        mrenclave,
        mrsigner,
        isv_prod_id,
        isv_svn,
        report_data,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic quote with known measurement bytes parses at the right offsets.
    #[test]
    fn parses_measurements_at_sgx_offsets() {
        let mut q = vec![0u8; MIN_QUOTE_LEN];
        let base = REPORT_BODY_OFFSET;
        q[base + RB_MISC_SELECT..base + RB_MISC_SELECT + 4]
            .copy_from_slice(&0x1122_3344_u32.to_le_bytes());
        q[base + RB_ATTRIBUTES] = 0x05;
        q[base + RB_MRENCLAVE..base + RB_MRENCLAVE + 32].copy_from_slice(&[0xAA; 32]);
        q[base + RB_MRSIGNER..base + RB_MRSIGNER + 32].copy_from_slice(&[0xBB; 32]);
        q[base + RB_ISV_PROD_ID] = 3;
        q[base + RB_ISV_SVN] = 7;
        q[base + RB_REPORT_DATA..base + RB_REPORT_DATA + 32].copy_from_slice(&[0xCC; 32]);
        let m = parse_quote_measurements(&q).unwrap();
        assert_eq!(m.misc_select, 0x1122_3344);
        assert_eq!(m.attributes[0], 0x05);
        assert!(!m.is_debug());
        assert_eq!(m.mrenclave, [0xAA; 32]);
        assert_eq!(m.mrsigner, [0xBB; 32]);
        assert_eq!(m.isv_prod_id, 3);
        assert_eq!(m.isv_svn, 7);
        assert_eq!(&m.report_data[..32], &[0xCC; 32]);

        q[base + RB_ATTRIBUTES] |= SGX_FLAGS_DEBUG;
        let debug_m = parse_quote_measurements(&q).unwrap();
        assert!(debug_m.is_debug());
    }

    #[test]
    fn rejects_short_quote() {
        assert!(parse_quote_measurements(&[0u8; 100]).is_err());
    }
}
