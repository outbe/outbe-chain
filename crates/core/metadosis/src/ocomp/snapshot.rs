//! Private OCOMP/Fidelity snapshot boundary used while CE is still active.

use alloy_primitives::Address;
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource};
use outbe_fidelity::schema::FidelityContract;
use outbe_ocomp_protocol::league_snapshot::{fidelity_league_snapshot_root, league_snapshot_key};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_tribute::TributeContract;

use crate::schema::MetadosisContract;

impl MetadosisContract<'_> {
    /// Snapshots each day-owner's Fidelity league into per-owner storage and
    /// commits the ordered root, once per day. It MUST run during the active CE
    /// lifecycle (tribute enumeration requires `PHASE_ACTIVE`); the terminal
    /// request runs post-seal and only reads the committed root.
    ///
    /// Idempotent: a non-zero stored root means the snapshot already exists, so
    /// re-entry is a no-op and the frozen leagues stay stable across the blocks
    /// between READY enqueue and terminal-request consumption.
    pub(crate) fn build_fidelity_league_snapshot(
        &mut self,
        scope: &ExecutionScope,
        parent: &impl ParentBodySource,
        wwd: WorldwideDay,
        timestamp: u64,
    ) -> Result<()> {
        if !self
            .ocomp_fidelity_league_snapshot_root
            .read(&wwd)?
            .is_zero()
        {
            return Ok(());
        }
        let tributes =
            TributeContract::new(self.storage.clone()).get_all_day_tributes(scope, parent, wwd)?;
        let fidelity = FidelityContract::new(self.storage.clone());
        let mut entries: Vec<(Address, u16)> = Vec::with_capacity(tributes.len());
        for tribute in &tributes {
            entries.push((tribute.owner, fidelity.league_at(tribute.owner, timestamp)?));
        }
        entries.sort_by_key(|(owner, _)| *owner);
        // One canonical Tribute per owner per day -> the owner set must be
        // unique; this is also the canonical OCOMP subject order.
        if entries.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return Err(PrecompileError::Fatal(
                "OCOMP day owner set is not strictly ordered and unique".into(),
            ));
        }
        for (owner, league) in &entries {
            self.ocomp_fidelity_league_snapshot
                .write(&league_snapshot_key(wwd.value(), *owner), *league)?;
        }
        self.ocomp_fidelity_league_snapshot_root
            .write(&wwd, fidelity_league_snapshot_root(wwd.value(), &entries))?;
        Ok(())
    }
}
