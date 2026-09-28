use outbe_chain_constants::{GenesisProtocolParametersV1, ProtocolConstantsError};
use serde_json::json;

fn resolve(
    oracle: serde_json::Value,
) -> Result<GenesisProtocolParametersV1, ProtocolConstantsError> {
    GenesisProtocolParametersV1::resolve(Some(&json!({ "oracle": oracle })))
}

#[test]
fn the_default_policy_is_eight_hours_refreshed_hourly() {
    let defaults = GenesisProtocolParametersV1::default();
    assert_eq!(
        (
            defaults.vwap_lookback_seconds,
            defaults.vwap_update_interval_seconds,
            defaults.vwap_policy_version
        ),
        (28_800, 3_600, 1)
    );
    assert_eq!(defaults.validate(), Ok(()));
}

#[test]
fn test_networks_may_shorten_the_lookback_and_the_cadence_independently() {
    let six_hours = resolve(json!({ "vwapLookbackSeconds": 21_600 })).unwrap();
    assert_eq!(
        (
            six_hours.vwap_lookback_seconds,
            six_hours.vwap_update_interval_seconds
        ),
        (21_600, 3_600)
    );
    let half_hourly = resolve(json!({ "vwapUpdateIntervalSeconds": 1_800 })).unwrap();
    assert_eq!(
        (
            half_hourly.vwap_lookback_seconds,
            half_hourly.vwap_update_interval_seconds
        ),
        (28_800, 1_800)
    );
}

#[test]
fn an_unsupported_policy_is_rejected() {
    for (oracle, field) in [
        (
            json!({ "vwapLookbackSeconds": 0 }),
            "oracle.vwapLookbackSeconds",
        ),
        (
            json!({ "vwapLookbackSeconds": 32_400 }),
            "oracle.vwapLookbackSeconds",
        ),
        (
            json!({ "vwapUpdateIntervalSeconds": 0 }),
            "oracle.vwapUpdateIntervalSeconds",
        ),
        (
            json!({ "vwapUpdateIntervalSeconds": 3_500 }),
            "oracle.vwapUpdateIntervalSeconds",
        ),
        (
            json!({ "vwapLookbackSeconds": 27_000, "vwapUpdateIntervalSeconds": 3_600 }),
            "oracle.vwapLookbackSeconds",
        ),
        (
            json!({ "vwapPolicyVersion": 0 }),
            "oracle.vwapPolicyVersion",
        ),
    ] {
        match resolve(oracle.clone()) {
            Err(ProtocolConstantsError::InvalidValue { field: actual, .. }) => {
                assert_eq!(actual, field, "{oracle}")
            }
            other => panic!("{oracle} resolved to {other:?}"),
        }
    }
}
