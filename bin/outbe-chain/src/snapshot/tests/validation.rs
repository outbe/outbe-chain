use outbe_snapshot::manifest::BlockIdentity;
use serde_json::json;

use crate::snapshot::validation::report::{
    CheckName, CheckStatus, InventoryBounds, RequiredHeight, RetainedRange, ValidationReport,
    MAX_DIAGNOSTIC_CHARS,
};

#[test]
fn selected_checks_start_incomplete_and_each_must_pass() {
    assert!(!ValidationReport::new([]).success());
    let mut report = ValidationReport::new([CheckName::Headers, CheckName::Evm]);
    for check in CheckName::ALL {
        assert_eq!(
            report.check(check).status,
            if matches!(check, CheckName::Headers | CheckName::Evm) {
                CheckStatus::Incomplete
            } else {
                CheckStatus::NotRequested
            }
        );
    }
    assert!(!report.success());
    report.record(CheckName::Headers, CheckStatus::Passed, None);
    assert!(!report.success());
    report.record(CheckName::Evm, CheckStatus::Passed, None);
    assert!(report.success());
    for status in [
        CheckStatus::Failed,
        CheckStatus::Incomplete,
        CheckStatus::NotRequested,
    ] {
        report.record(
            CheckName::Evm,
            status,
            Some("required current state unavailable"),
        );
        assert!(
            !report.success(),
            "selected {status:?} cannot count as passed"
        );
    }
}

#[test]
fn unselected_corruption_remains_not_requested_and_does_not_change_success() {
    // Dependency expansion happens before report construction in the orchestrator.
    let mut report = ValidationReport::new([CheckName::Headers, CheckName::Evm]);
    report.record(CheckName::Headers, CheckStatus::Passed, None);
    report.record(CheckName::Evm, CheckStatus::Passed, None);
    for check in [
        CheckName::Files,
        CheckName::Provenance,
        CheckName::Ce,
        CheckName::Bodies,
        CheckName::Ocomp,
    ] {
        report.record(
            check,
            CheckStatus::Failed,
            Some("unselected source is corrupt"),
        );
        assert_eq!(report.check(check).status, CheckStatus::NotRequested);
        assert!(report.check(check).diagnostic.is_none());
    }
    assert!(report.success());
    let encoded = serde_json::to_value(&report).unwrap();
    assert_eq!(encoded["checks"].as_object().unwrap().len(), 7);
    assert_eq!(encoded["checks"]["ocomp"]["status"], "not_requested");
}

#[test]
fn artifact_success_cannot_override_missing_or_failed_native_checks() {
    let mut report = ValidationReport::new(CheckName::ALL);
    report.record(CheckName::Files, CheckStatus::Passed, None);
    report.record(CheckName::Provenance, CheckStatus::Passed, None);
    report.provenance.signature_valid = Some(true);
    report.provenance.expected_signer_match = Some(true);
    assert!(!report.success());
    for check in [
        CheckName::Headers,
        CheckName::Evm,
        CheckName::Ce,
        CheckName::Bodies,
    ] {
        report.record(check, CheckStatus::Passed, None);
    }
    report.record(
        CheckName::Ocomp,
        CheckStatus::Failed,
        Some("required job has conflicting manifest"),
    );
    assert!(!report.success());
    report.record(
        CheckName::Ocomp,
        CheckStatus::Incomplete,
        Some("required second NOD job is missing"),
    );
    assert!(!report.success());
    report.record(CheckName::Ocomp, CheckStatus::Passed, None);
    assert!(report.success());
}

#[test]
fn report_serializes_distinct_frontiers_ranges_bounds_and_provenance() {
    let mut report = ValidationReport::new([CheckName::Bodies, CheckName::Provenance]);
    let block = |number: u64, byte: u8| BlockIdentity {
        number,
        hash: format!("{byte:02x}").repeat(32),
    };
    report.observed.h = Some(block(10, 1));
    report.observed.e = Some(block(14, 2));
    report.observed.q = Some(block(8, 3));
    report.observed.p = Some(block(9, 4));
    report.observed.c_baseline = Some(block(0, 5));
    report.observed.c_previous = Some(block(2, 6));
    report.observed.c_current = Some(block(7, 7));
    report.retained_ranges.push(RetainedRange {
        domain: "headers".into(),
        start: 7,
        end_inclusive: 14,
    });
    report.required_missing.push(RequiredHeight {
        domain: "receipt_frames".into(),
        height: 4,
    });
    report.inventory_bounds.push(InventoryBounds {
        name: "nod_fifo".into(),
        start: 3,
        end_exclusive: 8,
        visited: 2,
    });
    report.provenance.signature_valid = Some(true);
    report.provenance.signer = Some("02".to_owned() + &"ab".repeat(32));
    report.provenance.expected_signer_match = Some(false);
    report.record(
        CheckName::Bodies,
        CheckStatus::Incomplete,
        Some("Q differs from P"),
    );
    report.record(
        CheckName::Provenance,
        CheckStatus::Failed,
        Some("valid signature, unexpected signer"),
    );
    let encoded = serde_json::to_value(&report).unwrap();
    for (name, number) in [
        ("h", 10),
        ("e", 14),
        ("q", 8),
        ("p", 9),
        ("c_baseline", 0),
        ("c_previous", 2),
        ("c_current", 7),
    ] {
        assert_eq!(encoded["observed"][name]["number"], number);
    }
    assert_eq!(
        encoded["retained_ranges"],
        json!([{ "domain": "headers", "start": 7, "end_inclusive": 14 }])
    );
    assert_eq!(
        encoded["required_missing"],
        json!([{ "domain": "receipt_frames", "height": 4 }])
    );
    assert_eq!(
        encoded["inventory_bounds"],
        json!([{ "name": "nod_fifo", "start": 3, "end_exclusive": 8, "visited": 2 }])
    );
    assert_eq!(encoded["provenance"]["signature_valid"], true);
    assert_eq!(encoded["provenance"]["expected_signer_match"], false);
    assert_eq!(
        encoded["provenance"]["signer"],
        "02".to_owned() + &"ab".repeat(32)
    );
    assert_eq!(encoded["checks"]["bodies"]["status"], "incomplete");
    assert_eq!(encoded["checks"]["provenance"]["status"], "failed");
    assert!(!report.success());

    let unknown = serde_json::to_value(ValidationReport::new([CheckName::Evm])).unwrap();
    assert!(unknown["observed"]["h"].is_null());
    assert!(unknown["provenance"]["signature_valid"].is_null());
    assert!(unknown["provenance"]["expected_signer_match"].is_null());
    assert!(unknown["provenance"]["signer"].is_null());
}

#[test]
fn diagnostic_is_bounded_on_unicode_boundaries_and_replaced_not_accumulated() {
    let mut report = ValidationReport::new([CheckName::Ocomp]);
    let message = "a💾界é".repeat(MAX_DIAGNOSTIC_CHARS);
    report.record(CheckName::Ocomp, CheckStatus::Failed, Some(&message));
    let diagnostic = report.check(CheckName::Ocomp).diagnostic.as_ref().unwrap();
    assert_eq!(diagnostic.chars().count(), MAX_DIAGNOSTIC_CHARS);
    assert!(message.starts_with(diagnostic));
    serde_json::to_string(&report).unwrap();
    report.record(
        CheckName::Ocomp,
        CheckStatus::Incomplete,
        Some("new observation"),
    );
    assert_eq!(
        report.check(CheckName::Ocomp).diagnostic.as_deref(),
        Some("new observation")
    );
    report.record(CheckName::Ocomp, CheckStatus::Passed, None);
    assert!(report.check(CheckName::Ocomp).diagnostic.is_none());
    assert!(report.success());
}
