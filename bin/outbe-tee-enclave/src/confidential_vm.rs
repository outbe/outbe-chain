//! Platform backend of a service that runs in a confidential VM: AMD SEV-SNP
//! or Intel TDX.
//!
//! In an SGX enclave the service takes three things from Gramine: its code
//! identity, its attestation quote and its sealing key. In a confidential VM
//! this module supplies the same three things:
//!
//! | SGX under Gramine | SEV-SNP VM | TDX VM |
//! |---|---|---|
//! | `MRENCLAVE`, `MRSIGNER` | launch measurement, host data | `MRTD` with `RTMR0`-`RTMR2`, `MRCONFIGID` |
//! | DCAP quote | attestation report from the kernel TSM interface | TDX quote from the same interface |
//! | `EGETKEY` sealing key | key that the processor derives for this VM | key from the dstack guest agent |
//!
//! The process selects the backend one time at startup with `--platform
//! sev-snp` or `--platform tdx`. The rest of the service reaches it through
//! the hooks in [`crate::gramine`], [`crate::sgx_sealing`] and
//! [`crate::transport`].
//!
//! The Cargo feature `sev-snp` gates this module for both platforms: one VM
//! image runs on both.
//!
//! Limits of this version, recorded as technical debt:
//!
//! - The network descriptor comes from an unmeasured file. An operator can
//!   supply a descriptor of a private chain.
//! - SEV-SNP: the derived key depends on the launch measurement but not on the
//!   host data. A VM that starts the same image with a different application
//!   manifest receives the same key.
//! - TDX: the sealing key comes from the dstack guest agent. Its protection is
//!   that of the key provider of the VM. With the key provider `none` the
//!   agent makes new keys at every boot, so sealed state does not survive a
//!   restart and the key is not bound to the measurements.

use std::path::Path;
use std::sync::OnceLock;

use outbe_primitives::tee_attestation_v1::{
    sev_snp_measurement_rule_value, tdx_image_rule_value, tdx_mrconfigid_rule_value,
    NetworkBindingV1, TrustedNetworkDescriptorV1, SEV_SNP_RULE_PRODUCT_ID_V1, SEV_SNP_RULE_SVN_V1,
    TDX_RULE_PRODUCT_ID_V1, TDX_RULE_SVN_V1,
};
use outbe_tee::quote::ReportMeasurements;
use outbe_tee::tdx_report::{parse_tdx_quote_measurements, tdx_quote_canonical_len};
use zeroize::Zeroizing;

use crate::errors::{ConfidentialVmError, EnclaveResult, TsmError};
use crate::seal::{self, KeyPolicy, SealHeader, UnsealedTributeOfferAndGroupSig};
use crate::tsm::{self, SevSnpIdentity, TsmProvider, TsmReportSource};

/// Prefix of a blob sealed with the derived key of an SEV-SNP VM.
const SEV_SNP_SEAL_MAGIC: &[u8] = b"SNPS1";
/// Prefix of a blob sealed with the key of a TDX VM.
const TDX_SEAL_MAGIC: &[u8] = b"TDXS1";
/// Offset of `REPORT_DATA` in an SEV-SNP attestation report.
const REPORT_DATA_OFFSET: usize = 0x50;
const SEALING_KEY_INFO: &[u8] = b"outbe/tee/seal-key/sev-snp/v1";
const TDX_SEALING_KEY_INFO: &[u8] = b"outbe/tee/seal-key/tdx/v1";
/// Key path that the service requests from the dstack guest agent.
const TDX_GUEST_AGENT_KEY_PATH: &str = "outbe/tee/seal-key/v1";

/// Confidential VM platform that `--platform` selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfidentialPlatform {
    SevSnp,
    Tdx,
}

/// Identity of a trust domain, as carried in its quote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TdxIdentity {
    pub(crate) mrtd: [u8; 48],
    pub(crate) rtmrs: [[u8; 48]; 3],
    pub(crate) mr_config_id: [u8; 48],
}

/// Identity of the running VM on its platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Identity {
    SevSnp(SevSnpIdentity),
    Tdx(TdxIdentity),
}

impl Identity {
    const fn key_policy(&self) -> KeyPolicy {
        match self {
            Self::SevSnp(_) => KeyPolicy::SevSnpDerived,
            Self::Tdx(_) => KeyPolicy::TdxGuestAgent,
        }
    }

    const fn seal_magic(&self) -> &'static [u8] {
        match self {
            Self::SevSnp(_) => SEV_SNP_SEAL_MAGIC,
            Self::Tdx(_) => TDX_SEAL_MAGIC,
        }
    }
}

struct Backend {
    identity: Identity,
    sealing_key: Zeroizing<[u8; 32]>,
    source: TsmReportSource,
    network_descriptor: TrustedNetworkDescriptorV1,
}

static BACKEND: OnceLock<Backend> = OnceLock::new();

/// True after [`configure`] selected the confidential VM backend.
pub(crate) fn configured() -> bool {
    BACKEND.get().is_some()
}

fn backend() -> EnclaveResult<&'static Backend> {
    BACKEND
        .get()
        .ok_or_else(|| ConfidentialVmError::NotConfigured.into())
}

/// Select the confidential VM backend for this process.
///
/// Reads the launch identity of the VM, obtains the sealing key and loads the
/// network descriptor. Every step must succeed: the service does not start
/// with a partial platform.
pub fn configure(platform: ConfidentialPlatform, network_descriptor: &Path) -> EnclaveResult<()> {
    let source = TsmReportSource::system();
    if !source.is_available() {
        return Err(ConfidentialVmError::TsmUnavailable.into());
    }
    let provider = source.provider()?;
    let report = source.report(&[0; tsm::REPORT_DATA_LEN])?;
    let (identity, sealing_key) = match (platform, provider) {
        (ConfidentialPlatform::SevSnp, TsmProvider::SevGuest) => {
            let identity = tsm::sev_snp_identity(&report.report)?;
            let derived = derived_key()?;
            (
                Identity::SevSnp(identity),
                sealing_key_from(&derived, &identity)?,
            )
        }
        (ConfidentialPlatform::Tdx, TsmProvider::TdxGuest) => {
            let identity = tdx_identity(&report.report)?;
            let agent_key = crate::dstack_guest_agent::get_key(TDX_GUEST_AGENT_KEY_PATH)?;
            (
                Identity::Tdx(identity),
                tdx_sealing_key_from(&agent_key, &identity)?,
            )
        }
        (_, other) => return Err(TsmError::UnsupportedProvider(other.name().to_owned()).into()),
    };
    let bytes = std::fs::read(network_descriptor).map_err(|error| {
        ConfidentialVmError::network_descriptor(network_descriptor.display(), error)
    })?;
    let network_descriptor =
        TrustedNetworkDescriptorV1::decode_canonical(&bytes).map_err(|error| {
            ConfidentialVmError::network_descriptor(network_descriptor.display(), error)
        })?;
    BACKEND
        .set(Backend {
            identity,
            sealing_key,
            source,
            network_descriptor,
        })
        .map_err(|_| ConfidentialVmError::AlreadyConfigured.into())
}

/// Ask the processor for the key bound to the launch measurement of this VM.
fn derived_key() -> EnclaveResult<Zeroizing<[u8; 32]>> {
    use sev::firmware::guest::{DerivedKey, Firmware, GuestFieldSelect};

    let mut fields = GuestFieldSelect::default();
    fields.set_guest_policy(true);
    fields.set_measurement(true);
    // Root key 0 is the chip key. The request does not mix the firmware
    // version, so a firmware update keeps the sealed state readable.
    let request = DerivedKey::new(false, fields, 0, 0, 0, None);
    let mut firmware = Firmware::open().map_err(ConfidentialVmError::derived_key)?;
    firmware
        .get_derived_key(None, request)
        .map(Zeroizing::new)
        .map_err(|error| ConfidentialVmError::derived_key(error).into())
}

/// Expand the processor key into the sealing key of this service.
///
/// The host data separates the keys of two deployments of one image. It is not
/// a security boundary: the processor gives the same input key to both.
fn sealing_key_from(
    derived: &[u8; 32],
    identity: &SevSnpIdentity,
) -> EnclaveResult<Zeroizing<[u8; 32]>> {
    crate::crypto::hkdf_sha256(&identity.host_data, derived, SEALING_KEY_INFO)
        .map(Zeroizing::new)
        .map_err(Into::into)
}

/// Read the identity of a trust domain from its quote.
///
/// This reads fixed offsets and does not authenticate the quote. A debug TD is
/// refused: its memory is readable by the host.
fn tdx_identity(quote: &[u8]) -> EnclaveResult<TdxIdentity> {
    let measurements =
        parse_tdx_quote_measurements(quote).map_err(|_| TsmError::MalformedReport {
            platform: "TDX",
            detail: format!("quote of {} bytes has no TD report body", quote.len()),
        })?;
    if measurements.is_debug() {
        return Err(ConfidentialVmError::DebugTrustDomain.into());
    }
    Ok(TdxIdentity {
        mrtd: measurements.mrtd,
        rtmrs: measurements.rtmrs,
        mr_config_id: measurements.mr_config_id,
    })
}

/// Expand the key of the dstack guest agent into the sealing key of this
/// service.
///
/// The measurements separate the keys of two images and of two application
/// manifests. They are not a security boundary by themselves: the protection
/// of the input key is that of the key provider of the VM.
fn tdx_sealing_key_from(
    agent_key: &[u8],
    identity: &TdxIdentity,
) -> EnclaveResult<Zeroizing<[u8; 32]>> {
    let mut salt = Vec::with_capacity(64);
    salt.extend_from_slice(tdx_image_rule_value(&identity.mrtd, &identity.rtmrs).as_slice());
    salt.extend_from_slice(tdx_mrconfigid_rule_value(&identity.mr_config_id).as_slice());
    crate::crypto::hkdf_sha256(&salt, agent_key, TDX_SEALING_KEY_INFO)
        .map(Zeroizing::new)
        .map_err(Into::into)
}

/// Code identity of this VM in the fields of an SGX report.
///
/// The projection equals the one that the evidence verifier writes into its
/// verdict, so the service compares its own identity with a verdict directly.
fn measurements_of(identity: &Identity, report_data: &[u8; 64]) -> ReportMeasurements {
    let (mrenclave, mrsigner, isv_prod_id, isv_svn) = match identity {
        Identity::SevSnp(identity) => (
            sev_snp_measurement_rule_value(&identity.measurement).0,
            identity.host_data,
            SEV_SNP_RULE_PRODUCT_ID_V1,
            SEV_SNP_RULE_SVN_V1,
        ),
        Identity::Tdx(identity) => (
            tdx_image_rule_value(&identity.mrtd, &identity.rtmrs).0,
            tdx_mrconfigid_rule_value(&identity.mr_config_id).0,
            TDX_RULE_PRODUCT_ID_V1,
            TDX_RULE_SVN_V1,
        ),
    };
    ReportMeasurements {
        misc_select: 0,
        attributes: [0; 16],
        mrenclave,
        mrsigner,
        isv_prod_id,
        isv_svn,
        report_data: *report_data,
    }
}

/// Platform of the configured backend.
pub(crate) fn platform() -> Option<ConfidentialPlatform> {
    BACKEND.get().map(|backend| match backend.identity {
        Identity::SevSnp(_) => ConfidentialPlatform::SevSnp,
        Identity::Tdx(_) => ConfidentialPlatform::Tdx,
    })
}

/// Code identity of the running VM.
pub(crate) fn measurements(report_data: &[u8; 64]) -> EnclaveResult<ReportMeasurements> {
    Ok(measurements_of(&backend()?.identity, report_data))
}

/// Network descriptor that `--network-descriptor` supplied.
pub(crate) fn network_descriptor() -> Option<TrustedNetworkDescriptorV1> {
    BACKEND
        .get()
        .map(|backend| backend.network_descriptor.clone())
}

/// Sealing key of the running VM with its key policy.
pub(crate) fn sealing_key() -> Option<([u8; 32], KeyPolicy)> {
    BACKEND
        .get()
        .map(|backend| (*backend.sealing_key, backend.identity.key_policy()))
}

/// Request the evidence that binds `report_data`: an SEV-SNP attestation
/// report, or a TDX quote without bytes after its signature data.
pub(crate) fn registration_report(report_data: &[u8; 64]) -> EnclaveResult<Vec<u8>> {
    let backend = backend()?;
    let mut report = backend.source.report(report_data)?.report;
    if let Identity::Tdx(_) = backend.identity {
        let len = tdx_quote_canonical_len(&report).map_err(|_| TsmError::MalformedReport {
            platform: "TDX",
            detail: "signature data does not end the quote".to_owned(),
        })?;
        report.truncate(len);
    }
    validate_report(&report, report_data, &backend.identity)?;
    Ok(report)
}

/// Check that `report` binds `report_data` and belongs to the VM of `identity`.
fn validate_report(
    report: &[u8],
    report_data: &[u8; 64],
    identity: &Identity,
) -> EnclaveResult<()> {
    match identity {
        Identity::SevSnp(identity) => {
            if tsm::sev_snp_identity(report)? != *identity {
                return Err(ConfidentialVmError::ReportIdentityChanged.into());
            }
            if report[REPORT_DATA_OFFSET..REPORT_DATA_OFFSET + 64] != report_data[..] {
                return Err(ConfidentialVmError::ReportDataMismatch.into());
            }
        }
        Identity::Tdx(identity) => {
            if tdx_identity(report)? != *identity {
                return Err(ConfidentialVmError::ReportIdentityChanged.into());
            }
            let measurements =
                parse_tdx_quote_measurements(report).map_err(|_| TsmError::MalformedReport {
                    platform: "TDX",
                    detail: "quote has no TD report body".to_owned(),
                })?;
            if measurements.report_data != *report_data {
                return Err(ConfidentialVmError::ReportDataMismatch.into());
            }
        }
    }
    Ok(())
}

/// Check the report that the service is about to return to its node.
pub(crate) fn validate_registration_report(
    report: &[u8],
    report_data: &[u8; 64],
) -> EnclaveResult<()> {
    validate_report(report, report_data, &backend()?.identity)
}

/// Seal a payload with the key of the running VM.
pub(crate) fn seal(
    secret: &[u8; 32],
    extra: &[u8],
    binding: NetworkBindingV1,
    header: &SealHeader,
) -> EnclaveResult<Vec<u8>> {
    let backend = backend()?;
    seal_with(
        &backend.sealing_key,
        &backend.identity,
        secret,
        extra,
        binding,
        header,
    )
}

fn seal_with(
    key: &[u8; 32],
    identity: &Identity,
    secret: &[u8; 32],
    extra: &[u8],
    binding: NetworkBindingV1,
    header: &SealHeader,
) -> EnclaveResult<Vec<u8>> {
    let mut header = *header;
    header.key_policy = identity.key_policy();
    let payload = seal::seal_tribute_offer_and_group_sig(secret, extra, key, binding, &header)?;
    let magic = identity.seal_magic();
    let mut blob = Vec::with_capacity(magic.len() + payload.len());
    blob.extend_from_slice(magic);
    blob.extend_from_slice(&payload);
    Ok(blob)
}

/// Open a blob that [`seal`] wrote in a VM with the same sealing key.
pub(crate) fn unseal(blob: &[u8], svn: u16) -> EnclaveResult<UnsealedTributeOfferAndGroupSig> {
    let backend = backend()?;
    unseal_with(&backend.sealing_key, &backend.identity, blob, svn)
}

fn unseal_with(
    key: &[u8; 32],
    identity: &Identity,
    blob: &[u8],
    svn: u16,
) -> EnclaveResult<UnsealedTributeOfferAndGroupSig> {
    let payload = blob
        .strip_prefix(identity.seal_magic())
        .ok_or(ConfidentialVmError::NotSevSnpSeal)?;
    let value = seal::unseal_network_bound_payload(payload, key, svn)?;
    if value.header.key_policy != identity.key_policy() {
        return Err(ConfidentialVmError::SealPolicyMismatch.into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::EnclaveError;
    use outbe_primitives::tee_attestation_v1::AttestationMode;

    const SIMULATOR_REPORT: &[u8] =
        include_bytes!("../../../crates/system/tee/tests/fixtures/sev_snp/simulator-report.bin");
    const TDX_QUOTE_FILE: &[u8] = include_bytes!(
        "../../../crates/system/tee/tests/fixtures/intel-tdx-quote-v4/tdx-quote-v4.bin"
    );
    /// Offset of the report data in a TDX quote: the header and the field
    /// offset in the TD report body.
    const TDX_REPORT_DATA_OFFSET: usize = 48 + 520;
    /// Offset of `TD_ATTRIBUTES` in a TDX quote.
    const TDX_TD_ATTRIBUTES_OFFSET: usize = 48 + 120;

    fn snp_identity() -> SevSnpIdentity {
        tsm::sev_snp_identity(SIMULATOR_REPORT).unwrap()
    }

    fn identity() -> Identity {
        Identity::SevSnp(snp_identity())
    }

    fn tdx_quote() -> Vec<u8> {
        TDX_QUOTE_FILE[..tdx_quote_canonical_len(TDX_QUOTE_FILE).unwrap()].to_vec()
    }

    fn tdx() -> TdxIdentity {
        tdx_identity(&tdx_quote()).unwrap()
    }

    fn report_data() -> [u8; 64] {
        SIMULATOR_REPORT[REPORT_DATA_OFFSET..REPORT_DATA_OFFSET + 64]
            .try_into()
            .unwrap()
    }

    fn tdx_report_data() -> [u8; 64] {
        TDX_QUOTE_FILE[TDX_REPORT_DATA_OFFSET..TDX_REPORT_DATA_OFFSET + 64]
            .try_into()
            .unwrap()
    }

    fn binding() -> NetworkBindingV1 {
        NetworkBindingV1 {
            chain_id: [7; 32],
            genesis_hash: alloy_primitives::B256::repeat_byte(9),
            attestation_mode: AttestationMode::SevSnpRequired,
        }
    }

    fn header() -> SealHeader {
        SealHeader {
            format_version: seal::SEAL_FORMAT,
            key_policy: KeyPolicy::MrEnclaveAndSigner,
            isv_svn: SEV_SNP_RULE_SVN_V1,
            key_epoch: 0,
            tribute_offer_epoch: 0,
            nonce: [3; 12],
        }
    }

    #[test]
    fn projected_identity_equals_the_verifier_projection() {
        let snp = snp_identity();
        let measurements = measurements_of(&identity(), &[5; 64]);
        assert_eq!(
            measurements.mrenclave,
            sev_snp_measurement_rule_value(&snp.measurement).0
        );
        assert_eq!(measurements.mrsigner, snp.host_data);
        assert_eq!(measurements.isv_prod_id, SEV_SNP_RULE_PRODUCT_ID_V1);
        assert_eq!(measurements.isv_svn, SEV_SNP_RULE_SVN_V1);
        assert_eq!(measurements.report_data, [5; 64]);
        assert!(!measurements.is_debug());
    }

    #[test]
    fn tdx_projected_identity_equals_the_verifier_projection() {
        let quote = tdx_quote();
        let parsed = parse_tdx_quote_measurements(&quote).unwrap();
        let measurements = measurements_of(&Identity::Tdx(tdx()), &[5; 64]);
        assert_eq!(measurements.mrenclave, parsed.image_rule_value().0);
        assert_eq!(measurements.mrsigner, parsed.mrconfigid_rule_value().0);
        assert_eq!(measurements.isv_prod_id, TDX_RULE_PRODUCT_ID_V1);
        assert_eq!(measurements.isv_svn, TDX_RULE_SVN_V1);
        assert_eq!(measurements.report_data, [5; 64]);
    }

    #[test]
    fn report_validation_binds_report_data_and_identity() {
        let identity = identity();
        validate_report(SIMULATOR_REPORT, &report_data(), &identity).unwrap();

        let mut other_data = report_data();
        other_data[0] ^= 1;
        assert!(matches!(
            validate_report(SIMULATOR_REPORT, &other_data, &identity),
            Err(EnclaveError::ConfidentialVm(
                ConfidentialVmError::ReportDataMismatch
            ))
        ));

        let mut other_image = snp_identity();
        other_image.measurement[0] ^= 1;
        assert!(matches!(
            validate_report(
                SIMULATOR_REPORT,
                &report_data(),
                &Identity::SevSnp(other_image)
            ),
            Err(EnclaveError::ConfidentialVm(
                ConfidentialVmError::ReportIdentityChanged
            ))
        ));

        assert!(validate_report(&SIMULATOR_REPORT[..100], &report_data(), &identity).is_err());
    }

    #[test]
    fn tdx_quote_validation_binds_report_data_and_identity() {
        let quote = tdx_quote();
        let identity = Identity::Tdx(tdx());
        validate_report(&quote, &tdx_report_data(), &identity).unwrap();

        let mut other_data = tdx_report_data();
        other_data[0] ^= 1;
        assert!(matches!(
            validate_report(&quote, &other_data, &identity),
            Err(EnclaveError::ConfidentialVm(
                ConfidentialVmError::ReportDataMismatch
            ))
        ));

        let mut other_manifest = tdx();
        other_manifest.mr_config_id[0] ^= 1;
        assert!(matches!(
            validate_report(&quote, &tdx_report_data(), &Identity::Tdx(other_manifest)),
            Err(EnclaveError::ConfidentialVm(
                ConfidentialVmError::ReportIdentityChanged
            ))
        ));

        // An SEV-SNP report is not a TDX quote and the reverse.
        assert!(validate_report(SIMULATOR_REPORT, &tdx_report_data(), &identity).is_err());
        assert!(validate_report(&quote, &report_data(), &self::identity()).is_err());
    }

    #[test]
    fn debug_trust_domain_has_no_identity() {
        let mut quote = tdx_quote();
        quote[TDX_TD_ATTRIBUTES_OFFSET] |= 1;
        assert!(matches!(
            tdx_identity(&quote),
            Err(EnclaveError::ConfidentialVm(
                ConfidentialVmError::DebugTrustDomain
            ))
        ));
    }

    #[test]
    fn sealing_key_depends_on_the_processor_key_and_the_host_data() {
        let identity = snp_identity();
        let key = sealing_key_from(&[1; 32], &identity).unwrap();
        assert_eq!(*key, *sealing_key_from(&[1; 32], &identity).unwrap());
        assert_ne!(*key, *sealing_key_from(&[2; 32], &identity).unwrap());
        let mut other_manifest = identity;
        other_manifest.host_data[0] ^= 1;
        assert_ne!(*key, *sealing_key_from(&[1; 32], &other_manifest).unwrap());
    }

    #[test]
    fn tdx_sealing_key_depends_on_the_agent_key_and_the_measurements() {
        let identity = tdx();
        let key = tdx_sealing_key_from(&[1; 32], &identity).unwrap();
        assert_eq!(*key, *tdx_sealing_key_from(&[1; 32], &identity).unwrap());
        assert_ne!(*key, *tdx_sealing_key_from(&[2; 32], &identity).unwrap());

        let mut other_image = identity;
        other_image.rtmrs[2][0] ^= 1;
        assert_ne!(*key, *tdx_sealing_key_from(&[1; 32], &other_image).unwrap());
        let mut other_manifest = identity;
        other_manifest.mr_config_id[0] ^= 1;
        assert_ne!(
            *key,
            *tdx_sealing_key_from(&[1; 32], &other_manifest).unwrap()
        );
    }

    #[test]
    fn seal_round_trip_and_rejections() {
        let identity = identity();
        let blob = seal_with(
            &[4; 32],
            &identity,
            &[8; 32],
            b"extra",
            binding(),
            &header(),
        )
        .unwrap();
        assert!(blob.starts_with(SEV_SNP_SEAL_MAGIC));
        let value = unseal_with(&[4; 32], &identity, &blob, SEV_SNP_RULE_SVN_V1).unwrap();
        assert_eq!(*value.tribute_offer_secret, [8; 32]);
        assert_eq!(value.group_sig.as_slice(), b"extra");
        assert_eq!(value.network_binding, binding());
        assert_eq!(value.header.key_policy, KeyPolicy::SevSnpDerived);

        // A VM with another launch measurement has another key.
        assert!(unseal_with(&[5; 32], &identity, &blob, SEV_SNP_RULE_SVN_V1).is_err());

        // A blob of another backend is not accepted.
        for foreign in [b"TSGX1".as_slice(), b"LE2E1", b"TDXS1", b""] {
            assert!(matches!(
                unseal_with(&[4; 32], &identity, foreign, SEV_SNP_RULE_SVN_V1),
                Err(EnclaveError::ConfidentialVm(
                    ConfidentialVmError::NotSevSnpSeal
                ))
            ));
        }

        let mut corrupted = blob;
        let last = corrupted.len() - 1;
        corrupted[last] ^= 1;
        assert!(unseal_with(&[4; 32], &identity, &corrupted, SEV_SNP_RULE_SVN_V1).is_err());
    }

    #[test]
    fn tdx_seal_round_trip_and_platform_separation() {
        let tdx = Identity::Tdx(tdx());
        let blob = seal_with(&[4; 32], &tdx, &[8; 32], b"extra", binding(), &header()).unwrap();
        assert!(blob.starts_with(TDX_SEAL_MAGIC));
        let value = unseal_with(&[4; 32], &tdx, &blob, TDX_RULE_SVN_V1).unwrap();
        assert_eq!(*value.tribute_offer_secret, [8; 32]);
        assert_eq!(value.header.key_policy, KeyPolicy::TdxGuestAgent);

        // The SEV-SNP backend does not open a TDX blob with the same key.
        assert!(unseal_with(&[4; 32], &identity(), &blob, TDX_RULE_SVN_V1).is_err());
        assert!(unseal_with(&[5; 32], &tdx, &blob, TDX_RULE_SVN_V1).is_err());
    }

    #[test]
    fn hooks_are_inactive_until_the_backend_is_configured() {
        assert!(!configured());
        assert!(platform().is_none());
        assert!(sealing_key().is_none());
        assert!(network_descriptor().is_none());
        assert!(matches!(
            measurements(&[0; 64]),
            Err(EnclaveError::ConfidentialVm(
                ConfidentialVmError::NotConfigured
            ))
        ));
    }
}
