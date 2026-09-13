use crate::world::ocomp::*;

impl OcompTopology {
    /// Prove that release OCOMP roles use the scenario base directory and the
    /// canonical `validator-N/ocomp/domain-v1` layout. This is intentionally a
    /// path contract, not a service-UID contract: all harness roles run as the
    /// launching user.
    #[cfg(feature = "ocomp-integration")]
    pub fn verify_release_basedir_contract(&self) -> Result<()> {
        for validator_index in self.validator_indices()? {
            let expected = self
                .cfg
                .validator_dir(usize::from(validator_index))
                .join("ocomp")
                .join("domain-v1");
            let actual = self.domain_root(validator_index)?;
            eyre::ensure!(
                actual == expected,
                "validator-{validator_index} OCOMP root {} differs from basedir contract {}",
                actual.display(),
                expected.display()
            );
            for required in [
                "protocol-bundle-v1.ocb1",
                "ocomp-key-v1.hex",
                "ocomp-evm-key.hex",
            ] {
                eyre::ensure!(
                    actual.join(required).is_file(),
                    "validator-{validator_index} basedir is missing {required}"
                );
            }
        }
        Ok(())
    }

    /// Canonical chain manifest selected for this scenario before any
    /// mismatched-install fault is injected.
    #[cfg(feature = "ocomp-integration")]
    #[must_use]
    pub fn canonical_chain_manifest_path(&self) -> PathBuf {
        self.cfg.dir.join("genesis.json")
    }

    #[cfg(feature = "ocomp-integration")]
    pub fn canonical_fork_install(&self) -> Result<OcompForkInstallV1> {
        let spec = parse_outbe_chain_spec(&self.canonical_chain_manifest_path())?;
        Ok(
            outbe_node::ocomp::fork::require_startup_ocomp_fork_install(&spec)?
                .as_ref()
                .clone(),
        )
    }

    /// Network identity pinned when the genesis validator roles were started.
    #[cfg(feature = "ocomp-integration")]
    #[must_use]
    pub fn launch_identity(&self) -> Option<OcompLaunchIdentityV1> {
        self.launch_identity
    }

    #[cfg(feature = "ocomp-integration")]
    pub(in crate::world::ocomp) fn require_launch_identity(
        &self,
        identity: OcompLaunchIdentityV1,
    ) -> Result<()> {
        match self.launch_identity {
            Some(established) if established == identity => Ok(()),
            Some(_) => {
                eyre::bail!("OCOMP worker launch identity differs from the scenario network");
            }
            None => {
                eyre::bail!("OCOMP validator roles must start before workers");
            }
        }
    }
}

/// Fixed process roles represented in one validator's OCOMP domain.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OcompProcessRole {
    SnapshotExporter,
    Worker,
}

/// Typed fault operations available to OCOMP scenarios.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OcompProcessFault {
    StopSnapshotExporter {
        validator_index: u8,
    },
    StopWorker {
        validator_index: u8,
        worker_ordinal: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OcompLaunchIdentityEvidenceV1 {
    pub chain_id: u64,
    pub genesis_hash: String,
    pub protocol_bundle_hash: String,
    pub fork_install_hash: String,
    pub classification: String,
    pub activation_height: u64,
    pub metadosis_storage_layout_hash: String,
}

/// Exact chain/bundle identity shared by one measurement network.
#[cfg(feature = "ocomp-integration")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OcompLaunchIdentityV1 {
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub protocol_bundle_hash: B256,
    pub fork_install_hash: B256,
    pub classification: OcompForkInstallClassification,
    pub activation_height: u64,
    pub metadosis_storage_layout_hash: B256,
}
