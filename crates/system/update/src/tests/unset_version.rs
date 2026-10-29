use alloy_primitives::U256;
use alloy_sol_types::SolError;
use serde_json::Value;

use crate::api::{
    get_active_version, is_version_active_eq, resolve_active_version, version_at_height,
};
use crate::payload::encode_schedule_update_json;
use crate::schema::Update;
use crate::ProtocolVersion;

use super::{with_active_version, with_update, V1_2};

#[test]
fn get_active_version_returns_zero_on_fresh_chain() {
    with_update(|storage| {
        assert_eq!(
            get_active_version(storage.clone()).unwrap(),
            ProtocolVersion::ZERO,
            "fresh chain baseline is protocol version 0"
        );
        assert_eq!(
            resolve_active_version(storage).unwrap(),
            ProtocolVersion::ZERO,
            "the exact-state resolver preserves the valid baseline version"
        );
    });
}

#[test]
fn version_at_height_returns_zero_when_unset() {
    with_update(|storage| {
        assert_eq!(
            version_at_height(storage, 100).unwrap(),
            ProtocolVersion::ZERO
        );
    });
}

#[test]
fn is_version_active_zero_on_fresh_chain() {
    with_update(|storage| {
        assert!(is_version_active_eq(storage, ProtocolVersion::ZERO).unwrap());
    });
}

#[test]
fn schedule_update_rejects_zero_version() {
    with_update(|storage| {
        let mut update = Update::new(storage.clone());
        let payload: Value = serde_json::from_str(&encode_schedule_update_json(
            ProtocolVersion::ZERO,
            1000,
            "",
        ))
        .unwrap();
        let err = update
            .schedule_update_from_propose(U256::from(1), &payload, 100)
            .unwrap_err();
        match err {
            outbe_primitives::error::PrecompileError::RevertBytes(bytes) => {
                assert_eq!(
                    &bytes[..4],
                    &crate::precompile::IUpdate::InvalidVersion::SELECTOR
                );
            }
            other => panic!("expected typed revert bytes, got {other:?}"),
        }
    });
}

#[test]
fn set_active_version_makes_helpers_return_version() {
    with_active_version(V1_2, 500, |storage, _update| {
        assert_eq!(get_active_version(storage.clone()).unwrap(), V1_2);
        assert_eq!(resolve_active_version(storage.clone()).unwrap(), V1_2);
        assert_eq!(version_at_height(storage.clone(), 500).unwrap(), V1_2);
        assert!(is_version_active_eq(storage, V1_2).unwrap());
    });
}

#[test]
fn exact_state_resolver_rejects_noncanonical_width() {
    with_update(|storage| {
        storage
            .sstore(
                outbe_primitives::addresses::UPDATE_ADDRESS,
                U256::ZERO,
                U256::from(u32::MAX) + U256::from(1u64),
            )
            .unwrap();

        let expected = crate::precompile::IUpdate::ActiveVersionDoesNotFitU32 {}.abi_encode();
        for res in [
            resolve_active_version(storage.clone()),
            get_active_version(storage.clone()),
            Update::new(storage.clone()).get_active_version(),
        ] {
            assert!(matches!(
                res,
                Err(outbe_primitives::error::PrecompileError::RevertBytes(ref bytes))
                    if bytes.as_ref() == expected.as_slice()
            ));
        }
        assert!(matches!(
            is_version_active_eq(storage.clone(), V1_2),
            Err(outbe_primitives::error::PrecompileError::RevertBytes(ref bytes))
                if bytes.as_ref() == expected.as_slice()
        ));
        assert!(matches!(
            crate::api::is_version_active_gte(storage, V1_2),
            Err(outbe_primitives::error::PrecompileError::RevertBytes(ref bytes))
                if bytes.as_ref() == expected.as_slice()
        ));
    });
}
