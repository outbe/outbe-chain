//! Fixed-checkpoint EVM state observations for scenarios.

use eyre::{bail, eyre, Result};
use outbe_primitives::wwd_entity_id::WwdEntityId;

use crate::internal::config::Config;

#[derive(Debug)]
pub struct ProjectionFixture {
    cfg: Config,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedTribute {
    pub raw_id: WwdEntityId,
    pub stored_body: Vec<u8>,
}

/// One ciphertext record and both indexes read from one EVM checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TributeProjectionSnapshot {
    pub tribute: ProjectedTribute,
    pub owner_ids: Vec<WwdEntityId>,
    pub day_ids: Vec<WwdEntityId>,
}

pub(crate) fn projection_fixture(cfg: &Config) -> ProjectionFixture {
    ProjectionFixture { cfg: cfg.clone() }
}

impl ProjectionFixture {
    fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(Config) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let cfg = self.cfg.clone();
        std::thread::spawn(move || operation(cfg))
            .join()
            .map_err(|_| eyre!("projection fixture worker panicked"))?
    }

    pub fn wait_for_tribute_projection(&self, tx_hash: &str, tries: u32) -> Result<()> {
        self.wait_for_tribute_projection_on_nodes(tx_hash, tries, self.cfg.validators)
    }

    pub fn wait_for_tribute_projection_on_nodes(
        &self,
        tx_hash: &str,
        tries: u32,
        validators: usize,
    ) -> Result<()> {
        let tx_hash = tx_hash.to_owned();
        self.run(move |cfg| {
            let rpc = crate::world::rpc::Rpc::new(cfg.clone());
            let ports = (0..validators)
                .map(|index| cfg.http_port(index))
                .collect::<Vec<_>>();
            let height = rpc
                .receipt_block_number(&tx_hash, ports[0])
                .ok_or_else(|| eyre!("Tribute receipt is missing"))?;
            let checkpoint = rpc.wait_finalized_checkpoint(&ports, height, tries)?;
            let canonical = tribute::snapshot(&rpc, ports[0], checkpoint, &tx_hash)?;
            for &port in &ports[1..] {
                if tribute::snapshot(&rpc, port, checkpoint, &tx_hash)? != canonical {
                    bail!("Tribute state differs on port {port}");
                }
            }
            Ok(())
        })
    }

    pub fn projected_tribute(&self, validator: usize, tx_hash: &str) -> Result<ProjectedTribute> {
        Ok(self
            .tribute_projection_snapshot(validator, tx_hash)?
            .tribute)
    }

    pub fn tribute_projection_snapshot(
        &self,
        validator: usize,
        tx_hash: &str,
    ) -> Result<TributeProjectionSnapshot> {
        let tx_hash = tx_hash.to_owned();
        self.run(move |cfg| {
            let rpc = crate::world::rpc::Rpc::new(cfg.clone());
            let port = cfg.http_port(validator);
            let height = rpc
                .receipt_block_number(&tx_hash, port)
                .ok_or_else(|| eyre!("Tribute receipt is missing on port {port}"))?;
            let checkpoint = rpc.wait_finalized_checkpoint(&[port], height, 60)?;
            tribute::snapshot(&rpc, port, checkpoint, &tx_hash)
        })
    }

    pub fn assert_no_tribute_projection(&self) -> Result<()> {
        self.run(|cfg| {
            let rpc = crate::world::rpc::Rpc::new(cfg.clone());
            let ports = (0..cfg.validators)
                .map(|index| cfg.http_port(index))
                .collect::<Vec<_>>();
            let height = ports
                .iter()
                .map(|&port| {
                    rpc.head(port)
                        .ok_or_else(|| eyre!("missing head on port {port}"))
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .max()
                .unwrap_or(1);
            let checkpoint = rpc.wait_finalized_checkpoint(&ports, height, 60)?;
            for port in ports {
                if rpc.tribute_supply_at(port, checkpoint)? != 0 {
                    bail!("Tribute state is populated on port {port}");
                }
            }
            Ok(())
        })
    }
}

mod tribute;
