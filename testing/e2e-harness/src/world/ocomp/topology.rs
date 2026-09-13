use crate::world::ocomp::*;

#[derive(Debug)]
pub(super) struct OwnedProcess {
    pub(super) guard: ChildGuard,
    pub(super) record_index: usize,
}

#[derive(Debug)]
pub(super) struct OcompDomain {
    pub(super) root: PathBuf,
    pub(super) snapshot_exporter: Option<OwnedProcess>,
    pub(super) workers: BTreeMap<u32, OwnedProcess>,
}

impl OcompDomain {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            snapshot_exporter: None,
            workers: BTreeMap::new(),
        }
    }
}

/// Sole scenario owner of one isolated compute domain per configured validator.
#[derive(Debug)]
pub struct OcompTopology {
    pub(super) cfg: Config,
    pub(super) domains: Vec<OcompDomain>,
    /// One synchronized, non-voting FullNode compute domain awaiting canonical
    /// validator admission. It is deliberately outside `domains`, whose order
    /// is the ACTIVE OCOMP membership asserted by the harness.
    pub(super) keyless_full_node_domain: Option<(u8, OcompDomain)>,
    pub(super) records: Vec<OcompProcessRecordV1>,
    pub(super) faults: Vec<OcompFaultRecordV1>,
    pub(super) launch_identity_evidence: Option<OcompLaunchIdentityEvidenceV1>,
    pub(super) fork_restart_evidence: Option<OcompForkRestartEvidenceV1>,
    pub(super) fork_mismatch_evidence: Option<OcompForkMismatchEvidenceV1>,
    #[cfg(feature = "ocomp-integration")]
    pub(super) launch_identity: Option<OcompLaunchIdentityV1>,
    #[cfg(feature = "ocomp-integration")]
    pub(super) successor_identity: Option<OcompLaunchIdentityV1>,
    #[cfg(feature = "ocomp-integration")]
    pub(super) artifact_phases: BTreeMap<B256, OcompArtifactPhase>,
    pub(super) tribute_correlation: TributeCorrelationBuilder,
    pub(super) correlated_tribute: Option<CorrelatedTributeFixtureV1>,
}

#[cfg(feature = "ocomp-integration")]
impl Drop for OcompTopology {
    fn drop(&mut self) {
        if let Some((_, domain)) = self.keyless_full_node_domain.as_mut() {
            domain.workers.clear();
            domain.snapshot_exporter.take();
        }
        for domain in &mut self.domains {
            domain.workers.clear();
            domain.snapshot_exporter.take();
        }
    }
}

impl OcompTopology {
    pub(crate) fn new(cfg: Config) -> Self {
        let domains = (0..cfg.validators)
            .map(|index| OcompDomain::new(cfg.validator_dir(index).join("ocomp").join("domain-v1")))
            .collect();
        let tribute_correlation = TributeCorrelationBuilder::new(cfg.validators)
            .expect("harness validator count must fit the validator index format");
        Self {
            domains,
            keyless_full_node_domain: None,
            cfg,
            records: Vec::new(),
            faults: Vec::new(),
            launch_identity_evidence: None,
            fork_restart_evidence: None,
            fork_mismatch_evidence: None,
            #[cfg(feature = "ocomp-integration")]
            launch_identity: None,
            #[cfg(feature = "ocomp-integration")]
            successor_identity: None,
            #[cfg(feature = "ocomp-integration")]
            artifact_phases: BTreeMap::new(),
            tribute_correlation,
            correlated_tribute: None,
        }
    }

    /// Extend the process topology after the canonical ValidatorSet has
    /// activated exactly the next ordered validator. This allocates local
    /// harness resources only; chain membership remains authoritative.
    pub fn add_active_validator_domain(&mut self, validator_index: u8) -> Result<()> {
        let expected = self.domains.len();
        eyre::ensure!(
            usize::from(validator_index) == expected,
            "active validator domain must append at index {expected}"
        );
        let domain = match self.keyless_full_node_domain.take() {
            Some((index, domain)) => {
                eyre::ensure!(
                    index == validator_index,
                    "staged FullNode domain belongs to validator-{index}, not validator-{validator_index}"
                );
                eyre::ensure!(
                    domain.snapshot_exporter.is_none() && domain.workers.is_empty(),
                    "keyless FullNode roles must stop before validator activation"
                );
                domain
            }
            None => OcompDomain::new(
                self.cfg
                    .validator_dir(expected)
                    .join("ocomp")
                    .join("domain-v1"),
            ),
        };
        self.domains.push(domain);
        Ok(())
    }

    /// Stage the compute-only profile required by an OCOMP-enabled FullNode.
    /// The node owns the embedded Supervisor and its Worker endpoint, so this
    /// returns no node CLI arguments. The durable domain remains outside the
    /// ACTIVE voting topology and intentionally contains no voting keys.
    #[cfg(feature = "ocomp-integration")]
    pub fn stage_keyless_full_node_domain(&mut self, validator_index: u8) -> Result<Vec<String>> {
        let index = usize::from(validator_index);
        eyre::ensure!(
            index == self.domains.len(),
            "keyless FullNode must use the next ordered validator slot"
        );
        eyre::ensure!(
            self.keyless_full_node_domain.is_none(),
            "a keyless FullNode domain is already staged"
        );
        self.launch_identity
            .ok_or_else(|| eyre::eyre!("OCOMP launch identity is not established"))?;
        let source_bundle = self.domain_root(0)?.join("protocol-bundle-v1.ocb1");
        let root = self
            .cfg
            .validator_dir(index)
            .join("ocomp")
            .join("domain-v1");
        fs::create_dir_all(&root)?;
        publish_exact_file(
            &root.join("protocol-bundle-v1.ocb1"),
            &fs::read(source_bundle)?,
            0o640,
        )?;
        if let Some(identity) = self.launch_identity {
            publish_bundle_catalog_entry(
                &root,
                identity.protocol_bundle_hash,
                &fs::read(root.join("protocol-bundle-v1.ocb1"))?,
            )?;
        }
        eyre::ensure!(
            !root.join("ocomp-key-v1.hex").exists() && !root.join("ocomp-evm-key.hex").exists(),
            "keyless FullNode domain contains validator voting material"
        );
        self.keyless_full_node_domain = Some((validator_index, OcompDomain::new(root)));

        Ok(Vec::new())
    }

    /// Scenario-owned root for one validator domain.
    pub fn domain_root(&self, validator_index: u8) -> Result<&Path> {
        Ok(&self.domain(validator_index)?.root)
    }

    /// Compute ownership lookup only. Validator membership/enumeration continues
    /// to use `domains` and must never include the keyless FullNode.
    #[cfg(feature = "ocomp-integration")]
    pub(super) fn compute_domain_mut(&mut self, index: u8) -> Result<&mut OcompDomain> {
        if self
            .keyless_full_node_domain
            .as_ref()
            .is_some_and(|(slot, _)| *slot == index)
        {
            return self.keyless_full_node_domain_mut(index);
        }
        self.domain_mut(index)
    }

    pub(super) fn domain(&self, validator_index: u8) -> Result<&OcompDomain> {
        self.domains
            .get(usize::from(validator_index))
            .ok_or_else(|| eyre::eyre!("validator index is outside the configured topology"))
    }

    pub(super) fn domain_mut(&mut self, validator_index: u8) -> Result<&mut OcompDomain> {
        self.domains
            .get_mut(usize::from(validator_index))
            .ok_or_else(|| eyre::eyre!("validator index is outside the configured topology"))
    }

    #[cfg(feature = "ocomp-integration")]
    pub(super) fn keyless_full_node_domain(&self, validator_index: u8) -> Result<&OcompDomain> {
        match self.keyless_full_node_domain.as_ref() {
            Some((index, domain)) if *index == validator_index => Ok(domain),
            _ => Err(eyre::eyre!(
                "validator-{validator_index} has no staged keyless FullNode domain"
            )),
        }
    }

    #[cfg(feature = "ocomp-integration")]
    pub(super) fn keyless_full_node_domain_mut(
        &mut self,
        validator_index: u8,
    ) -> Result<&mut OcompDomain> {
        match self.keyless_full_node_domain.as_mut() {
            Some((index, domain)) if *index == validator_index => Ok(domain),
            _ => Err(eyre::eyre!(
                "validator-{validator_index} has no staged keyless FullNode domain"
            )),
        }
    }

    pub(super) fn validator_indices(&self) -> Result<Vec<u8>> {
        (0..self.domains.len())
            .map(|index| {
                u8::try_from(index)
                    .map_err(|_| eyre::eyre!("validator index exceeds the harness wire format"))
            })
            .collect()
    }
}
