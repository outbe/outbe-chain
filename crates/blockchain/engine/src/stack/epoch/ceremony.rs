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
    pub(super) fn ensure_frozen_rotation_timely(&mut self, current_height: u64) -> Result<()> {
        if let Some(target) = self.rotation.frozen_dkg_target.as_ref() {
            let activation_deadline = target
                .planned_activation_height
                .saturating_add(self.dkg_rotation_params.activation_grace_blocks);
            if frozen_dkg_target_expired(
                current_height,
                target.planned_activation_height,
                self.dkg_rotation_params.activation_grace_blocks,
            ) {
                self.vrf_safety.mark_expired(current_height);
                publish_randomness_status(&self.bridge, &self.vrf_safety);
                return Err(eyre::eyre!(
                    "frozen DKG target missed VRF expiry: cycle {}, height {}, deadline {}",
                    target.dkg_cycle,
                    current_height,
                    activation_deadline
                ));
            }
        }
        Ok(())
    }
}
