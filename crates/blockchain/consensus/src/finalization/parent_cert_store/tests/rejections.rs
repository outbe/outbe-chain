use super::*;

fn write_invalid_record(dir: &Path, key: B256, record: &CertifiedParentProofRecord) {
    let backend = MdbxParentProofBackend::open(dir).unwrap();
    let bytes = backend.encode_record(record).unwrap();
    let tx = backend.db.tx_mut().unwrap();
    tx.put::<tables::OutbeCertifiedParentFinalizationRecords>(key, bytes)
        .unwrap();
    tx.commit().unwrap();
}

#[test]
fn proof_store_record_format_version_is_two_and_rejects_unknown() {
    // format_version != 2 must be rejected on read.
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("records");
    let mut bad = finalization_record(0xCA, 100);
    bad.format_version = 42;
    // Bypass the put-side guard to simulate a corrupt on-disk row.
    write_invalid_record(&dir, B256::with_last_byte(0xCA), &bad);
    let err = match FinalizedParentCertStore::open(&dir) {
        Ok(_) => panic!("unknown format_version must be rejected on read"),
        Err(e) => e,
    };
    assert!(
        matches!(
            err,
            ParentProofStoreError::UnknownFormatVersion { version: 42, .. }
        ),
        "expected UnknownFormatVersion(42), got {err}"
    );
}

#[test]
fn durable_store_rejects_corrupt_key_payload_mismatch_on_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("records");
    let payload = finalization_record(0xAA, 77);
    write_invalid_record(&dir, B256::with_last_byte(0xBB), &payload);
    let err = match FinalizedParentCertStore::open(&dir) {
        Ok(_) => panic!("mismatched key/payload must fail closed"),
        Err(e) => e,
    };
    assert!(matches!(err, ParentProofStoreError::Corrupt { .. }));
}

#[test]
fn durable_store_rejects_legacy_v1_tables_on_open() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("records");
    let legacy_db = reth_db::mdbx::init_db_for::<_, tables::OutbeCertifiedParentProofLegacyTables>(
        &dir,
        DatabaseArguments::new(ClientVersion::default()),
    )
    .unwrap();
    drop(legacy_db);

    let err = match FinalizedParentCertStore::open(&dir) {
        Ok(_) => panic!("legacy V1 tables must fail startup"),
        Err(e) => e,
    };
    assert!(
        matches!(
            err,
            ParentProofStoreError::LegacyTableFound {
                table: "OutbeCertifiedParentFinalizationRecords",
                ..
            }
        ),
        "expected legacy V1 table error, got {err}"
    );
}
