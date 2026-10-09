use alloy_primitives::U256;

use outbe_primitives::error::PrecompileError;

use crate::api::{is_version_active_eq, is_version_active_gte};
use crate::schema::ScheduledUpdateStatus;
use crate::schema::Update;

use super::{
    assert_active_version, min_activation, schedule_update, schedule_version, with_active_version,
    with_scheduled_release, with_scheduled_update, with_update, SCHEDULE_HEIGHT, V1_2, V1_3, V1_5,
    V2_0,
};

#[test]
fn schedule_update_writes_fields_and_waiting_index() {
    with_scheduled_release(V1_2, "release-notes", |_storage, update, activation| {
        let proposal_id = U256::from(1);
        let scheduled = update.read_scheduled_update(proposal_id).unwrap().unwrap();
        assert_eq!(scheduled.proposal_id, proposal_id);
        assert_eq!(scheduled.version, V1_2);
        assert_eq!(scheduled.activation_height, activation);
        assert_eq!(scheduled.status, ScheduledUpdateStatus::Scheduled);
        assert_eq!(scheduled.info, "release-notes");
        assert_eq!(
            update.list_waiting_for_activation_proposal_ids().unwrap(),
            vec![proposal_id]
        );
    });
}

#[test]
fn active_version_helpers_roundtrip() {
    with_active_version(V1_5, 500, |storage, _update| {
        assert_active_version(&storage, V1_5, 500);
        assert!(is_version_active_gte(storage.clone(), V1_2).unwrap());
        assert!(!is_version_active_eq(storage.clone(), V1_3).unwrap());
    });
}

#[test]
fn rejects_downgrade_schedule() {
    with_active_version(V2_0, 1, |_storage, update| {
        let err = schedule_update(
            update,
            U256::from(1),
            crate::ScheduleUpdatePayload::new(V1_3, min_activation(10), ""),
            10,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::Revert(msg) if msg.contains("downgrade")
        ));
    });
}

#[test]
fn rejects_duplicate_proposal_id() {
    with_scheduled_update(V1_2, |_storage, update, activation| {
        let proposal_id = U256::from(1);
        let err = schedule_version(update, proposal_id, V1_2, activation + 1).unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::Revert(msg) if msg.contains("already exists")
        ));
    });
}

#[test]
fn rejects_conflicting_activation_height() {
    with_scheduled_update(V1_2, |_storage, update, activation| {
        let err = schedule_version(update, U256::from(2), V1_2, activation).unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::Revert(msg) if msg.contains("activation height")
        ));
    });
}

#[test]
fn max_waiting_for_activation_updates_is_enforced() {
    with_update(|storage| {
        let mut update = Update::new(storage.clone());
        let base_activation = min_activation(SCHEDULE_HEIGHT);
        for i in 0..crate::constants::MAX_WAITING_FOR_ACTIVATION_UPDATES {
            schedule_version(
                &mut update,
                U256::from(i + 1),
                V1_2,
                base_activation + i as u64,
            )
            .unwrap();
        }

        let err =
            schedule_version(&mut update, U256::from(65), V1_2, base_activation + 64).unwrap_err();
        assert!(matches!(
            err,
            PrecompileError::Revert(msg) if msg.contains("too many scheduled updates waiting")
        ));
    });
}
