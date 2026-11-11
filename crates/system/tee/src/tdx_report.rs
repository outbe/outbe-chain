//! TD report body of an Intel TDX quote v4.
//!
//! This module reads the fields that the attestation policy binds. It does not
//! verify signatures: that is the work of the native QVL. It has no native
//! dependency, so the service in a confidential VM and the node use it too.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{tdx_image_rule_value, tdx_mrconfigid_rule_value};

use crate::byte_cursor::ByteCursor;
use crate::dcap_protocol::DcapRejectCodeV1;

/// Offset of the TD report body: it follows the 48-byte quote header.
const TD_REPORT_BODY_OFFSET: usize = 48;
/// Length of the TD report body of a quote v4 (TDX 1.0 layout).
const TD_REPORT_BODY_BYTES: usize = 584;
/// Offset of the length of the signature data.
const SIGNATURE_DATA_LENGTH_OFFSET: usize = TD_REPORT_BODY_OFFSET + TD_REPORT_BODY_BYTES;

/// Length of a TDX quote v4 that ends with its signature data.
///
/// The kernel can return a quote in a larger buffer with zero bytes after the
/// signature data. Canonical evidence carries the quote without those bytes.
pub fn tdx_quote_canonical_len(quote: &[u8]) -> Result<usize, DcapRejectCodeV1> {
    let declared = quote
        .get(SIGNATURE_DATA_LENGTH_OFFSET..SIGNATURE_DATA_LENGTH_OFFSET + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(DcapRejectCodeV1::QuoteMalformed)?;
    let len = usize::try_from(declared)
        .ok()
        .and_then(|declared| (SIGNATURE_DATA_LENGTH_OFFSET + 4).checked_add(declared))
        .ok_or(DcapRejectCodeV1::QuoteMalformed)?;
    if len > quote.len() || quote[len..].iter().any(|byte| *byte != 0) {
        return Err(DcapRejectCodeV1::QuoteMalformed);
    }
    Ok(len)
}
const TD_ATTRIBUTES: usize = 120;
const MRTD: usize = 136;
const MRCONFIGID: usize = 184;
const RTMR0: usize = 328;
const REPORT_DATA: usize = 520;
/// `TD_ATTRIBUTES.TUD.DEBUG`: the host can read the memory of the TD.
const TD_ATTRIBUTE_DEBUG: u8 = 0x01;

const QUOTE_SIGNATURE_BYTES: usize = 64;
const ATTESTATION_PUBLIC_KEY_BYTES: usize = 64;
const QE_REPORT_BYTES: usize = 384;
const QE_REPORT_SIGNATURE_BYTES: usize = 64;
/// Certification data type "QE report certification data" of a TDX quote.
const QE_REPORT_CERTIFICATION_DATA_TYPE: u16 = 6;

/// Read the PCK certification data of a TDX quote v4 that ends with its
/// signature data.
///
/// The outer certification data must have type 6 and must end with the
/// signature data. The returned type and bytes are those of the nested
/// certification data. Type 5 is a PCK certificate chain.
pub fn tdx_quote_pck_certification_data(quote: &[u8]) -> Result<(u16, &[u8]), DcapRejectCodeV1> {
    let mut cursor = ByteCursor::new(
        quote
            .get(SIGNATURE_DATA_LENGTH_OFFSET + 4..)
            .ok_or(DcapRejectCodeV1::QuoteMalformed)?,
        |_| DcapRejectCodeV1::QuoteMalformed,
    );
    cursor.take(QUOTE_SIGNATURE_BYTES)?;
    cursor.take(ATTESTATION_PUBLIC_KEY_BYTES)?;
    let outer_type = u16::from_le_bytes(cursor.array()?);
    let outer_len = usize::try_from(u32::from_le_bytes(cursor.array()?))
        .map_err(|_| DcapRejectCodeV1::QuoteMalformed)?;
    let outer = cursor.take(outer_len)?;
    cursor.finish()?;
    if outer_type != QE_REPORT_CERTIFICATION_DATA_TYPE {
        return Err(DcapRejectCodeV1::QuoteMalformed);
    }

    let mut cursor = ByteCursor::new(outer, |_| DcapRejectCodeV1::QuoteMalformed);
    cursor.take(QE_REPORT_BYTES)?;
    cursor.take(QE_REPORT_SIGNATURE_BYTES)?;
    let qe_authentication_data_len = usize::from(u16::from_le_bytes(cursor.array()?));
    cursor.take(qe_authentication_data_len)?;
    let certification_data_type = u16::from_le_bytes(cursor.array()?);
    let certification_data_len = usize::try_from(u32::from_le_bytes(cursor.array()?))
        .map_err(|_| DcapRejectCodeV1::QuoteMalformed)?;
    let certification_data = cursor.take(certification_data_len)?;
    cursor.finish()?;
    Ok((certification_data_type, certification_data))
}

/// Fields of a TD report body that identify the TD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TdReportMeasurements {
    pub td_attributes: [u8; 8],
    /// Build-time measurement of the TD.
    pub mrtd: [u8; 48],
    /// Configuration identity that the VMM sets at launch.
    pub mr_config_id: [u8; 48],
    /// Runtime measurement registers 0, 1 and 2.
    pub rtmrs: [[u8; 48]; 3],
    pub report_data: [u8; 64],
}

impl TdReportMeasurements {
    /// Returns `true` when the TD runs in debug mode.
    #[must_use]
    pub const fn is_debug(&self) -> bool {
        (self.td_attributes[0] & TD_ATTRIBUTE_DEBUG) != 0
    }

    /// Projection of the image measurements into the `mrenclave` rule field.
    #[must_use]
    pub fn image_rule_value(&self) -> B256 {
        tdx_image_rule_value(&self.mrtd, &self.rtmrs)
    }

    /// Projection of `MRCONFIGID` into the `mrsigner` rule field.
    #[must_use]
    pub fn mrconfigid_rule_value(&self) -> B256 {
        tdx_mrconfigid_rule_value(&self.mr_config_id)
    }
}

/// Read the TD report body of a TDX quote v4.
pub fn parse_tdx_quote_measurements(
    quote: &[u8],
) -> Result<TdReportMeasurements, DcapRejectCodeV1> {
    let body = quote
        .get(TD_REPORT_BODY_OFFSET..TD_REPORT_BODY_OFFSET + TD_REPORT_BODY_BYTES)
        .ok_or(DcapRejectCodeV1::QuoteMalformed)?;
    let rtmr = |index: usize| field::<48>(body, RTMR0 + index * 48);
    Ok(TdReportMeasurements {
        td_attributes: field(body, TD_ATTRIBUTES)?,
        mrtd: field(body, MRTD)?,
        mr_config_id: field(body, MRCONFIGID)?,
        rtmrs: [rtmr(0)?, rtmr(1)?, rtmr(2)?],
        report_data: field(body, REPORT_DATA)?,
    })
}

fn field<const N: usize>(body: &[u8], offset: usize) -> Result<[u8; N], DcapRejectCodeV1> {
    body.get(offset..offset + N)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(DcapRejectCodeV1::QuoteMalformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote_with_body(body: &[u8; TD_REPORT_BODY_BYTES]) -> Vec<u8> {
        let mut quote = vec![0; TD_REPORT_BODY_OFFSET];
        quote.extend_from_slice(body);
        quote
    }

    #[test]
    fn reads_fields_at_the_td_report_offsets() {
        let mut body = [0_u8; TD_REPORT_BODY_BYTES];
        body[MRTD..MRTD + 48].fill(0x11);
        body[MRCONFIGID..MRCONFIGID + 48].fill(0x22);
        body[RTMR0..RTMR0 + 48].fill(0x30);
        body[RTMR0 + 48..RTMR0 + 96].fill(0x31);
        body[RTMR0 + 96..RTMR0 + 144].fill(0x32);
        // RTMR3 is not part of the identity.
        body[RTMR0 + 144..RTMR0 + 192].fill(0x33);
        body[REPORT_DATA..].fill(0x44);

        let measurements = parse_tdx_quote_measurements(&quote_with_body(&body)).unwrap();
        assert_eq!(measurements.mrtd, [0x11; 48]);
        assert_eq!(measurements.mr_config_id, [0x22; 48]);
        assert_eq!(measurements.rtmrs, [[0x30; 48], [0x31; 48], [0x32; 48]]);
        assert_eq!(measurements.report_data, [0x44; 64]);
        assert!(!measurements.is_debug());

        let mut other_rtmr3 = body;
        other_rtmr3[RTMR0 + 144..RTMR0 + 192].fill(0x99);
        let other = parse_tdx_quote_measurements(&quote_with_body(&other_rtmr3)).unwrap();
        assert_eq!(other.image_rule_value(), measurements.image_rule_value());
    }

    #[test]
    fn debug_attribute_is_bit_zero_of_td_attributes() {
        let mut body = [0_u8; TD_REPORT_BODY_BYTES];
        body[TD_ATTRIBUTES] = TD_ATTRIBUTE_DEBUG;
        assert!(parse_tdx_quote_measurements(&quote_with_body(&body))
            .unwrap()
            .is_debug());
    }

    #[test]
    fn canonical_length_cuts_zero_padding_only() {
        let mut quote = quote_with_body(&[0; TD_REPORT_BODY_BYTES]);
        quote.extend_from_slice(&3_u32.to_le_bytes());
        quote.extend_from_slice(&[7, 8, 9]);
        let canonical = quote.len();
        assert_eq!(tdx_quote_canonical_len(&quote), Ok(canonical));

        quote.extend_from_slice(&[0; 70]);
        assert_eq!(tdx_quote_canonical_len(&quote), Ok(canonical));

        *quote.last_mut().unwrap() = 1;
        assert_eq!(
            tdx_quote_canonical_len(&quote),
            Err(DcapRejectCodeV1::QuoteMalformed)
        );
        assert_eq!(
            tdx_quote_canonical_len(&quote[..canonical - 1]),
            Err(DcapRejectCodeV1::QuoteMalformed)
        );
    }

    #[test]
    fn short_quote_is_malformed() {
        let quote = vec![0; TD_REPORT_BODY_OFFSET + TD_REPORT_BODY_BYTES - 1];
        assert_eq!(
            parse_tdx_quote_measurements(&quote),
            Err(DcapRejectCodeV1::QuoteMalformed)
        );
    }
}
