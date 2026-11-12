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
//! | `EGETKEY` sealing key | random key of this process | random key of this process |
//!
//! The process selects the backend one time at startup with `--platform
//! sev-snp` or `--platform tdx`. The rest of the service reaches it through
//! the hooks in [`crate::gramine`], [`crate::sgx_sealing`] and
//! [`crate::transport`].
//!
//! The Cargo feature `sev-snp` gates this module for both platforms: one VM
//! image runs on both.
//!
//! The service keeps the offer key in the memory of the VM only. The sealing
//! key is random for each process, so a state file does not open after a
//! restart and a VM with another manifest cannot open it. A restarted VM
//! receives the offer key again from the network.
//!
//! The application manifest of the VM is the trust root for two decisions:
//! which network descriptor the service accepts, and which VMs receive the
//! offer key. See [`crate::measured_manifest`].

use std::path::Path;
use std::sync::OnceLock;

use outbe_primitives::tee_attestation_v1::{
    sev_snp_measurement_rule_value, tdx_image_rule_value, tdx_mrconfigid_rule_value,
    AttestationMode, NetworkBindingV1, TrustedNetworkDescriptorV1, SEV_SNP_RULE_PRODUCT_ID_V1,
    SEV_SNP_RULE_SVN_V1, TDX_RULE_PRODUCT_ID_V1, TDX_RULE_SVN_V1,
};
use outbe_tee::quote::ReportMeasurements;
use outbe_tee::tdx_report::{parse_tdx_quote_measurements, tdx_quote_canonical_len};
use zeroize::Zeroizing;

use crate::errors::{ConfidentialVmError, EnclaveResult, TsmError};
use crate::measured_manifest::{AdmittedIdentity, MeasuredManifest, MAX_MANIFEST_BYTES};
use crate::seal::{self, KeyPolicy, SealHeader, UnsealedTributeOfferAndGroupSig};
use crate::tsm::{self, SevSnpIdentity, TsmProvider, TsmReportSource};

/// Prefix of a blob sealed with the process key of a confidential VM.
const VM_SEAL_MAGIC: &[u8] = b"CVMS1";
/// Offset of `REPORT_DATA` in an SEV-SNP attestation report.
const REPORT_DATA_OFFSET: usize = 0x50;

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

struct Backend {
    identity: Identity,
    sealing_key: Zeroizing<[u8; 32]>,
    source: TsmReportSource,
    network_descriptor: TrustedNetworkDescriptorV1,
    manifest: MeasuredManifest,
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

/// Platform of the confidential VM that runs this process, as the kernel TSM
/// report interface names it.
pub fn detect_platform() -> EnclaveResult<ConfidentialPlatform> {
    let source = TsmReportSource::system();
    if !source.is_available() {
        return Err(ConfidentialVmError::TsmUnavailable.into());
    }
    platform_of(source.provider()?)
}

fn platform_of(provider: TsmProvider) -> EnclaveResult<ConfidentialPlatform> {
    match provider {
        TsmProvider::SevGuest => Ok(ConfidentialPlatform::SevSnp),
        TsmProvider::TdxGuest => Ok(ConfidentialPlatform::Tdx),
        other => Err(TsmError::UnsupportedProvider(other.name().to_owned()).into()),
    }
}

/// Select the confidential VM backend for this process.
///
/// Reads the launch identity of the VM, the application manifest and the
/// network descriptor. The launch identity must bind the manifest file, the
/// manifest must list the image of this VM and name the network descriptor
/// file. Every step must succeed: the service does not start with a partial
/// platform.
pub fn configure(
    platform: ConfidentialPlatform,
    network_descriptor: &Path,
    app_manifest: &Path,
) -> EnclaveResult<()> {
    let source = TsmReportSource::system();
    if !source.is_available() {
        return Err(ConfidentialVmError::TsmUnavailable.into());
    }
    let provider = source.provider()?;
    let report = source.report(&[0; tsm::REPORT_DATA_LEN])?;
    let identity = match (platform, provider) {
        (ConfidentialPlatform::SevSnp, TsmProvider::SevGuest) => {
            Identity::SevSnp(tsm::sev_snp_identity(&report.report)?)
        }
        (ConfidentialPlatform::Tdx, TsmProvider::TdxGuest) => {
            Identity::Tdx(tdx_identity(&report.report)?)
        }
        (_, other) => return Err(TsmError::UnsupportedProvider(other.name().to_owned()).into()),
    };
    let manifest = read_bounded(app_manifest, MAX_MANIFEST_BYTES)
        .map_err(|error| ConfidentialVmError::app_manifest(app_manifest.display(), error))?;
    let descriptor_bytes = std::fs::read(network_descriptor).map_err(|error| {
        ConfidentialVmError::network_descriptor(network_descriptor.display(), error)
    })?;
    let manifest = measured_manifest_of(&identity, &manifest, &descriptor_bytes)
        .map_err(|error| ConfidentialVmError::app_manifest(app_manifest.display(), error))?;
    let network_descriptor = TrustedNetworkDescriptorV1::decode_canonical(&descriptor_bytes)
        .map_err(|error| {
            ConfidentialVmError::network_descriptor(network_descriptor.display(), error)
        })?;
    BACKEND
        .set(Backend {
            identity,
            sealing_key: process_sealing_key()?,
            source,
            network_descriptor,
            manifest,
        })
        .map_err(|_| ConfidentialVmError::AlreadyConfigured.into())
}

fn read_bounded(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other(format!(
            "the file is longer than {limit} bytes"
        )));
    }
    Ok(bytes)
}

/// Read the release rules of `manifest` and check them against this VM.
fn measured_manifest_of(
    identity: &Identity,
    manifest: &[u8],
    network_descriptor: &[u8],
) -> Result<MeasuredManifest, crate::measured_manifest::MeasuredManifestError> {
    let manifest = MeasuredManifest::parse(manifest)?;
    match identity {
        Identity::SevSnp(identity) => {
            manifest.require_measured_by_host_data(&identity.host_data)?;
        }
        Identity::Tdx(identity) => {
            manifest.require_measured_by_mr_config_id(&identity.mr_config_id)?;
        }
    }
    manifest.require_own_identity(&admitted_identity_of(identity))?;
    manifest.require_network_descriptor(network_descriptor)?;
    Ok(manifest)
}

/// Make the sealing key of this process.
///
/// The key does not leave the memory of the VM and does not exist after the
/// process stops.
fn process_sealing_key() -> EnclaveResult<Zeroizing<[u8; 32]>> {
    use ring::rand::SecureRandom as _;
    let mut key = Zeroizing::new([0_u8; 32]);
    ring::rand::SystemRandom::new()
        .fill(key.as_mut())
        .map_err(|_| ConfidentialVmError::SealingKeyUnavailable)?;
    Ok(key)
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

/// Identity of a VM in the fields that the release rules compare.
fn admitted_identity_of(identity: &Identity) -> AdmittedIdentity {
    let measurements = measurements_of(identity, &[0; 64]);
    AdmittedIdentity {
        evidence_mode: match identity {
            Identity::SevSnp(_) => AttestationMode::SevSnpRequired,
            Identity::Tdx(_) => AttestationMode::TdxRequired,
        },
        mrenclave: measurements.mrenclave.into(),
        mrsigner: measurements.mrsigner.into(),
        isv_prod_id: measurements.isv_prod_id,
        isv_svn: measurements.isv_svn,
    }
}

/// Release rules of the measured manifest of this VM.
pub(crate) fn release_rules() -> Option<&'static MeasuredManifest> {
    BACKEND.get().map(|backend| &backend.manifest)
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
        .map(|backend| (*backend.sealing_key, KeyPolicy::ConfidentialVmMemory))
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
    seal_with(&backend.sealing_key, secret, extra, binding, header)
}

fn seal_with(
    key: &[u8; 32],
    secret: &[u8; 32],
    extra: &[u8],
    binding: NetworkBindingV1,
    header: &SealHeader,
) -> EnclaveResult<Vec<u8>> {
    let mut header = *header;
    header.key_policy = KeyPolicy::ConfidentialVmMemory;
    let payload = seal::seal_tribute_offer_and_group_sig(secret, extra, key, binding, &header)?;
    let mut blob = Vec::with_capacity(VM_SEAL_MAGIC.len() + payload.len());
    blob.extend_from_slice(VM_SEAL_MAGIC);
    blob.extend_from_slice(&payload);
    Ok(blob)
}

/// Open a blob that [`seal`] wrote in this process.
pub(crate) fn unseal(blob: &[u8], svn: u16) -> EnclaveResult<UnsealedTributeOfferAndGroupSig> {
    let backend = backend()?;
    unseal_with(&backend.sealing_key, blob, svn)
}

fn unseal_with(
    key: &[u8; 32],
    blob: &[u8],
    svn: u16,
) -> EnclaveResult<UnsealedTributeOfferAndGroupSig> {
    let payload = blob
        .strip_prefix(VM_SEAL_MAGIC)
        .ok_or(ConfidentialVmError::NotVmSeal)?;
    let value = seal::unseal_network_bound_payload(payload, key, svn)?;
    if value.header.key_policy != KeyPolicy::ConfidentialVmMemory {
        return Err(ConfidentialVmError::SealPolicyMismatch.into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::EnclaveError;

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
    fn seal_round_trip_and_rejections() {
        let blob = seal_with(&[4; 32], &[8; 32], b"extra", binding(), &header()).unwrap();
        assert!(blob.starts_with(VM_SEAL_MAGIC));
        let value = unseal_with(&[4; 32], &blob, SEV_SNP_RULE_SVN_V1).unwrap();
        assert_eq!(*value.tribute_offer_secret, [8; 32]);
        assert_eq!(value.group_sig.as_slice(), b"extra");
        assert_eq!(value.network_binding, binding());
        assert_eq!(value.header.key_policy, KeyPolicy::ConfidentialVmMemory);

        // Another process has another key.
        assert!(unseal_with(&[5; 32], &blob, SEV_SNP_RULE_SVN_V1).is_err());

        // A blob of another backend is not accepted.
        for foreign in [b"TSGX1".as_slice(), b"LE2E1", b"SNPS1", b"TDXS1", b""] {
            assert!(matches!(
                unseal_with(&[4; 32], foreign, SEV_SNP_RULE_SVN_V1),
                Err(EnclaveError::ConfidentialVm(ConfidentialVmError::NotVmSeal))
            ));
        }

        let mut corrupted = blob;
        let last = corrupted.len() - 1;
        corrupted[last] ^= 1;
        assert!(unseal_with(&[4; 32], &corrupted, SEV_SNP_RULE_SVN_V1).is_err());
    }

    #[test]
    fn process_sealing_keys_differ() {
        assert_ne!(
            *process_sealing_key().unwrap(),
            *process_sealing_key().unwrap()
        );
    }

    fn manifest_for(identities: &[Identity], descriptor: &[u8]) -> Vec<u8> {
        let mut snp = Vec::new();
        let mut tdx = Vec::new();
        for identity in identities {
            match identity {
                Identity::SevSnp(identity) => {
                    snp.push(format!("\"{}\"", hex::encode(identity.measurement)));
                }
                Identity::Tdx(identity) => tdx.push(format!(
                    r#"{{"mrtd":"{}","rtmr0":"{}","rtmr1":"{}","rtmr2":"{}"}}"#,
                    hex::encode(identity.mrtd),
                    hex::encode(identity.rtmrs[0]),
                    hex::encode(identity.rtmrs[1]),
                    hex::encode(identity.rtmrs[2]),
                )),
            }
        }
        format!(
            r#"{{"name":"outbe-validator","outbe_release":{{"network_descriptor_sha256":"{}","sev_snp_measurements":[{}],"tdx_images":[{}]}}}}"#,
            hex::encode(ring::digest::digest(&ring::digest::SHA256, descriptor)),
            snp.join(","),
            tdx.join(","),
        )
        .into_bytes()
    }

    /// The identities of the fixtures with the launch fields that bind
    /// `manifest`, as dstack sets them.
    fn launched_with(manifest: &[u8]) -> (Identity, Identity) {
        let hash: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, manifest)
            .as_ref()
            .try_into()
            .unwrap();
        let mut snp = snp_identity();
        snp.host_data = crate::measured_manifest::snp_host_data_of_manifest(&hash);
        let mut tdx = tdx();
        tdx.mr_config_id = crate::measured_manifest::tdx_mr_config_id_of_manifest(&hash);
        (Identity::SevSnp(snp), Identity::Tdx(tdx))
    }

    #[test]
    fn the_manifest_must_be_measured_list_this_image_and_name_the_descriptor() {
        let descriptor = b"descriptor".as_slice();
        let manifest = manifest_for(&[identity(), Identity::Tdx(tdx())], descriptor);
        let (snp, tdx_vm) = launched_with(&manifest);

        let rules = measured_manifest_of(&snp, &manifest, descriptor).unwrap();
        // A service on one platform admits the listed image of the other.
        assert!(rules.admits(&admitted_identity_of(&tdx_vm)));
        assert!(rules.admits(&admitted_identity_of(&snp)));
        measured_manifest_of(&tdx_vm, &manifest, descriptor).unwrap();

        use crate::measured_manifest::MeasuredManifestError;
        // The fixture identities carry the launch fields of another manifest.
        assert_eq!(
            measured_manifest_of(&identity(), &manifest, descriptor),
            Err(MeasuredManifestError::NotMeasured)
        );
        assert_eq!(
            measured_manifest_of(&Identity::Tdx(tdx()), &manifest, descriptor),
            Err(MeasuredManifestError::NotMeasured)
        );
        assert_eq!(
            measured_manifest_of(&snp, &manifest, b"another descriptor"),
            Err(MeasuredManifestError::NetworkDescriptorMismatch)
        );

        // A manifest that lists only the other platform does not start here.
        let tdx_only = manifest_for(&[Identity::Tdx(tdx())], descriptor);
        let (snp, _) = launched_with(&tdx_only);
        assert_eq!(
            measured_manifest_of(&snp, &tdx_only, descriptor),
            Err(MeasuredManifestError::OwnImageNotListed)
        );

        // A VM with the listed image and another manifest is not admitted.
        let (_, tdx_other_manifest) = launched_with(&tdx_only);
        assert!(!rules.admits(&admitted_identity_of(&tdx_other_manifest)));
    }

    #[test]
    fn platform_follows_the_tsm_provider() {
        assert_eq!(
            platform_of(TsmProvider::SevGuest).unwrap(),
            ConfidentialPlatform::SevSnp
        );
        assert_eq!(
            platform_of(TsmProvider::TdxGuest).unwrap(),
            ConfidentialPlatform::Tdx
        );
        assert!(platform_of(TsmProvider::Other("arm_cca_guest".to_owned())).is_err());
    }

    #[test]
    fn hooks_are_inactive_until_the_backend_is_configured() {
        assert!(!configured());
        assert!(platform().is_none());
        assert!(sealing_key().is_none());
        assert!(network_descriptor().is_none());
        assert!(release_rules().is_none());
        assert!(matches!(
            measurements(&[0; 64]),
            Err(EnclaveError::ConfidentialVm(
                ConfidentialVmError::NotConfigured
            ))
        ));
    }
}
