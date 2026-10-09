use alloy_primitives::U256;
use alloy_sol_types::SolEvent;

use outbe_primitives::addresses::UPDATE_ADDRESS;
use outbe_primitives::error::PrecompileError;

use crate::precompile::{dispatch, IUpdate};
use crate::schema::Update;

use super::{
    schedule_early_and_late, scheduled_update_provider, with_update, with_update_provider,
    UpdateTestExt, PV, V1_2,
};

#[test]
fn schedule_emits_scheduled_update_created_event() {
    let provider = scheduled_update_provider(V1_2, "notes", |_storage, _update, _activation| {});

    assert!(has_event(
        &provider,
        IUpdate::ScheduledUpdateCreated::SIGNATURE_HASH
    ));
}

#[test]
fn lifecycle_emits_upgrade_activated_event() {
    let provider = scheduled_update_provider(PV, "", |_storage, update, activation| {
        update.process_begin_block_test(activation).unwrap();
    });

    assert!(has_event(
        &provider,
        IUpdate::UpgradeActivated::SIGNATURE_HASH
    ));
}

#[test]
fn lifecycle_emits_upgrade_canceled_event() {
    let provider = with_update_provider(|storage| {
        let mut update = Update::new(storage.clone());
        let (activation_early, _activation_late) = schedule_early_and_late(&mut update);
        update.process_begin_block_test(activation_early).unwrap();
    });

    assert!(has_event(
        &provider,
        IUpdate::UpgradeCanceled::SIGNATURE_HASH
    ));
}

#[test]
fn dispatch_rejects_unknown_selector() {
    with_update(|storage| {
        // UPDATE_ADDRESS no longer dispatches the legacy createProposal selector.
        let data = alloy_primitives::hex!("b1a14106");
        let err =
            dispatch(storage, &data, alloy_primitives::Address::ZERO, U256::ZERO).unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)));
    });
}

fn has_event(
    provider: &outbe_primitives::storage::hashmap::HashMapStorageProvider,
    topic0: alloy_primitives::B256,
) -> bool {
    provider
        .get_events(UPDATE_ADDRESS)
        .iter()
        .any(|log| log.topics().first() == Some(&topic0))
}
