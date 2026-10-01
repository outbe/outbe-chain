use super::*;
use crate::snapshot::validation::ocomp::verify_present_receipt;
use outbe_ocomp::export_receipt::VerifiedExportReceipt;

fn check(
    f: &Fixture,
    job: &OcompJobRecordV1,
    expected: Option<ExportAuthorityV1>,
) -> eyre::Result<VerifiedExportReceipt> {
    let before = fingerprint(f.directory.path());
    let result = verify_present_receipt(f.directory.path(), job, expected, CAS_LIMITS);
    assert_eq!(fingerprint(f.directory.path()), before);
    result
}

fn remove_historical_siblings(f: &Fixture) {
    fs::remove_dir_all(&f.binding_root).unwrap();
    fs::remove_dir_all(&f.catalog_root).unwrap();
    // A receipt is not proof of surviving input chunks or binding CAS.
    fs::remove_file(f.cas_path(&f.binding_ref)).unwrap();
    fs::remove_file(f.cas_path(&f.chunk_ref)).unwrap();
}

#[test]
fn receipt_survives_pruned_binding_catalog_and_their_cas_objects() {
    let f = fixture(20, None);
    remove_historical_siblings(&f);
    for expected in [None, Some(f.expected())] {
        let receipt = check(&f, &f.job, expected).unwrap();
        assert_eq!(receipt.receipt_ref(), f.receipt_ref);
        assert_eq!(receipt.manifest_ref(), f.manifest_ref);
        assert_eq!(receipt.manifest_hash(), f.manifest_hash);
        assert_eq!(receipt.committed(), f.committed);
        assert!(!f.binding_root.exists());
        assert!(!f.catalog_root.exists());
    }
    // The weaker receipt observation cannot satisfy a complete-export obligation.
    assert!(f
        .check(Some(f.expected()))
        .unwrap_err()
        .downcast_ref::<Incomplete>()
        .is_some());
}

#[test]
fn canonical_manifest_fields_and_request_checkpoint_cannot_be_substituted() {
    for field in [
        "bundle",
        "attempt",
        "day",
        "collection_key",
        "collection_root",
        "count",
        "nominal",
        "ce_root",
        "height",
        "block_hash",
        "state_root",
    ] {
        let f = fixture(20, None);
        remove_historical_siblings(&f);
        let mut job = f.job.clone();
        match field {
            "bundle" => job.intent.protocol_bundle_hash = hash(0xee),
            "attempt" => job.intent.attempt += 1,
            "day" => job.intent.wwd += 1,
            "collection_key" => job.intent.sealed_tribute_collection_key = hash(0xee),
            "collection_root" => job.intent.sealed_tribute_collection_root = hash(0xee),
            "count" => job.intent.authenticated_day_count += 1,
            "nominal" => job.intent.authenticated_day_nominal += U256::from(1),
            "ce_root" => job.intent.ce_sealed_root = hash(0xee),
            "height" => job.intent_height += 1,
            "block_hash" => {
                job.finalized.as_mut().unwrap().finalized_request_block_hash = hash(0xee)
            }
            "state_root" => {
                job.finalized.as_mut().unwrap().finalized_request_state_root = hash(0xee)
            }
            _ => unreachable!(),
        }
        let error = check(&f, &job, None).unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_none(),
            "{field}: {error:#}"
        );
    }
}

#[test]
fn native_consistent_receipt_must_match_canonical_checkpoint_height_and_ce_schema() {
    for damage in ["checkpoint_height", "checkpoint_schema"] {
        let f = fixture(20, Some(damage));
        remove_historical_siblings(&f);
        let error = check(&f, &f.job, None).unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_none(),
            "{damage}: {error:#}"
        );
    }
}

#[test]
fn optional_saved_export_authority_binds_source_lease_and_manifest() {
    let f = fixture(20, None);
    remove_historical_siblings(&f);
    let expected = f.expected();
    for changed in [
        ExportAuthorityV1 {
            source_generation: 12,
            ..expected
        },
        ExportAuthorityV1 {
            lease_generation: 18,
            ..expected
        },
        ExportAuthorityV1 {
            manifest_hash: hash(0xee),
            ..expected
        },
    ] {
        let error = check(&f, &f.job, Some(changed)).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    }
    check(&f, &f.job, Some(expected)).unwrap();
}

#[test]
fn complete_receipt_still_requires_its_own_prepared_manifest_and_cas_evidence() {
    for missing in [
        "receipt_locator",
        "prepared_locator",
        "receipt_cas",
        "manifest_cas",
    ] {
        let f = fixture(20, None);
        remove_historical_siblings(&f);
        let path = match missing {
            "receipt_locator" => f.receipt_root.join("receipt.ref"),
            "prepared_locator" => f.receipt_root.join("prepared.ref"),
            "receipt_cas" => f.cas_path(&f.receipt_ref),
            "manifest_cas" => f.cas_path(&f.manifest_ref),
            _ => unreachable!(),
        };
        fs::remove_file(&path).unwrap();
        let error = check(&f, &f.job, None).unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_some(),
            "{missing}: {error:#}"
        );
        assert!(!path.exists());
    }
}

#[test]
fn malformed_receipt_cas_fails_without_requiring_pruned_siblings() {
    let f = fixture(20, None);
    remove_historical_siblings(&f);
    let path = f.cas_path(&f.receipt_ref);
    let mut bytes = fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(path, bytes).unwrap();
    let error = check(&f, &f.job, None).unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
}
