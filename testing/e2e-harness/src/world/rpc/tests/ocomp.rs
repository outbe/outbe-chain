use crate::world::rpc::*;

#[cfg(feature = "ocomp-integration")]
#[test]
fn job_request_selection_keeps_the_requested_day_visible_after_later_requests() {
    let day = |worldwide_day: u32| {
        serde_json::json!({
            "topics": [
                "0x00",
                "0x00",
                format!("0x{worldwide_day:064x}")
            ]
        })
    };
    let requested = day(20260807);
    let later_retry = day(20260806);
    let logs = vec![requested.clone(), later_retry];

    assert_eq!(
        select_ocomp_job_request_log_result(&logs, Some(20260807)).unwrap(),
        Some(&requested),
    );
}

#[cfg(feature = "ocomp-integration")]
#[test]
fn job_request_selection_does_not_treat_malformed_rpc_data_as_absence() {
    let logs = vec![serde_json::json!({"topics": ["0x00", "0x00"]})];

    assert!(select_ocomp_job_request_log_result(&logs, Some(20260807)).is_err());
}
