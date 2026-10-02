mod completion;
mod expiry;
mod finality;
mod request;
mod voting;

use super::index::{remove_ready_key, ReadyIndexKey};
use crate::{errors::storage_corruption_message, schema::MetadosisContract};
use alloy_primitives::B256;
use outbe_ocomp_protocol::SchemaLimits;
use outbe_primitives::{error::Result, time::WorldwideDay};

impl MetadosisContract<'_> {
    /// Removes a READY scheduler entry when Metadosis fails before committing
    /// a canonical OCOMP job.
    ///
    /// Once a job exists, only its own deadline may make it terminal. In
    /// particular, this outer failure path must never construct the reserved
    /// `Failed` OCOMP outcome.
    pub(crate) fn clear_ready_ocomp_for_failed_day(
        &mut self,
        wwd: WorldwideDay,
        schema_limits: &SchemaLimits,
    ) -> Result<()> {
        if self.ocomp_fsm_states.get_bytes(&wwd).is_empty()? {
            return Ok(());
        }
        let state = self.ocomp_fsm_state(wwd, schema_limits)?;
        let projection = state.projection();
        if projection.live_intent_id.is_some() {
            return Err(storage_corruption_message(
                "Metadosis failure cannot replace a live canonical OCOMP job",
            ));
        }
        let ready_key = ReadyIndexKey::from_projection(projection)?;
        let mut ready_index = self.read_ready_index()?;
        remove_ready_key(&mut ready_index, ready_key)?;
        self.write_ready_index(&ready_index)?;
        self.ocomp_fsm_states.get_bytes(&wwd).clear()
    }

    fn release_ocomp_lineage(
        &mut self,
        lineage: B256,
        at_height: u64,
        schema_limits: &SchemaLimits,
    ) -> Result<()> {
        let mut registry = outbe_ocompregistry::OcompRegistry::new(self.storage.clone());
        if registry.active_authority(schema_limits)?.is_none() {
            return Err(storage_corruption_message(
                "terminal OCOMP WWD has no active Registry authority",
            ));
        }
        let released = registry.release_lineage(lineage, at_height, schema_limits)?;
        if !released {
            return Err(storage_corruption_message(
                "terminal OCOMP WWD has no Registry lineage pin",
            ));
        }
        Ok(())
    }
}
