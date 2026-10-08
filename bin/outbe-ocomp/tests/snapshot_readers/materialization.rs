use alloy_primitives::B256;
use outbe_ocomp::nod_materialization::{
    MaterializationReferenceReaderV1, MaterializationReferenceStoreV1,
};
use outbe_ocomp_protocol::CasObjectRefV1;
use std::{fs, path::Path};

fn references(seed: u8) -> Vec<CasObjectRefV1> {
    vec![CasObjectRefV1 {
        transport_digest: B256::repeat_byte(seed),
        encoded_bytes: 200,
        expected_ocb1_kind: Some(1),
    }]
}

fn native_record(root: &Path, job: B256, ordinal: u32) -> std::path::PathBuf {
    let directory = root.join(hex::encode(job)).join(ordinal.to_string());
    MaterializationReferenceStoreV1::open(&directory)
        .unwrap()
        .pin_exact(job, &references(3))
        .unwrap();
    directory.join(format!("{}.materialization-refs-v1.json", hex::encode(job)))
}

use super::filesystem::fingerprint;

#[test]
fn nested_native_references_preserve_job_and_ordinal_without_mutation() {
    let source = tempfile::tempdir().unwrap();
    let root = source.path().join("materialization-references");
    for seed in [1, 2] {
        for ordinal in [0, 17] {
            native_record(&root, B256::repeat_byte(seed), ordinal);
        }
    }
    let before = fingerprint(source.path());
    let reader = MaterializationReferenceReaderV1::open_existing(&root).unwrap();
    let mut seen = vec![];
    reader
        .visit_references(&mut |job, ordinal, refs| {
            assert_eq!(refs, references(3));
            seen.push((job, ordinal));
            Ok(())
        })
        .unwrap();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (B256::repeat_byte(1), 0),
            (B256::repeat_byte(1), 17),
            (B256::repeat_byte(2), 0),
            (B256::repeat_byte(2), 17)
        ]
    );
    assert_eq!(
        reader.load_exact(B256::repeat_byte(1), 17).unwrap(),
        Some(references(3))
    );
    assert!(reader
        .load_exact(B256::repeat_byte(1), 99)
        .unwrap()
        .is_none());
    assert!(reader
        .load_exact(B256::repeat_byte(9), 0)
        .unwrap()
        .is_none());
    assert_eq!(fingerprint(source.path()), before);
}

#[test]
fn references_reject_cross_job_and_invalid_native_locators_without_repair() {
    for case in [
        "inner-job",
        "file-job",
        "ordinal",
        "version",
        "duplicate",
        "temp",
        "symlink",
        "oversized",
    ] {
        let source = tempfile::tempdir().unwrap();
        let root = source.path().join("references");
        let job = B256::repeat_byte(1);
        let path = native_record(&root, job, 17);
        match case {
            "inner-job" | "version" | "duplicate" => {
                let mut record: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                if case == "inner-job" {
                    record["jobId"] = serde_json::to_value(B256::repeat_byte(2)).unwrap();
                } else if case == "version" {
                    record["version"] = 2.into();
                } else {
                    let duplicate = record["dependencies"][0].clone();
                    record["dependencies"]
                        .as_array_mut()
                        .unwrap()
                        .push(duplicate);
                }
                fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
            }
            "file-job" => {
                fs::rename(
                    &path,
                    path.with_file_name(format!(
                        "{}.materialization-refs-v1.json",
                        hex::encode(B256::repeat_byte(2))
                    )),
                )
                .unwrap();
            }
            "ordinal" => {
                fs::rename(
                    path.parent().unwrap(),
                    root.join(hex::encode(job)).join("017"),
                )
                .unwrap();
            }
            "temp" => {
                fs::write(path.with_extension("tmp"), b"unfinished").unwrap();
            }
            "symlink" => {
                fs::remove_file(&path).unwrap();
                std::os::unix::fs::symlink("absent", &path).unwrap();
            }
            "oversized" => {
                fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(128 * 1024 + 1)
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let before = fingerprint(source.path());
        let result = MaterializationReferenceReaderV1::open_existing(&root)
            .and_then(|reader| reader.visit_references(&mut |_, _, _| Ok(())));
        assert!(result.is_err(), "{case}");
        assert_eq!(fingerprint(source.path()), before, "{case}");
    }
}

#[test]
fn missing_and_released_references_are_not_recreated_and_callback_failure_stops() {
    let source = tempfile::tempdir().unwrap();
    let root = source.path().join("references");
    assert!(MaterializationReferenceReaderV1::open_existing(&root).is_err());
    assert!(!root.exists());
    let job = B256::repeat_byte(1);
    let path = native_record(&root, job, 0);
    fs::remove_file(&path).unwrap();
    let before = fingerprint(source.path());
    let reader = MaterializationReferenceReaderV1::open_existing(&root).unwrap();
    reader
        .visit_references(&mut |_, _, _| panic!("released record must be absent"))
        .unwrap();
    assert!(reader.load_exact(job, 0).unwrap().is_none());
    assert_eq!(fingerprint(source.path()), before);
    native_record(&root, job, 0);
    native_record(&root, job, 1);
    let before = fingerprint(source.path());
    let mut calls = 0;
    assert!(reader
        .visit_references(&mut |_, _, _| {
            calls += 1;
            Err(outbe_ocomp::nod_materialization::MaterializationReferenceErrorV1::InvalidRecord)
        })
        .is_err());
    assert_eq!(calls, 1);
    assert_eq!(fingerprint(source.path()), before);
}
