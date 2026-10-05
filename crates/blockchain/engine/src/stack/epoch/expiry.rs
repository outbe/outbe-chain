//! Exact frozen-cycle expiry uses the height supplied by its caller.
use super::super::*;
use super::runtime::*;

pub(super) struct DkgTargetDeadline {
    cycle: u64,
    planned_activation_height: u64,
}
impl From<&FrozenDkgTarget> for DkgTargetDeadline {
    fn from(target: &FrozenDkgTarget) -> Self {
        Self {
            cycle: target.dkg_cycle,
            planned_activation_height: target.planned_activation_height,
        }
    }
}
impl<E> EpochSupervisor<E>
where
    E: BufferPooler
        + Clock
        + CryptoRng
        + Network
        + Resolver
        + Spawner
        + Storage
        + Metrics
        + Send
        + Sync
        + 'static,
{
    pub(super) fn ensure_dkg_target_timely(
        &mut self,
        target: DkgTargetDeadline,
        finalized_height: u64,
        diagnostic: &'static str,
    ) -> Result<()> {
        if frozen_dkg_target_expired(
            finalized_height,
            target.planned_activation_height,
            self.dkg_rotation_params.activation_grace_blocks,
        ) {
            let deadline = target
                .planned_activation_height
                .saturating_add(self.dkg_rotation_params.activation_grace_blocks);
            self.vrf_safety.mark_expired(finalized_height);
            publish_randomness_status(&self.bridge, &self.vrf_safety);
            return Err(eyre::eyre!(
                "{}: cycle {}, height {}, deadline {}",
                diagnostic,
                target.cycle,
                finalized_height,
                deadline
            ));
        }
        Ok(())
    }
}
