//! Pure verifier for AMD SEV-SNP attestation reports and the VCEK -> ASK -> ARK
//! certificate chain against the AMD roots pinned in the `sev` crate.
//!
//! Ported from `pso-snp-verify` + `pso-precompiles::snp_report_verify` so
//! `EnclaveRegistry` verifies reports natively inside Rust without an EVM
//! sub-precompile roundtrip.

use std::panic::{catch_unwind, AssertUnwindSafe};

use p384::ecdsa::signature::DigestVerifier as _;
use sev::certs::snp::builtin::{genoa, milan, turin};
use sev::certs::snp::{Certificate, Verifiable as _};
use sha2::{Digest as _, Sha384};
use x509_cert::der::asn1::ObjectIdentifier;
use x509_cert::der::Decode as _;

/// Length of an AMD SEV-SNP `ATTESTATION_REPORT`.
pub const REPORT_LEN: usize = 1184;

/// Maximum DER certificate length accepted for VCEK.
pub const MAX_CERT_LEN: usize = 4096;

/// Product id constants matching `EnclaveRegistry.sol` and `pso-precompiles`.
pub const PRODUCT_MILAN: u8 = 1;
pub const PRODUCT_GENOA: u8 = 2;
pub const PRODUCT_TURIN: u8 = 3;

const SIGNED_LEN: usize = 0x2a0;
const POLICY_OFFSET: usize = 0x08;
const VMPL_OFFSET: usize = 0x30;
const KEY_INFO_OFFSET: usize = 0x48;
const REPORT_DATA_OFFSET: usize = 0x50;
const MEASUREMENT_OFFSET: usize = 0x90;
const REPORTED_TCB_OFFSET: usize = 0x180;
const CHIP_ID_OFFSET: usize = 0x1a0;

const OID_BOOTLOADER_SPL: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.3704.1.3.1");
const OID_TEE_SPL: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.3704.1.3.2");
const OID_SNP_SPL: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.3704.1.3.3");
const OID_MICROCODE_SPL: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.3704.1.3.8");
const OID_FMC_SPL: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.3704.1.3.9");
const OID_HW_ID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.4.1.3704.1.4");

const SCALAR_LEN: usize = 48;
const SCALAR_FIELD_LEN: usize = 72;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    Milan,
    Genoa,
    Turin,
}

impl Product {
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            PRODUCT_MILAN => Some(Self::Milan),
            PRODUCT_GENOA => Some(Self::Genoa),
            PRODUCT_TURIN => Some(Self::Turin),
            _ => None,
        }
    }

    pub const fn id(self) -> u8 {
        match self {
            Self::Milan => PRODUCT_MILAN,
            Self::Genoa => PRODUCT_GENOA,
            Self::Turin => PRODUCT_TURIN,
        }
    }
}

/// Verified fields extracted from an AMD SEV-SNP report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSnpReport {
    pub version: u32,
    pub vmpl: u32,
    pub measurement: [u8; 48],
    pub report_data: [u8; 64],
    pub policy: u64,
    pub reported_tcb: u64,
    pub chip_id: [u8; 64],
    pub product: Product,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SnpVerifyError {
    #[error("malformed input: {0}")]
    Malformed(&'static str),
    #[error("unsupported report version {0}")]
    UnsupportedVersion(u32),
    #[error("unknown AMD product id {0}")]
    UnknownProduct(u8),
    #[error("certificate chain invalid: {0}")]
    ChainInvalid(&'static str),
    #[error("report signature does not verify under the VCEK")]
    SignatureInvalid,
    #[error("report is signed by a VLEK")]
    VlekRefused,
    #[error("REPORTED_TCB differs from the VCEK's TCB")]
    TcbMismatch,
    #[error("CHIP_ID is masked to zero")]
    ChipMasked,
    #[error("CHIP_ID differs from the VCEK's hwID")]
    ChipMismatch,
    #[error("certificate exceeds {MAX_CERT_LEN} bytes")]
    InputTooLarge,
}

/// Verifies `report` under `vcek_der` against the pinned AMD ASK/ARK for `product_id`.
///
/// Panic-guarded so untrusted evidence cannot unwind across the precompile boundary.
pub fn verify_snp_report(
    report: &[u8],
    vcek_der: &[u8],
    product_id: u8,
) -> Result<VerifiedSnpReport, SnpVerifyError> {
    catch_unwind(AssertUnwindSafe(|| {
        verify_unguarded(report, vcek_der, product_id)
    }))
    .unwrap_or(Err(SnpVerifyError::Malformed("panic")))
}

fn verify_unguarded(
    report: &[u8],
    vcek_der: &[u8],
    product_id: u8,
) -> Result<VerifiedSnpReport, SnpVerifyError> {
    let product = Product::from_id(product_id).ok_or(SnpVerifyError::UnknownProduct(product_id))?;
    let report: &[u8; REPORT_LEN] = report
        .try_into()
        .map_err(|_| SnpVerifyError::Malformed("report is not 1184 bytes"))?;
    if vcek_der.len() > MAX_CERT_LEN {
        return Err(SnpVerifyError::InputTooLarge);
    }

    let version = read_u32(report, 0);
    if !(2..=5).contains(&version) {
        return Err(SnpVerifyError::UnsupportedVersion(version));
    }
    check_signature_padding(report)?;

    match (read_u32(report, KEY_INFO_OFFSET) >> 2) & 0b111 {
        0 => {}
        1 => return Err(SnpVerifyError::VlekRefused),
        _ => return Err(SnpVerifyError::Malformed("KEY_INFO.SIGNING_KEY invalid")),
    }

    let (ask, ark) = pinned_pair(product)?;
    (&ark, &ask)
        .verify()
        .map_err(|_| SnpVerifyError::ChainInvalid("ARK does not sign ASK"))?;

    let vcek_x509 = x509_cert::Certificate::from_der(vcek_der)
        .map_err(|_| SnpVerifyError::ChainInvalid("VCEK does not parse"))?;
    if vcek_x509.signature_algorithm != vcek_x509.tbs_certificate.signature {
        return Err(SnpVerifyError::ChainInvalid(
            "VCEK signatureAlgorithm differs from tbsCertificate.signature",
        ));
    }
    if vcek_x509.signature.unused_bits() != 0 {
        return Err(SnpVerifyError::ChainInvalid(
            "VCEK signature BIT STRING has unused bits",
        ));
    }
    let vcek = Certificate::from(vcek_x509.clone());
    (&ask, &vcek)
        .verify()
        .map_err(|_| SnpVerifyError::ChainInvalid("ASK does not sign VCEK"))?;

    let key = p384::ecdsa::VerifyingKey::from_sec1_bytes(vcek.public_key_sec1())
        .map_err(|_| SnpVerifyError::ChainInvalid("VCEK public key is not a P-384 point"))?;
    let signature = report_signature(report)?;
    key.verify_digest(Sha384::new_with_prefix(&report[..SIGNED_LEN]), &signature)
        .map_err(|_| SnpVerifyError::SignatureInvalid)?;

    let reported_tcb = read_u64(report, REPORTED_TCB_OFFSET);
    let chip_id = read_array(report, CHIP_ID_OFFSET);
    check_vcek_binding(&vcek_x509, product, reported_tcb, &chip_id)?;

    Ok(VerifiedSnpReport {
        version,
        vmpl: read_u32(report, VMPL_OFFSET),
        measurement: read_array(report, MEASUREMENT_OFFSET),
        report_data: read_array(report, REPORT_DATA_OFFSET),
        policy: read_u64(report, POLICY_OFFSET),
        reported_tcb,
        chip_id,
        product,
    })
}

fn pinned_pair(product: Product) -> Result<(Certificate, Certificate), SnpVerifyError> {
    let (ask_res, ark_res) = match product {
        Product::Milan => (milan::ask(), milan::ark()),
        Product::Genoa => (genoa::ask(), genoa::ark()),
        Product::Turin => (turin::ask(), turin::ark()),
    };
    let ask = ask_res.map_err(|_| SnpVerifyError::ChainInvalid("pinned ASK does not parse"))?;
    let ark = ark_res.map_err(|_| SnpVerifyError::ChainInvalid("pinned ARK does not parse"))?;
    Ok((ask, ark))
}

fn check_vcek_binding(
    vcek: &x509_cert::Certificate,
    product: Product,
    reported_tcb: u64,
    chip_id: &[u8; 64],
) -> Result<(), SnpVerifyError> {
    let spl = |oid| {
        u8::from_der(vcek_extension(vcek, oid)?).map_err(|_| {
            SnpVerifyError::ChainInvalid("VCEK SPL extension is not a DER INTEGER 0..=255")
        })
    };
    let (bootloader, tee, snp, microcode) = (
        spl(OID_BOOTLOADER_SPL)?,
        spl(OID_TEE_SPL)?,
        spl(OID_SNP_SPL)?,
        spl(OID_MICROCODE_SPL)?,
    );
    let (tcb, hw_id_len) = match product {
        Product::Milan | Product::Genoa => ([bootloader, tee, 0, 0, 0, 0, snp, microcode], 64),
        Product::Turin => {
            let fmc = spl(OID_FMC_SPL)?;
            ([fmc, bootloader, tee, snp, 0, 0, 0, microcode], 8)
        }
    };
    if u64::from_le_bytes(tcb) != reported_tcb {
        return Err(SnpVerifyError::TcbMismatch);
    }

    let hw_id = vcek_extension(vcek, OID_HW_ID)?;
    if hw_id.len() != hw_id_len {
        return Err(SnpVerifyError::ChainInvalid(
            "VCEK hwID has the wrong length",
        ));
    }
    if chip_id.iter().all(|&byte| byte == 0) {
        return Err(SnpVerifyError::ChipMasked);
    }
    if chip_id[..hw_id_len] != *hw_id || chip_id[hw_id_len..].iter().any(|&byte| byte != 0) {
        return Err(SnpVerifyError::ChipMismatch);
    }
    Ok(())
}

fn vcek_extension(
    vcek: &x509_cert::Certificate,
    oid: ObjectIdentifier,
) -> Result<&[u8], SnpVerifyError> {
    let mut found = vcek
        .tbs_certificate
        .extensions
        .iter()
        .flatten()
        .filter(|ext| ext.extn_id == oid);
    match (found.next(), found.next()) {
        (Some(ext), None) => Ok(ext.extn_value.as_bytes()),
        _ => Err(SnpVerifyError::ChainInvalid(
            "VCEK lacks a TCB or hwID extension, or repeats one",
        )),
    }
}

fn check_signature_padding(report: &[u8; REPORT_LEN]) -> Result<(), SnpVerifyError> {
    let signature = &report[SIGNED_LEN..];
    let r_pad = &signature[SCALAR_LEN..SCALAR_FIELD_LEN];
    let s_pad = &signature[SCALAR_FIELD_LEN + SCALAR_LEN..2 * SCALAR_FIELD_LEN];
    let reserved = &signature[2 * SCALAR_FIELD_LEN..];
    if [r_pad, s_pad, reserved]
        .into_iter()
        .flatten()
        .any(|&byte| byte != 0)
    {
        return Err(SnpVerifyError::Malformed(
            "SIGNATURE field has nonzero padding",
        ));
    }
    Ok(())
}

fn report_signature(report: &[u8; REPORT_LEN]) -> Result<p384::ecdsa::Signature, SnpVerifyError> {
    let mut r: [u8; SCALAR_LEN] = read_array(report, SIGNED_LEN);
    let mut s: [u8; SCALAR_LEN] = read_array(report, SIGNED_LEN + SCALAR_FIELD_LEN);
    r.reverse();
    s.reverse();
    p384::ecdsa::Signature::from_scalars(r, s).map_err(|_| SnpVerifyError::SignatureInvalid)
}

fn read_array<const N: usize>(report: &[u8; REPORT_LEN], offset: usize) -> [u8; N] {
    let mut bytes = [0u8; N];
    bytes.copy_from_slice(&report[offset..offset + N]);
    bytes
}

fn read_u32(report: &[u8; REPORT_LEN], offset: usize) -> u32 {
    u32::from_le_bytes(read_array(report, offset))
}

fn read_u64(report: &[u8; REPORT_LEN], offset: usize) -> u64 {
    u64::from_le_bytes(read_array(report, offset))
}
