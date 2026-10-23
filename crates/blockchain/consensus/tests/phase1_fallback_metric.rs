//! The Phase 1 certified-notarization fallback counter must not grow one series
//! per epoch or view. Round detail stays off the counter.

#[test]
fn phase1_cn_fallback_counter_has_no_round_labels() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::with_local_recorder(&recorder, || {
        outbe_consensus::metrics::record_phase1_used_cn_fallback(7, 99);
        outbe_consensus::metrics::record_phase1_used_cn_fallback(8, 100);
    });

    let snapshot = snapshotter.snapshot().into_vec();
    let matches: Vec<_> = snapshot
        .iter()
        .filter(|(key, _, _, _)| key.key().name() == "outbe_phase1_used_cn_fallback_total")
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "epoch or view labels split the counter into {matches:?}"
    );
    let labels: Vec<_> = matches[0]
        .0
        .key()
        .labels()
        .map(|label| label.key().to_string())
        .collect();
    assert!(
        labels.is_empty(),
        "counter labels must not include epoch or view: {labels:?}"
    );
    match &matches[0].3 {
        DebugValue::Counter(value) => {
            assert_eq!(*value, 2, "two fallback events must share one counter");
        }
        other => panic!("expected counter value, got {other:?}"),
    }
}
