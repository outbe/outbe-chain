//! Release rules of a service that runs in a confidential VM.
//!
//! The service gives the offer key to another VM. It must not take the rules
//! for that decision from its node: the operator of the VM controls the node,
//! the chain state that the node reads and the attestation policy in that
//! state. The service takes the rules from the application manifest of its VM.
//! The processor measures the manifest, so the operator cannot change it
//! without a change of the launch identity.
//!
//! dstack binds the SHA-256 hash of `app-compose.json` to the launch identity:
//!
//! | Platform | Field | Value |
//! |---|---|---|
//! | TDX | `MRCONFIGID` | `0x01`, the manifest hash, 15 zero bytes |
//! | SEV-SNP | `HOST_DATA` | SHA-256 of a document that names the manifest hash |
//!
//! The service reads the manifest file, checks its hash against its own launch
//! identity and reads the `outbe_release` section:
//!
//! ```json
//! "outbe_release": {
//!   "network_descriptor_sha256": "<64 hex digits>",
//!   "sev_snp_measurements": ["<96 hex digits>"],
//!   "tdx_images": [{"mrtd": "…", "rtmr0": "…", "rtmr1": "…", "rtmr2": "…"}]
//! }
//! ```
//!
//! A VM receives the key only if its image is in one of the two lists and its
//! manifest is this manifest. The lists hold the images of both platforms and
//! of every VM shape, so a service on one platform can admit a VM on the
//! other. The manifest does not contain its own hash: the image measurements
//! do not depend on the manifest.

use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::{
    sev_snp_measurement_rule_value, tdx_image_rule_value, tdx_mrconfigid_rule_value,
    AttestationMode, SEV_SNP_RULE_PRODUCT_ID_V1, SEV_SNP_RULE_SVN_V1, TDX_RULE_PRODUCT_ID_V1,
    TDX_RULE_SVN_V1,
};
use ring::digest;
use serde::Deserialize;

/// Upper bound on the manifest file.
pub(crate) const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
/// Upper bound on the images of one platform in the manifest.
const MAX_RELEASE_IMAGES: usize = 16;
/// Domain of the dstack document hash that becomes `HOST_DATA`.
const DSTACK_DOCUMENT_HASH_DOMAIN: &[u8] = b"dstack-mr-config-v3:";
/// Version byte of a TDX `MRCONFIGID` that carries the manifest hash directly.
const DSTACK_MRCONFIGID_COMPOSE_HASH_VERSION: u8 = 1;

/// Failure to read the release rules from the manifest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MeasuredManifestError {
    /// The file is not a JSON object with a valid `outbe_release` section.
    #[error("the application manifest has no valid outbe_release section: {0}")]
    Malformed(String),
    /// A list has no entry or too many entries.
    #[error("the application manifest lists {0} images of one platform")]
    ImageCount(usize),
    /// The launch identity of this VM does not bind this manifest file.
    #[error("the launch identity of this VM does not bind the application manifest file")]
    NotMeasured,
    /// The image of this VM is not in the release list of its own manifest.
    #[error("the image of this VM is not in the release list of its application manifest")]
    OwnImageNotListed,
    /// The network descriptor file is not the one that the manifest names.
    #[error("the network descriptor is not the one that the application manifest names")]
    NetworkDescriptorMismatch,
}

#[derive(Deserialize)]
struct ManifestFile {
    outbe_release: ReleaseSection,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseSection {
    network_descriptor_sha256: String,
    sev_snp_measurements: Vec<String>,
    tdx_images: Vec<TdxImageText>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TdxImageText {
    mrtd: String,
    rtmr0: String,
    rtmr1: String,
    rtmr2: String,
}

/// Launch identity that the release rules admit, in the fields of a verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AdmittedIdentity {
    /// Attestation mode of the evidence: the platform of the VM.
    pub(crate) evidence_mode: AttestationMode,
    pub(crate) mrenclave: B256,
    pub(crate) mrsigner: B256,
    pub(crate) isv_prod_id: u16,
    pub(crate) isv_svn: u16,
}

/// Release rules that the measured application manifest carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MeasuredManifest {
    manifest_hash: [u8; 32],
    network_descriptor_sha256: [u8; 32],
    admitted: Vec<AdmittedIdentity>,
}

impl MeasuredManifest {
    /// Read the release rules from the bytes of `app-compose.json`.
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, MeasuredManifestError> {
        let file: ManifestFile = serde_json::from_slice(bytes)
            .map_err(|error| MeasuredManifestError::Malformed(error.to_string()))?;
        let section = file.outbe_release;
        let manifest_hash = sha256(bytes);
        let snp_host_data = B256::from(snp_host_data_of_manifest(&manifest_hash));
        let tdx_config = tdx_mrconfigid_rule_value(&tdx_mr_config_id_of_manifest(&manifest_hash));

        for count in [section.sev_snp_measurements.len(), section.tdx_images.len()] {
            if count > MAX_RELEASE_IMAGES {
                return Err(MeasuredManifestError::ImageCount(count));
            }
        }
        let mut admitted = Vec::new();
        for measurement in &section.sev_snp_measurements {
            admitted.push(AdmittedIdentity {
                evidence_mode: AttestationMode::SevSnpRequired,
                mrenclave: sev_snp_measurement_rule_value(&hex_array(measurement)?),
                mrsigner: snp_host_data,
                isv_prod_id: SEV_SNP_RULE_PRODUCT_ID_V1,
                isv_svn: SEV_SNP_RULE_SVN_V1,
            });
        }
        for image in &section.tdx_images {
            let rtmrs = [
                hex_array(&image.rtmr0)?,
                hex_array(&image.rtmr1)?,
                hex_array(&image.rtmr2)?,
            ];
            admitted.push(AdmittedIdentity {
                evidence_mode: AttestationMode::TdxRequired,
                mrenclave: tdx_image_rule_value(&hex_array(&image.mrtd)?, &rtmrs),
                mrsigner: tdx_config,
                isv_prod_id: TDX_RULE_PRODUCT_ID_V1,
                isv_svn: TDX_RULE_SVN_V1,
            });
        }
        if admitted.is_empty() {
            return Err(MeasuredManifestError::ImageCount(0));
        }
        Ok(Self {
            manifest_hash,
            network_descriptor_sha256: hex_array(&section.network_descriptor_sha256)?,
            admitted,
        })
    }

    /// Check that the SEV-SNP host data of this VM binds this manifest.
    pub(crate) fn require_measured_by_host_data(
        &self,
        host_data: &[u8; 32],
    ) -> Result<(), MeasuredManifestError> {
        if *host_data == snp_host_data_of_manifest(&self.manifest_hash) {
            Ok(())
        } else {
            Err(MeasuredManifestError::NotMeasured)
        }
    }

    /// Check that the `MRCONFIGID` of this trust domain binds this manifest.
    pub(crate) fn require_measured_by_mr_config_id(
        &self,
        mr_config_id: &[u8; 48],
    ) -> Result<(), MeasuredManifestError> {
        if *mr_config_id == tdx_mr_config_id_of_manifest(&self.manifest_hash) {
            Ok(())
        } else {
            Err(MeasuredManifestError::NotMeasured)
        }
    }

    /// Check that the release list holds the identity of this VM.
    pub(crate) fn require_own_identity(
        &self,
        identity: &AdmittedIdentity,
    ) -> Result<(), MeasuredManifestError> {
        if self.admits(identity) {
            Ok(())
        } else {
            Err(MeasuredManifestError::OwnImageNotListed)
        }
    }

    /// Check that `descriptor` is the network descriptor file of the manifest.
    pub(crate) fn require_network_descriptor(
        &self,
        descriptor: &[u8],
    ) -> Result<(), MeasuredManifestError> {
        if sha256(descriptor) == self.network_descriptor_sha256 {
            Ok(())
        } else {
            Err(MeasuredManifestError::NetworkDescriptorMismatch)
        }
    }

    /// True if a VM with this verified identity can receive the offer key.
    pub(crate) fn admits(&self, identity: &AdmittedIdentity) -> bool {
        self.admitted.contains(identity)
    }
}

/// `HOST_DATA` that dstack gives to an SEV-SNP VM with this manifest.
///
/// dstack hashes a JSON document in canonical form. For a manifest without a
/// key provider and without an instance identifier the document has four
/// fields, and the application identifier is the first 20 bytes of the
/// manifest hash.
pub(crate) fn snp_host_data_of_manifest(manifest_hash: &[u8; 32]) -> [u8; 32] {
    let document = format!(
        r#"{{"app_id":"{}","compose_hash":"{}","key_provider":"none","version":3}}"#,
        hex::encode(&manifest_hash[..20]),
        hex::encode(manifest_hash),
    );
    let mut context = digest::Context::new(&digest::SHA256);
    context.update(DSTACK_DOCUMENT_HASH_DOMAIN);
    context.update(&[0]);
    context.update(document.as_bytes());
    digest_array(context.finish().as_ref())
}

/// `MRCONFIGID` that dstack gives to a trust domain with this manifest.
pub(crate) fn tdx_mr_config_id_of_manifest(manifest_hash: &[u8; 32]) -> [u8; 48] {
    let mut config_id = [0_u8; 48];
    config_id[0] = DSTACK_MRCONFIGID_COMPOSE_HASH_VERSION;
    config_id[1..33].copy_from_slice(manifest_hash);
    config_id
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    digest_array(digest::digest(&digest::SHA256, bytes).as_ref())
}

fn digest_array(digest: &[u8]) -> [u8; 32] {
    let mut out = [0_u8; 32];
    out.copy_from_slice(digest);
    out
}

fn hex_array<const N: usize>(text: &str) -> Result<[u8; N], MeasuredManifestError> {
    let bytes = hex::decode(text)
        .map_err(|_| MeasuredManifestError::Malformed("a value is not hexadecimal".to_owned()))?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        MeasuredManifestError::Malformed(format!("a value has {} bytes, not {N}", bytes.len()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Manifest hash and launch identities of one manifest, read from an Intel
    /// TDX server and from an AMD SEV-SNP server.
    const HARDWARE_MANIFEST_HASH: &str =
        "f1015ee8bd4301452477ef06625dd7818c28e072d5cccc18398eb8a407174703";
    const HARDWARE_SNP_HOST_DATA: &str =
        "6dbc0f26557f512109ca25d986040b9f06d8bacd128735ddccb3226ede817426";
    const HARDWARE_TDX_MR_CONFIG_ID: &str = "01f1015ee8bd4301452477ef06625dd7818c28e072d5cccc18398eb8a407174703000000000000000000000000000000";

    const SNP_MEASUREMENT: [u8; 48] = [0x31; 48];
    const TDX_MRTD: [u8; 48] = [0x41; 48];
    const TDX_RTMRS: [[u8; 48]; 3] = [[0x42; 48], [0x43; 48], [0x44; 48]];
    const DESCRIPTOR: &[u8] = b"network descriptor";

    fn manifest_text(extra: &str) -> String {
        format!(
            r#"{{"manifest_version":2,"name":"outbe-validator"{extra},"outbe_release":{{"network_descriptor_sha256":"{}","sev_snp_measurements":["{}"],"tdx_images":[{{"mrtd":"{}","rtmr0":"{}","rtmr1":"{}","rtmr2":"{}"}}]}}}}"#,
            hex::encode(sha256(DESCRIPTOR)),
            hex::encode(SNP_MEASUREMENT),
            hex::encode(TDX_MRTD),
            hex::encode(TDX_RTMRS[0]),
            hex::encode(TDX_RTMRS[1]),
            hex::encode(TDX_RTMRS[2]),
        )
    }

    fn manifest() -> (MeasuredManifest, [u8; 32]) {
        let text = manifest_text("");
        (
            MeasuredManifest::parse(text.as_bytes()).unwrap(),
            sha256(text.as_bytes()),
        )
    }

    fn snp_identity(hash: &[u8; 32]) -> AdmittedIdentity {
        AdmittedIdentity {
            evidence_mode: AttestationMode::SevSnpRequired,
            mrenclave: sev_snp_measurement_rule_value(&SNP_MEASUREMENT),
            mrsigner: B256::from(snp_host_data_of_manifest(hash)),
            isv_prod_id: SEV_SNP_RULE_PRODUCT_ID_V1,
            isv_svn: SEV_SNP_RULE_SVN_V1,
        }
    }

    fn tdx_identity(hash: &[u8; 32]) -> AdmittedIdentity {
        AdmittedIdentity {
            evidence_mode: AttestationMode::TdxRequired,
            mrenclave: tdx_image_rule_value(&TDX_MRTD, &TDX_RTMRS),
            mrsigner: tdx_mrconfigid_rule_value(&tdx_mr_config_id_of_manifest(hash)),
            isv_prod_id: TDX_RULE_PRODUCT_ID_V1,
            isv_svn: TDX_RULE_SVN_V1,
        }
    }

    #[test]
    fn launch_identities_equal_the_values_of_real_hardware() {
        let hash: [u8; 32] = hex_array(HARDWARE_MANIFEST_HASH).unwrap();
        assert_eq!(
            hex::encode(snp_host_data_of_manifest(&hash)),
            HARDWARE_SNP_HOST_DATA
        );
        assert_eq!(
            hex::encode(tdx_mr_config_id_of_manifest(&hash)),
            HARDWARE_TDX_MR_CONFIG_ID
        );
    }

    #[test]
    fn admits_the_listed_images_of_both_platforms_with_this_manifest() {
        let (manifest, hash) = manifest();
        assert!(manifest.admits(&snp_identity(&hash)));
        assert!(manifest.admits(&tdx_identity(&hash)));
        manifest.require_own_identity(&snp_identity(&hash)).unwrap();
        manifest
            .require_measured_by_host_data(&snp_host_data_of_manifest(&hash))
            .unwrap();
        manifest
            .require_measured_by_mr_config_id(&tdx_mr_config_id_of_manifest(&hash))
            .unwrap();
    }

    #[test]
    fn rejects_another_manifest_another_image_and_another_platform_tag() {
        let (manifest, hash) = manifest();
        let other_hash = sha256(manifest_text(r#","public_logs":true"#).as_bytes());
        assert_ne!(hash, other_hash);
        assert!(!manifest.admits(&snp_identity(&other_hash)));
        assert!(!manifest.admits(&tdx_identity(&other_hash)));

        let mut other_image = snp_identity(&hash);
        other_image.mrenclave = sev_snp_measurement_rule_value(&[0x32; 48]);
        assert!(!manifest.admits(&other_image));
        assert_eq!(
            manifest.require_own_identity(&other_image),
            Err(MeasuredManifestError::OwnImageNotListed)
        );

        let mut other_platform = snp_identity(&hash);
        other_platform.evidence_mode = AttestationMode::TdxRequired;
        assert!(!manifest.admits(&other_platform));
        let mut sgx = snp_identity(&hash);
        sgx.evidence_mode = AttestationMode::DcapRequired;
        assert!(!manifest.admits(&sgx));
    }

    #[test]
    fn a_launch_identity_of_another_manifest_does_not_measure_this_file() {
        let (manifest, _) = manifest();
        assert_eq!(
            manifest.require_measured_by_host_data(&[0x55; 32]),
            Err(MeasuredManifestError::NotMeasured)
        );
        let mut version_three = tdx_mr_config_id_of_manifest(&manifest.manifest_hash);
        version_three[0] = 3;
        assert_eq!(
            manifest.require_measured_by_mr_config_id(&version_three),
            Err(MeasuredManifestError::NotMeasured)
        );
        assert_eq!(
            manifest.require_measured_by_mr_config_id(&[0; 48]),
            Err(MeasuredManifestError::NotMeasured)
        );
    }

    #[test]
    fn binds_the_network_descriptor_file() {
        let (manifest, _) = manifest();
        manifest.require_network_descriptor(DESCRIPTOR).unwrap();
        assert_eq!(
            manifest.require_network_descriptor(b"another descriptor"),
            Err(MeasuredManifestError::NetworkDescriptorMismatch)
        );
    }

    #[test]
    fn rejects_malformed_release_sections() {
        let no_section = br#"{"manifest_version":2,"name":"outbe-validator"}"#;
        assert!(matches!(
            MeasuredManifest::parse(no_section),
            Err(MeasuredManifestError::Malformed(_))
        ));
        let empty = format!(
            r#"{{"outbe_release":{{"network_descriptor_sha256":"{}","sev_snp_measurements":[],"tdx_images":[]}}}}"#,
            "00".repeat(32)
        );
        assert_eq!(
            MeasuredManifest::parse(empty.as_bytes()),
            Err(MeasuredManifestError::ImageCount(0))
        );
        let short = format!(
            r#"{{"outbe_release":{{"network_descriptor_sha256":"{}","sev_snp_measurements":["3131"],"tdx_images":[]}}}}"#,
            "00".repeat(32)
        );
        assert!(matches!(
            MeasuredManifest::parse(short.as_bytes()),
            Err(MeasuredManifestError::Malformed(_))
        ));
        let unknown = format!(
            r#"{{"outbe_release":{{"network_descriptor_sha256":"{}","sev_snp_measurements":[],"tdx_images":[],"allow_any":true}}}}"#,
            "00".repeat(32)
        );
        assert!(matches!(
            MeasuredManifest::parse(unknown.as_bytes()),
            Err(MeasuredManifestError::Malformed(_))
        ));
        let many = format!(
            r#"{{"outbe_release":{{"network_descriptor_sha256":"{}","sev_snp_measurements":[{}],"tdx_images":[]}}}}"#,
            "00".repeat(32),
            vec![format!("\"{}\"", "31".repeat(48)); MAX_RELEASE_IMAGES + 1].join(",")
        );
        assert_eq!(
            MeasuredManifest::parse(many.as_bytes()),
            Err(MeasuredManifestError::ImageCount(MAX_RELEASE_IMAGES + 1))
        );
    }
}
