use std::{fs, os::unix::fs::PermissionsExt as _};

use alloy_primitives::B256;
use outbe_ocomp::export_receipt::{list_prepared_export_jobs_read_only, ExportReceiptError};

use super::filesystem::fingerprint;

#[test]
fn listing_preserves_existing_files_modes_and_native_prepared_selection() {
    let source = tempfile::tempdir().unwrap();
    let root = source.path().join("receipts");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o710)).unwrap();
    for seed in 1..=4 {
        let job = root.join(hex::encode(B256::repeat_byte(seed)));
        fs::create_dir(&job).unwrap();
        if seed != 3 {
            fs::write(job.join("prepared.ref"), b"unchanged locator").unwrap();
        }
        if seed == 2 {
            fs::write(job.join("receipt.ref"), b"committed locator").unwrap();
        }
    }
    let before = fingerprint(source.path());
    assert_eq!(
        list_prepared_export_jobs_read_only(&root, 2).unwrap(),
        vec![B256::repeat_byte(1), B256::repeat_byte(4)]
    );
    assert!(matches!(
        list_prepared_export_jobs_read_only(&root, 1),
        Err(ExportReceiptError::JobCapacityExceeded { .. })
    ));
    assert!(matches!(
        list_prepared_export_jobs_read_only(&root, 0),
        Err(ExportReceiptError::InvalidJobLimit(0))
    ));
    assert_eq!(fingerprint(source.path()), before);
}

#[test]
fn listing_never_creates_an_absent_root_or_follows_non_native_job_entries() {
    let source = tempfile::tempdir().unwrap();
    let absent = source.path().join("absent");
    assert!(list_prepared_export_jobs_read_only(&absent, 4).is_err());
    assert!(!absent.exists());
    for invalid in ["bad-name", "symlink", "file"] {
        let root = source.path().join(invalid);
        fs::create_dir(&root).unwrap();
        let job = root.join(hex::encode(B256::repeat_byte(1)));
        match invalid {
            "bad-name" => fs::create_dir(root.join("invalid")).unwrap(),
            "symlink" => std::os::unix::fs::symlink(source.path(), job).unwrap(),
            "file" => fs::write(job, b"not a directory").unwrap(),
            _ => unreachable!(),
        }
        let before = fingerprint(source.path());
        assert!(list_prepared_export_jobs_read_only(&root, 4).is_err());
        assert_eq!(fingerprint(source.path()), before);
    }
}
