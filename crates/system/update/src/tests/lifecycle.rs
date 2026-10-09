use alloy_primitives::U256;
use outbe_primitives::error::PrecompileError;

use crate::api::{get_active_version, version_at_height};
use crate::constants::{PROTOCOL_VERSION, PROTOCOL_VERSION_MAJOR};
use crate::encode_protocol_version;
use crate::schema::ScheduledUpdateStatus;
use crate::schema::Update;

use super::{
    min_activation, schedule_early_and_late, schedule_update, schedule_version,
    with_scheduled_update, with_update, UpdateTestExt, PV, SCHEDULE_HEIGHT, V1_2, V1_3,
};

#[test]
fn activation_boundary_is_begin_block_inclusive() {
    with_scheduled_update(PV, |storage, update, activation| {
        update.process_begin_block_test(activation - 1).unwrap();
        assert_eq!(
            get_active_version(storage.clone()).unwrap(),
            crate::ProtocolVersion::ZERO
        );
        assert_eq!(
            version_at_height(storage.clone(), activation - 1).unwrap(),
            crate::ProtocolVersion::ZERO
        );

        update.process_begin_block_test(activation).unwrap();
        assert_eq!(get_active_version(storage.clone()).unwrap(), PV);
        assert_eq!(version_at_height(storage.clone(), activation).unwrap(), PV);

        update.process_begin_block_test(activation + 1).unwrap();
        assert_eq!(get_active_version(storage.clone()).unwrap(), PV);
        assert_eq!(
            version_at_height(storage, activation + 1).unwrap(),
            crate::ProtocolVersion::ZERO,
            "legacy history records only the exact activation height"
        );
    });
}

#[test]
fn lifecycle_activates_scheduled_update() {
    with_scheduled_update(PV, |storage, update, activation| {
        let proposal_id = U256::from(1);

        update.process_begin_block_test(activation).unwrap();

        let scheduled = update.read_scheduled_update(proposal_id).unwrap().unwrap();
        assert_eq!(scheduled.status, ScheduledUpdateStatus::Activated);
        assert!(update
            .list_waiting_for_activation_proposal_ids()
            .unwrap()
            .is_empty());
        assert_eq!(get_active_version(storage.clone()).unwrap(), PV);
        assert_eq!(version_at_height(storage, activation).unwrap(), PV);
    });
}

#[test]
fn waiting_index_tracks_pending_scheduled_updates() {
    with_scheduled_update(V1_2, |_storage, update, _activation| {
        let proposal_id = U256::from(1);
        assert_eq!(
            update.list_waiting_for_activation_proposal_ids().unwrap(),
            vec![proposal_id]
        );
    });
}

#[test]
fn second_schedule_at_same_height_is_rejected() {
    with_scheduled_update(V1_2, |_storage, update, activation| {
        assert!(schedule_version(update, U256::from(2), V1_3, activation).is_err());
        assert_eq!(
            update.list_waiting_for_activation_proposal_ids().unwrap(),
            vec![U256::from(1)]
        );
    });
}

#[test]
fn activate_scheduled_update_cancels_stale_lower_version() {
    with_update(|storage| {
        let mut update = Update::new(storage.clone());
        // Both schedules use PV (the only activatable version). The later one
        // becomes stale once the earlier activation sets active == PV.
        let (activation_early, activation_late) = schedule_early_and_late(&mut update);

        update.process_begin_block_test(activation_early).unwrap();
        assert_eq!(get_active_version(storage.clone()).unwrap(), PV);

        let stale = update
            .read_scheduled_update(U256::from(2))
            .unwrap()
            .unwrap();
        assert_eq!(
            stale.status,
            ScheduledUpdateStatus::Canceled,
            "stale equal-version update should be canceled when same version activates"
        );
        assert!(update
            .list_waiting_for_activation_proposal_ids()
            .unwrap()
            .is_empty());

        update.process_begin_block_test(activation_late).unwrap();
        assert_eq!(
            get_active_version(storage.clone()).unwrap(),
            PV,
            "activating an older scheduled update must not downgrade active version"
        );
    });
}

#[test]
fn multiple_due_updates_cannot_reduce_active_version() {
    with_update(|storage| {
        let mut update = Update::new(storage.clone());
        let current = 200u64;
        let activation = min_activation(current) + 1000;
        schedule_update(
            &mut update,
            U256::from(1),
            crate::ScheduleUpdatePayload::new(PV, activation, ""),
            current,
        )
        .unwrap();
        schedule_update(
            &mut update,
            U256::from(2),
            crate::ScheduleUpdatePayload::new(PV, activation, ""),
            current + 1,
        )
        .expect_err("conflicting activation height must be rejected at schedule time");
        schedule_update(
            &mut update,
            U256::from(2),
            crate::ScheduleUpdatePayload::new(PV, activation + 1, ""),
            current + 1,
        )
        .unwrap();

        update.process_begin_block_test(activation).unwrap();
        assert_eq!(get_active_version(storage.clone()).unwrap(), PV);

        update.process_begin_block_test(activation + 1).unwrap();
        assert_eq!(
            get_active_version(storage.clone()).unwrap(),
            PV,
            "later activation of equal version must not change active version"
        );
    });
}

#[test]
fn activate_version_above_protocol_version_is_fatal() {
    with_update(|storage| {
        let mut update = Update::new(storage.clone());
        let activation = min_activation(SCHEDULE_HEIGHT);
        let unsupported = encode_protocol_version(PROTOCOL_VERSION_MAJOR.saturating_add(1), 0);
        assert!(
            unsupported > PROTOCOL_VERSION,
            "test version must exceed binary PROTOCOL_VERSION"
        );
        schedule_version(&mut update, U256::from(1), unsupported, activation).unwrap();

        let err = update.process_begin_block_test(activation).unwrap_err();
        assert!(
            matches!(
                err,
                PrecompileError::Fatal(ref message)
                    if message.contains("cannot activate protocol version")
                        && message.contains("binary supports at most")
            ),
            "expected Fatal for unsupported activation, got {err:?}"
        );

        let scheduled = update
            .read_scheduled_update(U256::from(1))
            .unwrap()
            .unwrap();
        assert_eq!(scheduled.status, ScheduledUpdateStatus::Scheduled);
        assert_ne!(get_active_version(storage).unwrap(), unsupported);
    });
}
