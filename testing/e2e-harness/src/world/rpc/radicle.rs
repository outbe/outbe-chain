use crate::world::rpc::*;

impl Rpc {
    /// Immutable Radicle integration status as published by validator `port`.
    pub fn radicle_status(&self, port: u16) -> Option<serde_json::Value> {
        eth::raw_json(&self.url(port), "outbe_radicleStatus")
    }

    /// Canonical signed endpoint evidence known by validator `port`.
    pub fn radicle_peers(&self, port: u16) -> Option<serde_json::Value> {
        eth::raw_json(&self.url(port), "outbe_radiclePeers")
    }

    /// Finalized desired repositories and their local availability state.
    pub fn radicle_repositories(&self, port: u16) -> Option<serde_json::Value> {
        eth::raw_json(&self.url(port), "outbe_radicleRepositories")
    }

    /// Permissionlessly register one public Heartwood repository.
    pub fn register_radicle_repository(&self, key: &str, repo_id: [u8; 20]) -> Result<String> {
        let tx = eth::send_call(
            &self.cfg.rpc0,
            outbe_primitives::addresses::RADICLE_REGISTRY_ADDRESS,
            key,
            &IRadicleRegistry::registerRepositoryCall {
                repoId: repo_id.into(),
            },
            None,
        )?;
        if !self.wait_successful_receipt(&tx, 240) {
            return Err(eyre!(
                "Radicle repository registration receipt was not successful: {tx}"
            ));
        }
        Ok(tx)
    }

    /// `(count, generation, maximum)` from the finalized repository registry view.
    pub fn radicle_registry_state(&self) -> Option<(u32, u64, u32)> {
        let address = outbe_primitives::addresses::RADICLE_REGISTRY_ADDRESS;
        let count: u32 = eth::read_call(
            &self.cfg.rpc0,
            address,
            &IRadicleRegistry::repositoryCountCall {},
        )?;
        let generation: u64 = eth::read_call(
            &self.cfg.rpc0,
            address,
            &IRadicleRegistry::registryGenerationCall {},
        )?;
        let maximum: u32 = eth::read_call(
            &self.cfg.rpc0,
            address,
            &IRadicleRegistry::maxRepositoriesCall {},
        )?;
        Some((count, generation, maximum))
    }
}
