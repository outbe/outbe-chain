//! PCK certificate chain grammar for DCAP V1.
//!
//! This module checks the canonical PEM and DER form of the PCK chain, the
//! PCK CRL and the collateral issuer chains. It also reads the PCK identity
//! (issuing CA, FMSPC and PCE-ID) from the leaf certificate. Every parse
//! failure is `CollateralNonCanonical`. An unknown issuing CA is
//! `PlatformIdentityMismatch`.

use alloy_primitives::B256;
use der::{
    asn1::{AnyRef, ObjectIdentifier, OctetStringRef},
    Decode as _, Encode as _, Reader as _, SliceReader, Tag, TagNumber, Tagged as _,
};
use pem::{EncodeConfig, LineEnding};
use sha2::{Digest as _, Sha256};

use crate::dcap_protocol::{DcapPckCaV1, DcapRejectCodeV1};

pub(super) fn validate_canonical_pck_certificate_chain(
    bytes: &[u8],
) -> Result<(), DcapRejectCodeV1> {
    let canonical_pem = bytes
        .strip_suffix(&[0])
        .ok_or(DcapRejectCodeV1::CollateralNonCanonical)?;
    if canonical_pem.contains(&0) {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    validate_canonical_certificate_chain(canonical_pem, 3)
}

pub(super) fn pck_root_der_hash(bytes: &[u8]) -> Result<B256, DcapRejectCodeV1> {
    let certificates = pck_certificates(bytes)?;
    let root = certificates
        .last()
        .ok_or(DcapRejectCodeV1::CollateralNonCanonical)?;
    Ok(B256::from_slice(&Sha256::digest(root.contents())))
}

pub(super) struct PckIdentity {
    pub(super) ca: DcapPckCaV1,
    pub(super) fmspc: [u8; 6],
    pub(super) pce_id: [u8; 2],
}

/// Read the PEM blocks of a NUL-terminated PCK certificate chain.
fn pck_certificates(bytes: &[u8]) -> Result<Vec<pem::Pem>, DcapRejectCodeV1> {
    let canonical_pem = bytes
        .strip_suffix(&[0])
        .ok_or(DcapRejectCodeV1::CollateralNonCanonical)?;
    pem::parse_many(canonical_pem).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)
}

pub(super) fn parse_pck_identity(bytes: &[u8]) -> Result<PckIdentity, DcapRejectCodeV1> {
    const SGX_EXTENSION_OID: ObjectIdentifier =
        ObjectIdentifier::new_unwrap("1.2.840.113741.1.13.1");

    let certificates = pck_certificates(bytes)?;
    let leaf = certificates
        .first()
        .ok_or(DcapRejectCodeV1::CollateralNonCanonical)?;
    let (issuer, extensions) = certificate_issuer_and_extensions(leaf.contents())?;
    let extensions = extensions.ok_or(DcapRejectCodeV1::CollateralNonCanonical)?;
    let ca = pck_ca_from_issuer(issuer)?;
    let sgx_extension = find_certificate_extension(extensions, SGX_EXTENSION_OID)?;
    let (fmspc, pce_id) = parse_sgx_identity_entries(sgx_extension)?;
    Ok(PckIdentity { ca, fmspc, pce_id })
}

/// Read the issuer and the last explicit extensions field of one DER
/// certificate. The parser does not examine the other fields.
fn certificate_issuer_and_extensions(
    der: &[u8],
) -> Result<(AnyRef<'_>, Option<AnyRef<'_>>), DcapRejectCodeV1> {
    let certificate =
        AnyRef::from_der(der).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    certificate
        .sequence(|reader| {
            let tbs_certificate = reader.decode::<AnyRef<'_>>()?;
            reader.decode::<AnyRef<'_>>()?;
            reader.decode::<AnyRef<'_>>()?;
            tbs_certificate.sequence(read_tbs_issuer_and_extensions)
        })
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)
}

fn read_tbs_issuer_and_extensions<'a>(
    reader: &mut SliceReader<'a>,
) -> der::Result<(AnyRef<'a>, Option<AnyRef<'a>>)> {
    if reader.peek_tag()? == TagNumber::N0.context_specific(true) {
        reader.decode::<AnyRef<'_>>()?;
    }
    reader.decode::<AnyRef<'_>>()?;
    reader.decode::<AnyRef<'_>>()?;
    let issuer = reader.decode::<AnyRef<'a>>()?;
    reader.decode::<AnyRef<'_>>()?;
    reader.decode::<AnyRef<'_>>()?;
    reader.decode::<AnyRef<'_>>()?;
    let mut extensions = None;
    while !reader.is_finished() {
        let field = reader.decode::<AnyRef<'a>>()?;
        if field.tag() == TagNumber::N3.context_specific(true) {
            extensions = Some(field);
        }
    }
    Ok((issuer, extensions))
}

/// Read FMSPC and PCE-ID from the Intel SGX extension. If an entry occurs
/// more than once, the last value is used.
fn parse_sgx_identity_entries(
    sgx_extension: OctetStringRef<'_>,
) -> Result<([u8; 6], [u8; 2]), DcapRejectCodeV1> {
    const PCE_ID_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113741.1.13.1.3");
    const FMSPC_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113741.1.13.1.4");

    let mut fmspc = None;
    let mut pce_id = None;
    AnyRef::from_der(sgx_extension.as_bytes())
        .and_then(|entries| {
            entries.sequence(|reader| {
                while !reader.is_finished() {
                    let (oid, value) = oid_and_value(reader.decode::<AnyRef<'_>>()?)?;
                    if oid == FMSPC_OID {
                        fmspc = Some(fixed_octets(value)?);
                    } else if oid == PCE_ID_OID {
                        pce_id = Some(fixed_octets(value)?);
                    }
                }
                Ok(())
            })
        })
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    Ok((
        fmspc.ok_or(DcapRejectCodeV1::CollateralNonCanonical)?,
        pce_id.ok_or(DcapRejectCodeV1::CollateralNonCanonical)?,
    ))
}

/// Decode one `SEQUENCE { OBJECT IDENTIFIER, ANY }` value.
fn oid_and_value(entry: AnyRef<'_>) -> der::Result<(ObjectIdentifier, AnyRef<'_>)> {
    entry.sequence(|reader| {
        let oid = reader.decode::<ObjectIdentifier>()?;
        let value = reader.decode::<AnyRef<'_>>()?;
        Ok((oid, value))
    })
}

fn fixed_octets<const N: usize>(value: AnyRef<'_>) -> der::Result<[u8; N]> {
    value
        .decode_as::<OctetStringRef<'_>>()?
        .as_bytes()
        .try_into()
        .map_err(|_| der::Tag::OctetString.unexpected_error(None))
}

fn pck_ca_from_issuer(issuer: AnyRef<'_>) -> Result<DcapPckCaV1, DcapRejectCodeV1> {
    const COMMON_NAME_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.3");
    let mut common_name_count = 0_u8;
    let mut common_name = None;
    for_each_name_attribute(issuer, |oid, value| {
        if oid == COMMON_NAME_OID {
            common_name_count = common_name_count.saturating_add(1);
            common_name = Some((value.tag(), value.value()));
        }
    })
    .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    let (tag, common_name) = common_name.ok_or(DcapRejectCodeV1::CollateralNonCanonical)?;
    if common_name_count != 1 || !matches!(tag, Tag::Utf8String | Tag::PrintableString) {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    match common_name {
        b"Intel SGX PCK Processor CA" => Ok(DcapPckCaV1::Processor),
        b"Intel SGX PCK Platform CA" => Ok(DcapPckCaV1::Platform),
        _ => Err(DcapRejectCodeV1::PlatformIdentityMismatch),
    }
}

/// Visit each attribute of an X.501 `Name` in DER order. Each relative
/// distinguished name must be a SET.
fn for_each_name_attribute<'a>(
    name: AnyRef<'a>,
    mut visit: impl FnMut(ObjectIdentifier, AnyRef<'a>),
) -> der::Result<()> {
    name.sequence(|reader| {
        while !reader.is_finished() {
            let relative_name = reader.decode::<AnyRef<'a>>()?;
            relative_name.tag().assert_eq(Tag::Set)?;
            let mut set_reader = SliceReader::new(relative_name.value())?;
            while !set_reader.is_finished() {
                let (oid, value) = oid_and_value(set_reader.decode::<AnyRef<'a>>()?)?;
                visit(oid, value);
            }
            set_reader.finish(())?;
        }
        Ok(())
    })
}

fn find_certificate_extension<'a>(
    explicit_extensions: AnyRef<'a>,
    expected_oid: ObjectIdentifier,
) -> Result<OctetStringRef<'a>, DcapRejectCodeV1> {
    let extensions = AnyRef::from_der(explicit_extensions.value())
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    let mut matched = None;
    extensions
        .sequence(|reader| {
            while !reader.is_finished() {
                let (oid, value) = extension_oid_and_value(reader.decode::<AnyRef<'_>>()?)?;
                // A second match is an error, so the replaced value is not used.
                if oid == expected_oid && matched.replace(value).is_some() {
                    return Err(Tag::ObjectIdentifier.unexpected_error(None));
                }
            }
            Ok(())
        })
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    matched.ok_or(DcapRejectCodeV1::CollateralNonCanonical)
}

/// Decode `Extension ::= SEQUENCE { extnID, critical BOOLEAN DEFAULT FALSE,
/// extnValue OCTET STRING }`. The critical flag is read and not used.
fn extension_oid_and_value(
    extension: AnyRef<'_>,
) -> der::Result<(ObjectIdentifier, OctetStringRef<'_>)> {
    extension.sequence(|reader| {
        let oid = reader.decode::<ObjectIdentifier>()?;
        if reader.peek_tag()? == Tag::Boolean {
            reader.decode::<bool>()?;
        }
        let value = reader.decode::<OctetStringRef<'_>>()?;
        Ok((oid, value))
    })
}

pub(super) fn validate_canonical_certificate_chain(
    bytes: &[u8],
    expected_count: usize,
) -> Result<(), DcapRejectCodeV1> {
    let certificates =
        pem::parse_many(bytes).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    if certificates.len() != expected_count {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    let config = EncodeConfig::new().set_line_ending(LineEnding::LF);
    let mut canonical = String::new();
    for certificate in certificates {
        if certificate.tag() != "CERTIFICATE" || certificate.headers().iter().next().is_some() {
            return Err(DcapRejectCodeV1::CollateralNonCanonical);
        }
        let canonical_der = canonical_der_document(certificate.contents())?;
        if canonical_der != certificate.contents() {
            return Err(DcapRejectCodeV1::CollateralNonCanonical);
        }
        canonical.push_str(&pem::encode_config(
            &pem::Pem::new("CERTIFICATE", canonical_der),
            config,
        ));
    }
    if canonical.as_bytes() != bytes {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    Ok(())
}

pub(super) fn validate_canonical_der_crl(bytes: &[u8]) -> Result<(), DcapRejectCodeV1> {
    let canonical = canonical_der_document(bytes)?;
    if canonical != bytes {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    Ok(())
}

fn canonical_der_document(bytes: &[u8]) -> Result<Vec<u8>, DcapRejectCodeV1> {
    let document = AnyRef::from_der(bytes).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    if document.tag() != Tag::Sequence {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    document
        .to_der()
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Characterization of the PCK leaf grammar. The DER values are minimal:
    // the parser reads only the issuer and the explicit extensions.
    const SGX_EXTENSION: &str = "1.2.840.113741.1.13.1";
    const PCE_ID: &str = "1.2.840.113741.1.13.1.3";
    const FMSPC: &str = "1.2.840.113741.1.13.1.4";
    const TCB: &str = "1.2.840.113741.1.13.1.2";
    const COMMON_NAME: &str = "2.5.4.3";
    const ORGANIZATION: &str = "2.5.4.10";
    const PROCESSOR_CA: &[u8] = b"Intel SGX PCK Processor CA";
    const PLATFORM_CA: &[u8] = b"Intel SGX PCK Platform CA";
    const UTF8_STRING: u8 = 0x0c;
    const PRINTABLE_STRING: u8 = 0x13;
    const IA5_STRING: u8 = 0x16;
    const TEST_FMSPC: [u8; 6] = [0x00, 0x90, 0x6e, 0xd5, 0x00, 0x00];
    const TEST_PCE_ID: [u8; 2] = [0x00, 0x01];

    fn tlv(tag: u8, value: &[u8]) -> Vec<u8> {
        let length = value.len();
        let mut bytes = vec![tag];
        match length {
            0..=0x7f => bytes.push(length as u8),
            0x80..=0xff => bytes.extend_from_slice(&[0x81, length as u8]),
            _ => bytes.extend_from_slice(&[0x82, (length >> 8) as u8, length as u8]),
        }
        bytes.extend_from_slice(value);
        bytes
    }

    fn sequence(parts: &[Vec<u8>]) -> Vec<u8> {
        tlv(0x30, &parts.concat())
    }

    fn oid(value: &str) -> Vec<u8> {
        der::Encode::to_der(&ObjectIdentifier::new_unwrap(value)).unwrap()
    }

    fn octets(value: &[u8]) -> Vec<u8> {
        tlv(0x04, value)
    }

    fn sgx_entry(entry_oid: &str, value: Vec<u8>) -> Vec<u8> {
        sequence(&[oid(entry_oid), value])
    }

    fn sgx_entries(pce_id: Option<Vec<u8>>, fmspc: Option<Vec<u8>>) -> Vec<Vec<u8>> {
        let tcb = sequence(&[sgx_entry("1.2.840.113741.1.13.1.2.1", tlv(0x02, &[1]))]);
        let mut entries = vec![sgx_entry(TCB, tcb)];
        entries.extend(pce_id.map(|value| sgx_entry(PCE_ID, value)));
        entries.extend(fmspc.map(|value| sgx_entry(FMSPC, value)));
        entries
    }

    fn default_sgx_entries() -> Vec<Vec<u8>> {
        sgx_entries(Some(octets(&TEST_PCE_ID)), Some(octets(&TEST_FMSPC)))
    }

    fn extension(extension_oid: &str, critical: bool, value: &[u8]) -> Vec<u8> {
        let mut parts = vec![oid(extension_oid)];
        if critical {
            parts.push(tlv(0x01, &[0xff]));
        }
        parts.push(octets(value));
        sequence(&parts)
    }

    fn sgx_extension(entries: &[Vec<u8>]) -> Vec<u8> {
        extension(SGX_EXTENSION, false, &sequence(entries))
    }

    fn explicit_extensions(extensions: &[Vec<u8>]) -> Vec<u8> {
        tlv(0xa3, &sequence(extensions))
    }

    fn issuer(attributes: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let names: Vec<Vec<u8>> = attributes
            .iter()
            .map(|(attribute_oid, tag, value)| {
                tlv(0x31, &sequence(&[oid(attribute_oid), tlv(*tag, value)]))
            })
            .collect();
        sequence(&names)
    }

    fn processor_issuer() -> Vec<u8> {
        issuer(&[
            (COMMON_NAME, UTF8_STRING, PROCESSOR_CA),
            (ORGANIZATION, UTF8_STRING, b"Intel Corporation"),
        ])
    }

    fn tbs(version: bool, issuer: Vec<u8>, tail: &[Vec<u8>]) -> Vec<u8> {
        let mut parts = Vec::new();
        if version {
            parts.push(tlv(0xa0, &tlv(0x02, &[2])));
        }
        parts.push(tlv(0x02, &[1]));
        parts.push(sequence(&[oid("1.2.840.10045.4.3.2")]));
        parts.push(issuer);
        parts.push(sequence(&[]));
        parts.push(sequence(&[]));
        parts.push(sequence(&[]));
        parts.extend_from_slice(tail);
        sequence(&parts)
    }

    fn certificate(tbs: Vec<u8>) -> Vec<u8> {
        sequence(&[
            tbs,
            sequence(&[oid("1.2.840.10045.4.3.2")]),
            tlv(0x03, &[0]),
        ])
    }

    fn leaf(issuer: Vec<u8>, tail: &[Vec<u8>]) -> Vec<u8> {
        certificate(tbs(true, issuer, tail))
    }

    fn default_tail() -> Vec<Vec<u8>> {
        vec![explicit_extensions(&[
            extension("2.5.29.19", true, &sequence(&[])),
            sgx_extension(&default_sgx_entries()),
        ])]
    }

    fn pck_chain(certificates: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes: Vec<u8> = certificates
            .iter()
            .flat_map(|der| pem::encode(&pem::Pem::new("CERTIFICATE", der.clone())).into_bytes())
            .collect();
        bytes.push(0);
        bytes
    }

    fn parse_leaf(der: Vec<u8>) -> Result<(DcapPckCaV1, [u8; 6], [u8; 2]), DcapRejectCodeV1> {
        parse_pck_identity(&pck_chain(&[der]))
            .map(|identity| (identity.ca, identity.fmspc, identity.pce_id))
    }

    #[test]
    fn pck_identity_reads_ca_fmspc_and_pce_id_from_the_first_certificate() {
        let expected = Ok((DcapPckCaV1::Processor, TEST_FMSPC, TEST_PCE_ID));
        assert_eq!(
            parse_leaf(leaf(processor_issuer(), &default_tail())),
            expected
        );
        assert_eq!(
            parse_leaf(certificate(tbs(false, processor_issuer(), &default_tail()))),
            expected
        );
        let platform = issuer(&[(COMMON_NAME, PRINTABLE_STRING, PLATFORM_CA)]);
        assert_eq!(
            parse_leaf(leaf(platform, &default_tail())),
            Ok((DcapPckCaV1::Platform, TEST_FMSPC, TEST_PCE_ID))
        );
        let undecoded_second = vec![0xff, 0x00];
        assert_eq!(
            parse_pck_identity(&pck_chain(&[
                leaf(processor_issuer(), &default_tail()),
                undecoded_second,
            ]))
            .map(|identity| identity.fmspc),
            Ok(TEST_FMSPC)
        );
    }

    #[test]
    fn pck_identity_rejects_noncanonical_chain_framing() {
        let mut unterminated = pck_chain(&[leaf(processor_issuer(), &default_tail())]);
        unterminated.pop();
        assert_eq!(
            parse_pck_identity(&unterminated).err(),
            Some(DcapRejectCodeV1::CollateralNonCanonical)
        );
        assert_eq!(
            parse_pck_identity(&[0]).err(),
            Some(DcapRejectCodeV1::CollateralNonCanonical)
        );
        assert_eq!(
            parse_leaf(vec![0x30, 0x01]).err(),
            Some(DcapRejectCodeV1::CollateralNonCanonical)
        );
        let trailing_element = sequence(&[
            tbs(true, processor_issuer(), &default_tail()),
            sequence(&[oid("1.2.840.10045.4.3.2")]),
            tlv(0x03, &[0]),
            tlv(0x05, &[]),
        ]);
        assert_eq!(
            parse_leaf(trailing_element).err(),
            Some(DcapRejectCodeV1::CollateralNonCanonical)
        );
        let truncated_tbs = certificate(sequence(&[tlv(0x02, &[1]), processor_issuer()]));
        assert_eq!(
            parse_leaf(truncated_tbs).err(),
            Some(DcapRejectCodeV1::CollateralNonCanonical)
        );
    }

    #[test]
    fn pck_identity_requires_extensions_before_issuer_policy() {
        let unknown = issuer(&[(COMMON_NAME, UTF8_STRING, b"Other CA")]);
        assert_eq!(
            parse_leaf(leaf(processor_issuer(), &[])).err(),
            Some(DcapRejectCodeV1::CollateralNonCanonical)
        );
        assert_eq!(
            parse_leaf(leaf(unknown.clone(), &[])).err(),
            Some(DcapRejectCodeV1::CollateralNonCanonical)
        );
        assert_eq!(
            parse_leaf(leaf(unknown.clone(), &default_tail())).err(),
            Some(DcapRejectCodeV1::PlatformIdentityMismatch)
        );
        let without_sgx = vec![explicit_extensions(&[extension("2.5.29.19", true, &[])])];
        assert_eq!(
            parse_leaf(leaf(unknown, &without_sgx)).err(),
            Some(DcapRejectCodeV1::PlatformIdentityMismatch)
        );
    }

    #[test]
    fn pck_identity_issuer_needs_one_string_common_name() {
        for issuer in [
            issuer(&[(ORGANIZATION, UTF8_STRING, b"Intel Corporation")]),
            issuer(&[
                (COMMON_NAME, UTF8_STRING, PROCESSOR_CA),
                (COMMON_NAME, UTF8_STRING, PROCESSOR_CA),
            ]),
            issuer(&[(COMMON_NAME, IA5_STRING, PROCESSOR_CA)]),
            sequence(&[sequence(&[sequence(&[
                oid(COMMON_NAME),
                tlv(UTF8_STRING, PROCESSOR_CA),
            ])])]),
        ] {
            assert_eq!(
                parse_leaf(leaf(issuer, &default_tail())).err(),
                Some(DcapRejectCodeV1::CollateralNonCanonical)
            );
        }
    }

    #[test]
    fn pck_identity_uses_the_last_explicit_extension_field() {
        let without_sgx = explicit_extensions(&[extension("2.5.29.19", false, &[])]);
        let with_sgx = explicit_extensions(&[sgx_extension(&default_sgx_entries())]);
        assert_eq!(
            parse_leaf(leaf(
                processor_issuer(),
                &[without_sgx.clone(), with_sgx.clone()]
            )),
            Ok((DcapPckCaV1::Processor, TEST_FMSPC, TEST_PCE_ID))
        );
        assert_eq!(
            parse_leaf(leaf(processor_issuer(), &[with_sgx, without_sgx])).err(),
            Some(DcapRejectCodeV1::CollateralNonCanonical)
        );
    }

    #[test]
    fn pck_identity_requires_exactly_one_sgx_extension() {
        let sgx = sgx_extension(&default_sgx_entries());
        let critical_sgx = extension(SGX_EXTENSION, true, &sequence(&default_sgx_entries()));
        assert_eq!(
            parse_leaf(leaf(
                processor_issuer(),
                &[explicit_extensions(&[critical_sgx])]
            )),
            Ok((DcapPckCaV1::Processor, TEST_FMSPC, TEST_PCE_ID))
        );
        for extensions in [
            vec![sgx.clone(), sgx.clone()],
            vec![extension("2.5.29.19", false, &[])],
            vec![sequence(&[oid(SGX_EXTENSION), tlv(0x02, &[1])])],
        ] {
            assert_eq!(
                parse_leaf(leaf(
                    processor_issuer(),
                    &[explicit_extensions(&extensions)]
                ))
                .err(),
                Some(DcapRejectCodeV1::CollateralNonCanonical)
            );
        }
        let two_values = tlv(
            0xa3,
            &[sequence(std::slice::from_ref(&sgx)), sequence(&[sgx])].concat(),
        );
        assert_eq!(
            parse_leaf(leaf(processor_issuer(), &[two_values])).err(),
            Some(DcapRejectCodeV1::CollateralNonCanonical)
        );
    }

    #[test]
    fn pck_identity_sgx_entries_need_exact_fmspc_and_pce_id_octets() {
        for entries in [
            sgx_entries(Some(octets(&TEST_PCE_ID)), None),
            sgx_entries(None, Some(octets(&TEST_FMSPC))),
            sgx_entries(Some(octets(&TEST_PCE_ID)), Some(octets(&TEST_FMSPC[..5]))),
            sgx_entries(
                Some(octets(&TEST_PCE_ID)),
                Some(octets(&[TEST_FMSPC.as_slice(), &[0]].concat())),
            ),
            sgx_entries(Some(octets(&[0x00])), Some(octets(&TEST_FMSPC))),
            sgx_entries(Some(tlv(0x02, &[1])), Some(octets(&TEST_FMSPC))),
            vec![sequence(&[oid(FMSPC), octets(&TEST_FMSPC), tlv(0x05, &[])])],
            vec![octets(&TEST_FMSPC)],
        ] {
            assert_eq!(
                parse_leaf(leaf(
                    processor_issuer(),
                    &[explicit_extensions(&[sgx_extension(&entries)])]
                ))
                .err(),
                Some(DcapRejectCodeV1::CollateralNonCanonical)
            );
        }
        let mut repeated = default_sgx_entries();
        repeated.push(sgx_entry(FMSPC, octets(&[0x11; 6])));
        assert_eq!(
            parse_leaf(leaf(
                processor_issuer(),
                &[explicit_extensions(&[sgx_extension(&repeated)])]
            )),
            Ok((DcapPckCaV1::Processor, [0x11; 6], TEST_PCE_ID))
        );
    }
}
