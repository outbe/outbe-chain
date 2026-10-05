//! Freeze a deterministic target and supervise durable DKG ceremonies.
mod freeze;
mod retry;
mod task;
use super::super::*;
use super::runtime::*;
use task::CeremonyAttempt;

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
    pub(super) async fn schedule_rotation(
        &mut self,
        ctx: &E,
        current_height: u64,
    ) -> Result<EventAction> {
        self.ensure_frozen_rotation_timely(current_height)?;
        if self.rotation.retry_frozen_dkg {
            self.rotation.retry_frozen_dkg = false;
            if let Some(target) = self.rotation.frozen_dkg_target.as_ref().cloned() {
                self.retry_frozen_rotation(ctx, target).await?;
                return Ok(EventAction::Continue);
            }
        }
        let freeze_height = self
            .dkg_rotation_params
            .freeze_height(self.state.last_dkg_activation_height);
        if self.rotation.dealer_only_dkg_activation.is_none()
            && should_start_dkg_rotation(
                self.rotation.frozen_dkg_target.is_some(),
                self.rotation.pending_dkg_activation.is_some(),
                current_height,
                freeze_height,
            )
        {
            self.freeze_new_rotation(ctx, current_height, freeze_height)
                .await
        } else {
            debug!(
                current_height,
                freeze_height, "DKG rotation freeze height not reached"
            );
            Ok(EventAction::Proceed)
        }
    }
    fn ensure_frozen_rotation_timely(&mut self, current_height: u64) -> Result<()> {
        if let Some(target) = self
            .rotation
            .frozen_dkg_target
            .as_ref()
            .map(super::expiry::DkgTargetDeadline::from)
        {
            self.ensure_dkg_target_timely(
                target,
                current_height,
                "frozen DKG target missed VRF expiry",
            )?;
        }
        Ok(())
    }
}
