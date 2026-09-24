use super::is_lower_hex;
use super::GramineIdentity;

use clap::ValueEnum;
use eyre::bail;

use eyre::Result;
use eyre::WrapErr;

use outbe_primitives::chain::OutbeNetwork;
use outbe_primitives::chain::MAINNET_CHAIN_ID;
use outbe_primitives::chain::TESTNET_CHAIN_ID;

use outbe_tee::release_dcap_artifacts::ReleaseDcapArtifactSetV1;
use serde::Deserialize;
use serde::Serialize;

use std::fs;

use std::path::Path;

pub(super) const REQUIRED_BUNDLE_FILES: [&str; 9] = [
    "metadata/network-descriptor-v1.bin",
    "rootfs/opt/outbe/sgx/bin/outbe-tee-enclave",
    "rootfs/opt/outbe/sgx/gramine/libpal.so",
    "rootfs/opt/outbe/sgx/gramine/libsysdb.so",
    "rootfs/opt/outbe/sgx/gramine/loader",
    "rootfs/opt/outbe/sgx/network-descriptor-v1.bin",
    "rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest",
    "rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest.sgx",
    "rootfs/opt/outbe/sgx/outbe-tee-enclave.sig",
];

pub(super) const EXCLUDED_BUNDLE_FILES: [&str; 4] = [
    "metadata/testnet-sgx-bundle.json",
    "metadata/mainnet-sgx-bundle.json",
    "SHA256SUMS",
    "SHA256SUMS.unsigned",
];

pub(super) const GITHUB_ACTIONS_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum SgxReleaseNetwork {
    Testnet,
    Mainnet,
}

impl SgxReleaseNetwork {
    #[must_use]
    pub const fn chain_id(self) -> u64 {
        match self {
            Self::Testnet => TESTNET_CHAIN_ID,
            Self::Mainnet => MAINNET_CHAIN_ID,
        }
    }

    #[must_use]
    pub const fn chain_name(self) -> &'static str {
        match self {
            Self::Testnet => "outbe-testnet-1",
            Self::Mainnet => "outbe-mainnet-1",
        }
    }

    #[must_use]
    pub const fn authorization_scope(self) -> &'static str {
        match self {
            Self::Testnet => "testnet",
            Self::Mainnet => "mainnet",
        }
    }

    #[must_use]
    pub const fn bundle_spec_path(self) -> &'static str {
        match self {
            Self::Testnet => "release/testnet-sgx-bundle-v1.json",
            Self::Mainnet => "release/mainnet-sgx-bundle-v1.json",
        }
    }

    #[must_use]
    pub const fn bundle_manifest_path(self) -> &'static str {
        match self {
            Self::Testnet => "metadata/testnet-sgx-bundle.json",
            Self::Mainnet => "metadata/mainnet-sgx-bundle.json",
        }
    }

    #[must_use]
    pub const fn workflow_path(self) -> &'static str {
        match self {
            Self::Testnet => ".github/workflows/testnet-release.yml",
            Self::Mainnet => ".github/workflows/mainnet-release.yml",
        }
    }

    #[must_use]
    pub const fn certificate_identity(self) -> &'static str {
        match self {
            Self::Testnet => "https://github.com/outbe/outbe-chain/.github/workflows/testnet-release.yml@refs/heads/main",
            Self::Mainnet => "https://github.com/outbe/outbe-chain/.github/workflows/mainnet-release.yml@refs/heads/main",
        }
    }

    #[must_use]
    pub const fn genesis_artifact_name(self) -> &'static str {
        self.dcap_artifact_set().genesis_artifact_path()
    }

    #[must_use]
    pub const fn outbe_network(self) -> OutbeNetwork {
        match self {
            Self::Testnet => OutbeNetwork::Testnet,
            Self::Mainnet => OutbeNetwork::Mainnet,
        }
    }

    #[must_use]
    pub const fn dcap_artifact_set(self) -> ReleaseDcapArtifactSetV1 {
        match ReleaseDcapArtifactSetV1::for_network(self.outbe_network()) {
            Some(contract) => contract,
            None => unreachable!(),
        }
    }

    #[must_use]
    pub const fn oci_name(self) -> &'static str {
        match self {
            Self::Testnet => "outbe-tee-enclave-testnet",
            Self::Mainnet => "outbe-tee-enclave-mainnet",
        }
    }

    fn from_spec(spec: &BundleSpec) -> Result<Self> {
        let network = match spec.network.as_str() {
            "testnet" => Self::Testnet,
            "mainnet" => Self::Mainnet,
            _ => {
                bail!("unsupported SGX release network {}", spec.network);
            }
        };
        if spec.chain_id != network.chain_id()
            || spec.network_name != network.chain_name()
            || spec.authorization_scope != network.authorization_scope()
        {
            bail!("SGX bundle network identity is inconsistent");
        }
        Ok(network)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct BundleSpec {
    pub authorization_scope: String,
    pub bundle_version: u32,
    pub chain_id: u64,
    pub gramine: GramineIdentity,
    pub inputs: Vec<String>,
    pub install_root: String,
    pub network: String,
    pub network_name: String,
    pub platform: String,
    pub project_toolchain: String,
    pub sealed_state_schema: u32,
    pub sgx: SgxPolicy,
    pub spec_version: u32,
}

impl BundleSpec {
    pub fn read(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .wrap_err_with(|| format!("read SGX bundle spec metadata: {}", path.display()))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            bail!("missing or unsafe SGX bundle spec: {}", path.display());
        }
        let bytes =
            fs::read(path).wrap_err_with(|| format!("read SGX bundle spec: {}", path.display()))?;
        let spec: Self = serde_json::from_slice(&bytes)
            .wrap_err_with(|| format!("parse SGX bundle spec: {}", path.display()))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<()> {
        if self.spec_version != 1 || self.bundle_version != 1 {
            bail!("unsupported SGX bundle contract");
        }
        SgxReleaseNetwork::from_spec(self)?;
        let Some((image, digest)) = self.gramine.builder_image.split_once("@sha256:") else {
            bail!("Gramine builder image must be pinned by sha256 digest");
        };
        if image.is_empty() || image.chars().any(char::is_whitespace) || !is_lower_hex(digest, 64) {
            bail!("Gramine builder image must be pinned by sha256 digest");
        }
        if !is_lower_hex(&self.gramine.source_commit, 40) {
            bail!("Gramine source commit must be a lowercase 40-character Git SHA");
        }
        if self.platform != "linux/amd64" {
            bail!("SGX bundle supports only linux/amd64");
        }
        if self.project_toolchain != "release/project-toolchain-v1.json" {
            bail!("SGX bundle must bind the project toolchain version pin");
        }
        if self.install_root != "/opt/outbe/sgx" {
            bail!("SGX install root must remain /opt/outbe/sgx");
        }
        if self.sealed_state_schema != u32::from(outbe_tee::SEALED_STATE_SCHEMA_V1) {
            bail!("SGX bundle sealed-state schema does not match the enclave wire format");
        }
        if self.sgx.debug {
            bail!("release SGX bundle must use a non-debug enclave");
        }
        if self.sgx.remote_attestation != "dcap" {
            bail!("production SGX bundle must enable DCAP remote attestation");
        }
        if self.sgx.minimum_tcb_evaluation_data_number == 0 {
            bail!(
                "production SGX bundle must pin a non-zero minimum Intel TCB evaluation data number"
            );
        }
        if self.sgx.sigstruct_date_source != "source-date-epoch-utc" {
            bail!("SIGSTRUCT date must derive from SOURCE_DATE_EPOCH in UTC");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SgxPolicy {
    pub debug: bool,
    pub edmm_enable: bool,
    pub isv_prod_id: u16,
    pub isv_svn: u16,
    pub max_threads: u32,
    pub minimum_tcb_evaluation_data_number: u32,
    pub remote_attestation: String,
    pub sigstruct_date_source: String,
}
