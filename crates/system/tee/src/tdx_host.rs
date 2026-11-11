//! Node-side assembly of the Intel collateral that accompanies a TDX quote in
//! registration evidence.
//!
//! Quote verification is deterministic and never uses the network. The sender
//! of the evidence therefore supplies the eight collateral components. This
//! module takes them from two places:
//!
//! 1. the PCK certificate chain: from the quote itself;
//! 2. the other seven components: from the file `collateral.json` in the
//!    directory that [`TDX_COLLATERAL_DIR_ENV`] names.
//!
//! An operator downloads the collateral from the Intel provisioning
//! certification service for the processor of the VM and refreshes it before
//! it expires. The file has these string fields:
//!
//! | Field | Content |
//! |---|---|
//! | `pck_crl_issuer_chain`, `tcb_info_issuer_chain`, `qe_identity_issuer_chain` | PEM text |
//! | `root_ca_crl`, `pck_crl` | DER as hexadecimal text |
//! | `tcb_info`, `qe_identity` | JSON text of the signed body |
//! | `tcb_info_signature`, `qe_identity_signature` | hexadecimal text |
//!
//! This module checks the shape of the components only. The service that
//! receives the evidence verifies the quote and the collateral.

use std::path::{Path, PathBuf};

use outbe_primitives::tee_attestation_v1::{
    DcapCollateralComponentV1, DcapCollateralKind, MAX_COLLATERAL_COMPONENT_BYTES, MAX_QUOTE_BYTES,
};
use serde::Deserialize;

use crate::tdx_report::{tdx_quote_canonical_len, tdx_quote_pck_certification_data};

/// Environment variable that names the directory with `collateral.json`.
pub const TDX_COLLATERAL_DIR_ENV: &str = "OUTBE_TDX_COLLATERAL_DIR";

const COLLATERAL_FILE: &str = "collateral.json";
/// Upper bound on the collateral file. The PCK CRL is the largest part.
const MAX_COLLATERAL_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Certification data type of a PCK certificate chain.
const PCK_CERTIFICATE_CHAIN_TYPE: u16 = 5;
const PEM_BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----\n";

/// A TDX quote in its canonical form with its eight collateral components.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TdxHostEvidenceV1 {
    /// The quote without bytes after its signature data.
    pub quote: Vec<u8>,
    pub components: Vec<DcapCollateralComponentV1>,
}

/// Failure to assemble the evidence parts of a TDX quote.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TdxHostError {
    #[error("TDX quote is malformed")]
    QuoteMalformed,
    #[error("TDX quote is {actual} bytes, the limit is {limit}")]
    QuoteTooLarge { actual: usize, limit: usize },
    #[error("TDX quote does not carry a PCK certificate chain (certification data type {0})")]
    NoPckCertificateChain(u16),
    #[error("PCK certificate chain of the TDX quote is not canonical PEM text")]
    NonCanonicalPckCertificateChain,
    #[error(
        "{TDX_COLLATERAL_DIR_ENV} is not set: it must name the directory with collateral.json"
    )]
    CollateralDirectoryNotSet,
    #[error("cannot read {path}: {reason}")]
    ReadCollateral { path: String, reason: String },
    #[error("{path} is larger than {limit} bytes")]
    CollateralTooLarge { path: String, limit: u64 },
    #[error("{path} is not a collateral file: {reason}")]
    ParseCollateral { path: String, reason: String },
    #[error("collateral field {field} is empty or too large")]
    ComponentLength { field: &'static str },
    #[error("collateral field {field} is not hexadecimal text")]
    ComponentNotHex { field: &'static str },
    #[error("collateral field {field} contains a zero byte")]
    ComponentContainsNul { field: &'static str },
}

#[derive(Deserialize)]
struct CollateralFile {
    pck_crl_issuer_chain: String,
    root_ca_crl: String,
    pck_crl: String,
    tcb_info_issuer_chain: String,
    tcb_info: String,
    tcb_info_signature: String,
    qe_identity_issuer_chain: String,
    qe_identity: String,
    qe_identity_signature: String,
}

/// Assemble the canonical quote and the collateral components for `quote`.
///
/// The collateral comes from the directory that [`TDX_COLLATERAL_DIR_ENV`]
/// names.
pub fn acquire_tdx_collateral_v1(quote: &[u8]) -> Result<TdxHostEvidenceV1, TdxHostError> {
    let directory = std::env::var_os(TDX_COLLATERAL_DIR_ENV)
        .map(PathBuf::from)
        .ok_or(TdxHostError::CollateralDirectoryNotSet)?;
    acquire_tdx_collateral_from(quote, &directory)
}

/// [`acquire_tdx_collateral_v1`] with an explicit collateral directory.
pub fn acquire_tdx_collateral_from(
    quote: &[u8],
    directory: &Path,
) -> Result<TdxHostEvidenceV1, TdxHostError> {
    let quote = canonical_quote(quote)?;
    let pck_chain = pck_certificate_chain(&quote)?;
    let path = directory.join(COLLATERAL_FILE);
    let file = read_collateral_file(&path)?;
    let components = components(pck_chain, &file)?;
    Ok(TdxHostEvidenceV1 { quote, components })
}

/// Cut the zero bytes that can follow the signature data of a quote.
fn canonical_quote(quote: &[u8]) -> Result<Vec<u8>, TdxHostError> {
    let len = tdx_quote_canonical_len(quote).map_err(|_| TdxHostError::QuoteMalformed)?;
    if len > MAX_QUOTE_BYTES {
        return Err(TdxHostError::QuoteTooLarge {
            actual: len,
            limit: MAX_QUOTE_BYTES,
        });
    }
    Ok(quote[..len].to_vec())
}

/// Copy the PCK certificate chain exactly as the quote carries it. The
/// verifier compares the component with the quote byte for byte.
fn pck_certificate_chain(quote: &[u8]) -> Result<Vec<u8>, TdxHostError> {
    let (kind, chain) =
        tdx_quote_pck_certification_data(quote).map_err(|_| TdxHostError::QuoteMalformed)?;
    if kind != PCK_CERTIFICATE_CHAIN_TYPE {
        return Err(TdxHostError::NoPckCertificateChain(kind));
    }
    let text_ends_with_nul = chain.last() == Some(&0);
    let text = if text_ends_with_nul {
        &chain[..chain.len() - 1]
    } else {
        chain
    };
    if !text.starts_with(PEM_BEGIN) || text.contains(&0) {
        return Err(TdxHostError::NonCanonicalPckCertificateChain);
    }
    Ok(chain.to_vec())
}

fn read_collateral_file(path: &Path) -> Result<CollateralFile, TdxHostError> {
    let display = path.display().to_string();
    let metadata = std::fs::metadata(path).map_err(|error| TdxHostError::ReadCollateral {
        path: display.clone(),
        reason: error.to_string(),
    })?;
    if metadata.len() > MAX_COLLATERAL_FILE_BYTES {
        return Err(TdxHostError::CollateralTooLarge {
            path: display,
            limit: MAX_COLLATERAL_FILE_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(|error| TdxHostError::ReadCollateral {
        path: display.clone(),
        reason: error.to_string(),
    })?;
    serde_json::from_slice(&bytes).map_err(|error| TdxHostError::ParseCollateral {
        path: display,
        reason: error.to_string(),
    })
}

fn components(
    pck_chain: Vec<u8>,
    file: &CollateralFile,
) -> Result<Vec<DcapCollateralComponentV1>, TdxHostError> {
    let components = vec![
        (DcapCollateralKind::PckCertificateChain, pck_chain),
        (
            DcapCollateralKind::PckCrl,
            hex_der("pck_crl", &file.pck_crl)?,
        ),
        (
            DcapCollateralKind::PckCrlIssuerChain,
            text("pck_crl_issuer_chain", &file.pck_crl_issuer_chain)?,
        ),
        (
            DcapCollateralKind::RootCaCrl,
            hex_der("root_ca_crl", &file.root_ca_crl)?,
        ),
        (
            DcapCollateralKind::TcbInfo,
            signed_document(
                "tcbInfo",
                text("tcb_info", &file.tcb_info)?,
                text("tcb_info_signature", &file.tcb_info_signature)?,
            ),
        ),
        (
            DcapCollateralKind::TcbInfoIssuerChain,
            text("tcb_info_issuer_chain", &file.tcb_info_issuer_chain)?,
        ),
        (
            DcapCollateralKind::QeIdentity,
            signed_document(
                "enclaveIdentity",
                text("qe_identity", &file.qe_identity)?,
                text("qe_identity_signature", &file.qe_identity_signature)?,
            ),
        ),
        (
            DcapCollateralKind::QeIdentityIssuerChain,
            text("qe_identity_issuer_chain", &file.qe_identity_issuer_chain)?,
        ),
    ];
    components
        .into_iter()
        .map(|(kind, bytes)| {
            if bytes.is_empty() || bytes.len() > MAX_COLLATERAL_COMPONENT_BYTES {
                return Err(TdxHostError::ComponentLength {
                    field: "assembled component",
                });
            }
            Ok(DcapCollateralComponentV1 { kind, bytes })
        })
        .collect()
}

/// The envelope that the Intel service signs over: the body and its signature.
fn signed_document(field: &str, body: Vec<u8>, signature: Vec<u8>) -> Vec<u8> {
    let mut document = Vec::with_capacity(field.len() + body.len() + signature.len() + 24);
    document.extend_from_slice(b"{\"");
    document.extend_from_slice(field.as_bytes());
    document.extend_from_slice(b"\":");
    document.extend_from_slice(&body);
    document.extend_from_slice(b",\"signature\":\"");
    document.extend_from_slice(&signature);
    document.extend_from_slice(b"\"}");
    document
}

fn text(field: &'static str, value: &str) -> Result<Vec<u8>, TdxHostError> {
    if value.is_empty() || value.len() > MAX_COLLATERAL_COMPONENT_BYTES {
        return Err(TdxHostError::ComponentLength { field });
    }
    if value.as_bytes().contains(&0) {
        return Err(TdxHostError::ComponentContainsNul { field });
    }
    Ok(value.as_bytes().to_vec())
}

fn hex_der(field: &'static str, value: &str) -> Result<Vec<u8>, TdxHostError> {
    let value = value.as_bytes();
    if value.is_empty() || value.len() > 2 * MAX_COLLATERAL_COMPONENT_BYTES {
        return Err(TdxHostError::ComponentLength { field });
    }
    if !value.len().is_multiple_of(2) {
        return Err(TdxHostError::ComponentNotHex { field });
    }
    value
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte: u8| (byte as char).to_digit(16);
            match (digit(pair[0]), digit(pair[1])) {
                (Some(high), Some(low)) => Ok((high * 16 + low) as u8),
                _ => Err(TdxHostError::ComponentNotHex { field }),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUOTE_FILE: &[u8] =
        include_bytes!("../tests/fixtures/intel-tdx-quote-v4/tdx-quote-v4.bin");
    const COLLATERAL: &[u8] =
        include_bytes!("../tests/fixtures/intel-tdx-quote-v4/tdx-collateral-wrapper.json");
    /// Length of the fixture quote without the zero bytes after its signature
    /// data.
    const QUOTE_BYTES: usize = 4936;

    fn directory_with(collateral: &[u8]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(COLLATERAL_FILE), collateral).unwrap();
        directory
    }

    #[test]
    fn assembles_the_canonical_quote_and_eight_components() {
        let directory = directory_with(COLLATERAL);
        let evidence = acquire_tdx_collateral_from(QUOTE_FILE, directory.path()).unwrap();

        // The zero bytes after the signature data are cut.
        assert_eq!(evidence.quote, QUOTE_FILE[..QUOTE_BYTES]);
        assert_eq!(evidence.components.len(), 8);
        for (index, component) in evidence.components.iter().enumerate() {
            assert_eq!(component.kind as u8, index as u8 + 1);
            assert!(!component.bytes.is_empty());
        }
        // The PCK chain is the tail of the quote, byte for byte.
        let pck_chain = &evidence.components[0].bytes;
        assert!(evidence.quote.ends_with(pck_chain));
        assert!(pck_chain.starts_with(PEM_BEGIN));
        // CRLs are DER: an ASN.1 SEQUENCE.
        assert_eq!(evidence.components[1].bytes[0], 0x30);
        assert_eq!(evidence.components[3].bytes[0], 0x30);
        assert!(evidence.components[4]
            .bytes
            .starts_with(br#"{"tcbInfo":{"id":"TDX""#));
        assert!(evidence.components[6]
            .bytes
            .starts_with(br#"{"enclaveIdentity":{"id":"TD_QE""#));
        assert!(evidence.components[4].bytes.ends_with(b"\"}"));
    }

    #[test]
    fn a_canonical_quote_is_taken_unchanged() {
        let directory = directory_with(COLLATERAL);
        let evidence =
            acquire_tdx_collateral_from(&QUOTE_FILE[..QUOTE_BYTES], directory.path()).unwrap();
        assert_eq!(evidence.quote, QUOTE_FILE[..QUOTE_BYTES]);
    }

    #[test]
    fn malformed_quotes_are_rejected() {
        let directory = directory_with(COLLATERAL);
        for quote in [&QUOTE_FILE[..QUOTE_BYTES - 1], &QUOTE_FILE[..100], &[][..]] {
            assert_eq!(
                acquire_tdx_collateral_from(quote, directory.path()),
                Err(TdxHostError::QuoteMalformed)
            );
        }
        let mut data_after_the_quote = QUOTE_FILE.to_vec();
        *data_after_the_quote.last_mut().unwrap() = 1;
        assert_eq!(
            acquire_tdx_collateral_from(&data_after_the_quote, directory.path()),
            Err(TdxHostError::QuoteMalformed)
        );
    }

    #[test]
    fn missing_or_broken_collateral_file_is_an_error() {
        let empty = tempfile::tempdir().unwrap();
        assert!(matches!(
            acquire_tdx_collateral_from(QUOTE_FILE, empty.path()),
            Err(TdxHostError::ReadCollateral { .. })
        ));

        let not_json = directory_with(b"not json");
        assert!(matches!(
            acquire_tdx_collateral_from(QUOTE_FILE, not_json.path()),
            Err(TdxHostError::ParseCollateral { .. })
        ));

        let mut value: serde_json::Value = serde_json::from_slice(COLLATERAL).unwrap();
        value["pck_crl"] = "zz".into();
        let bad_hex = directory_with(&serde_json::to_vec(&value).unwrap());
        assert_eq!(
            acquire_tdx_collateral_from(QUOTE_FILE, bad_hex.path()),
            Err(TdxHostError::ComponentNotHex { field: "pck_crl" })
        );

        value["pck_crl"] = "".into();
        let empty_field = directory_with(&serde_json::to_vec(&value).unwrap());
        assert_eq!(
            acquire_tdx_collateral_from(QUOTE_FILE, empty_field.path()),
            Err(TdxHostError::ComponentLength { field: "pck_crl" })
        );
    }
}
