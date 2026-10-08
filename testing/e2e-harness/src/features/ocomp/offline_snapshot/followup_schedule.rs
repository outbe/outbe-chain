use super::*;

pub(super) fn create_snapshot_day(
    world: &mut crate::world::World,
    day: u32,
) -> eyre::Result<Option<crate::world::rpc::MetadosisWorldwideDayStateV1>> {
    use crate::features::ocomp::{
        first_protocol_cycle_at_or_after, restart_committee_at_logical_time,
    };
    use outbe_primitives::time::WorldwideDay;
    use std::time::{Duration, Instant};
    let primary = world.validators.primary_port();
    let mut schedule = world.rpc.metadosis_wwd_state_on(primary, day);
    if schedule.is_none() {
        // The native ProtocolCycle creates a day only after its forming start.
        // Read phase boundaries from that created state, never fabricate them.
        let creation =
            first_protocol_cycle_at_or_after(world, WorldwideDay::new(day).start_timestamp());
        let height = world
            .rpc
            .head(primary)
            .ok_or_else(|| eyre!("head before next WWD"))?;
        let timestamp = world
            .rpc
            .block_timestamp(primary, height)
            .ok_or_else(|| eyre!("timestamp before next WWD"))?;
        let mut publication = if timestamp < creation {
            restart_committee_at_logical_time(world, creation).3
        } else {
            None
        };
        let deadline = Instant::now() + Duration::from_secs(180);
        while schedule.is_none() || publication.is_some() {
            ensure!(
                Instant::now() < deadline,
                "next WWD creation and Oracle publication did not complete"
            );
            if let Some(pending) = publication.as_ref() {
                if crate::features::price_oracle::observe_pending_publication(world, pending) {
                    publication = None;
                }
            }
            schedule = world.rpc.metadosis_wwd_state_on(primary, day);
            ensure!(
                schedule.as_ref().is_none_or(|state| state.status <= 2),
                "next WWD passed offering while awaiting creation and Oracle publication"
            );
            if schedule.is_none() || publication.is_some() {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    Ok(schedule)
}

pub(super) fn await_snapshot_offering(
    world: &mut crate::world::World,
    day: u32,
) -> eyre::Result<crate::world::rpc::MetadosisWorldwideDayStateV1> {
    use crate::features::ocomp::restart_committee_at_logical_time;
    use std::time::{Duration, Instant};
    let primary = world.validators.primary_port();
    let schedule = create_snapshot_day(world, day)?;
    let schedule = schedule.ok_or_else(|| eyre!("next WWD schedule unavailable"))?;
    ensure!(schedule.status <= 2, "next-day offering already passed");
    let mut publication = if schedule.status < 2 {
        restart_committee_at_logical_time(world, schedule.lookback_end + 1).3
    } else {
        None
    };
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let state = world
            .rpc
            .metadosis_wwd_state_on(primary, day)
            .ok_or_else(|| eyre!("next WWD schedule unavailable"))?;
        ensure!(
            state.status <= 2 && Instant::now() < deadline,
            "next WWD offering and Oracle publication did not remain available"
        );
        if let Some(pending) = publication.as_ref() {
            if crate::features::price_oracle::observe_pending_publication(world, pending) {
                publication = None;
                // Re-read current offering after the publication observation.
                continue;
            }
        }
        if state.status == 2 && publication.is_none() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(schedule)
}

pub(super) fn request_snapshot_job(
    world: &mut crate::world::World,
    day: u32,
    cut: u64,
    scheduled_process_time: u64,
) -> eyre::Result<(u64, crate::world::rpc::OcompPublicJobRequestV1)> {
    use crate::features::ocomp::{
        first_protocol_cycle_at_or_after, restart_committee_at_logical_time,
    };
    use std::time::{Duration, Instant};
    let primary = world.validators.primary_port();
    ensure!(
        world
            .rpc
            .finalized_ocomp_job_request_for_worldwide_day_result_on(primary, cut, day)?
            .is_absent(),
        "follow-up job already existed before requested work"
    );
    let operator = world.validators.get(0).evm_key()?;
    let requested = snapshot_now_millis()?;
    let tx = world
        .rpc
        .tribute_offer(&operator, &day.to_string())
        .ok_or_else(|| eyre!("submit real next-day Tribute"))?;
    ensure!(
        world.rpc.wait_successful_receipt(&tx, 240),
        "new Tribute failed"
    );
    world.projection.wait_for_tribute_projection(&tx, 240)?;
    let processing = first_protocol_cycle_at_or_after(world, scheduled_process_time);
    let mut publication = restart_committee_at_logical_time(world, processing).3;
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut finalized_request = None;
    let request = loop {
        ensure!(
            Instant::now() < deadline,
            "new JobIntent and Oracle publication did not finalize"
        );
        if finalized_request.is_none() {
            finalized_request = world
                .rpc
                .finalized_ocomp_job_request_for_worldwide_day_result_on(primary, cut + 1, day)?
                .into_bound_request()?;
        }
        if let Some(pending) = publication.as_ref() {
            if crate::features::price_oracle::observe_pending_publication(world, pending) {
                publication = None;
            }
        }
        if publication.is_none() {
            if let Some(request) = finalized_request.take() {
                break request;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    Ok((requested, request))
}
