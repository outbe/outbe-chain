mod payout {
    use std::{
        collections::BTreeMap,
        fs::{self, File},
        io::{Read, Write},
        os::unix::fs::{symlink, MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
    };

    use alloy_primitives::{Address, B256, U256};
    use outbe_intex::{
        payout::{contributor_list_root, encode_contributor_leaf, ContributorLeafData},
        CertifiedContributorGenerationProjection,
    };
    use outbe_ocomp::payout_artifact::{
        verify_contributor_payout_artifact, PayoutArtifactError, CONTRIBUTOR_PAYOUT_ARTIFACT_FILE,
    };

    fn leaf(index: u32) -> ContributorLeafData {
        ContributorLeafData {
            owner: Address::repeat_byte((index % 251 + 1) as u8),
            source_tribute_id: (U256::from(20_260_718_u32) << 224) | U256::from(index),
            nominal: U256::from(u64::from(index) + 1),
        }
    }

    fn certified(leaves: &[ContributorLeafData]) -> CertifiedContributorGenerationProjection {
        CertifiedContributorGenerationProjection {
            worldwide_day: 20_260_718,
            series_version: 1,
            contributor_count: u32::try_from(leaves.len()).unwrap(),
            contributor_root: contributor_list_root(
                u32::try_from(leaves.len()).unwrap(),
                leaves.iter().map(encode_contributor_leaf),
            )
            .unwrap(),
            eligible_nominal_total: leaves.iter().fold(U256::ZERO, |total, leaf| {
                total.checked_add(leaf.nominal).unwrap()
            }),
        }
    }

    fn artifact(root: &Path, leaves: &[ContributorLeafData]) -> PathBuf {
        let job = root.join("supervisor-v1/jobs").join("11".repeat(32));
        fs::create_dir_all(&job).unwrap();
        let path = job.join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE);
        let mut file = File::create(&path).unwrap();
        for leaf in leaves {
            file.write_all(&encode_contributor_leaf(leaf)).unwrap();
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        fs::write(
            job.join("contributor-payout-v1.bin.tmp"),
            b"untouched pending writer bytes",
        )
        .unwrap();
        fs::write(
            root.join("protected-submission-journal"),
            b"signing authority stays local",
        )
        .unwrap();
        path
    }

    #[derive(Debug, Eq, PartialEq)]
    struct Entry {
        mode: u32,
        len: u64,
        link: Option<PathBuf>,
        digest: Option<B256>,
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Entry> {
        fn visit(root: &Path, path: &Path, output: &mut BTreeMap<PathBuf, Entry>) {
            let metadata = fs::symlink_metadata(path).unwrap();
            let mut entry = Entry {
                mode: metadata.mode(),
                len: metadata.len(),
                link: None,
                digest: None,
            };
            if metadata.file_type().is_symlink() {
                entry.link = Some(fs::read_link(path).unwrap());
            } else if metadata.is_file() {
                let mut file = File::open(path).unwrap();
                let mut hash = alloy_primitives::Keccak256::new();
                let mut buffer = [0; 8192];
                loop {
                    let read = file.read(&mut buffer).unwrap();
                    if read == 0 {
                        break;
                    }
                    hash.update(&buffer[..read]);
                }
                entry.digest = Some(hash.finalize());
            }
            output.insert(path.strip_prefix(root).unwrap().to_path_buf(), entry);
            if metadata.is_dir() {
                for child in fs::read_dir(path).unwrap() {
                    visit(root, &child.unwrap().path(), output);
                }
            }
        }
        let mut output = BTreeMap::new();
        visit(root, root, &mut output);
        output
    }

    #[test]
    fn certified_artifact_streams_native_roots_across_padding_boundaries_without_intermediates() {
        for count in [0, 1, 2, 3, 255, 256, 257, 4097] {
            let source = tempfile::tempdir().unwrap();
            let leaves: Vec<_> = (0..count).map(leaf).collect();
            let certified = certified(&leaves);
            let path = artifact(source.path(), &leaves);
            // No CE database, admission catalog, plan or result catalog is present.
            let before = snapshot(source.path());
            let report = verify_contributor_payout_artifact(&path, &certified).unwrap();
            assert_eq!(report.contributor_count, certified.contributor_count);
            assert_eq!(report.contributor_root, certified.contributor_root);
            assert_eq!(
                report.eligible_nominal_total,
                certified.eligible_nominal_total
            );
            assert_eq!(snapshot(source.path()), before);
        }
    }

    #[test]
    fn artifact_rejects_partial_records_extra_records_and_certified_count_mismatch() {
        for case in ["truncated", "extra-byte", "extra-record", "count"] {
            let source = tempfile::tempdir().unwrap();
            let leaves: Vec<_> = (0..3).map(leaf).collect();
            let mut certified = certified(&leaves);
            let path = artifact(source.path(), &leaves);
            match case {
                "truncated" => fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(251)
                    .unwrap(),
                "extra-byte" => fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(&[0])
                    .unwrap(),
                "extra-record" => fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(&encode_contributor_leaf(&leaf(4)))
                    .unwrap(),
                "count" => certified.contributor_count += 1,
                _ => unreachable!(),
            }
            let before = snapshot(source.path());
            assert!(
                matches!(
                    verify_contributor_payout_artifact(&path, &certified),
                    Err(PayoutArtifactError::LengthMismatch { .. })
                ),
                "{case}"
            );
            assert_eq!(snapshot(source.path()), before);
        }
    }

    #[test]
    fn artifact_rejects_changed_leaf_order_root_and_nominal_total() {
        for case in ["nominal", "owner", "order", "root", "total"] {
            let source = tempfile::tempdir().unwrap();
            let mut leaves: Vec<_> = (0..3).map(leaf).collect();
            let mut certified = certified(&leaves);
            match case {
                "nominal" => leaves[1].nominal += U256::from(1),
                "owner" => leaves[1].owner = Address::repeat_byte(99),
                "order" => leaves.reverse(),
                "root" => certified.contributor_root = B256::repeat_byte(99),
                "total" => certified.eligible_nominal_total += U256::from(1),
                _ => unreachable!(),
            }
            let path = artifact(source.path(), &leaves);
            let before = snapshot(source.path());
            let error = verify_contributor_payout_artifact(&path, &certified).unwrap_err();
            if case == "total" {
                assert!(matches!(error, PayoutArtifactError::TotalMismatch { .. }));
            } else {
                assert!(matches!(error, PayoutArtifactError::RootMismatch { .. }));
            }
            assert_eq!(snapshot(source.path()), before);
        }
    }

    #[test]
    fn artifact_reports_nominal_overflow_and_missing_certified_authority() {
        let source = tempfile::tempdir().unwrap();
        let mut leaves = vec![leaf(0), leaf(1)];
        leaves[0].nominal = U256::MAX;
        leaves[1].nominal = U256::from(1);
        let path = artifact(source.path(), &leaves);
        let mut authority = CertifiedContributorGenerationProjection {
            worldwide_day: 20_260_718,
            series_version: 1,
            contributor_root: contributor_list_root(2, leaves.iter().map(encode_contributor_leaf))
                .unwrap(),
            contributor_count: 2,
            eligible_nominal_total: U256::MAX,
        };
        let before = snapshot(source.path());
        assert!(matches!(
            verify_contributor_payout_artifact(&path, &authority),
            Err(PayoutArtifactError::NominalOverflow)
        ));
        authority.contributor_root = B256::ZERO;
        assert!(matches!(
            verify_contributor_payout_artifact(&path, &authority),
            Err(PayoutArtifactError::InvalidCertifiedGeneration(_))
        ));
        authority.contributor_root = B256::repeat_byte(1);
        authority.series_version = 0;
        assert!(matches!(
            verify_contributor_payout_artifact(&path, &authority),
            Err(PayoutArtifactError::InvalidCertifiedGeneration(_))
        ));
        assert_eq!(snapshot(source.path()), before);
    }

    #[test]
    fn artifact_rejects_symlinks_nonregular_and_missing_paths_without_creating_or_repairing() {
        let source = tempfile::tempdir().unwrap();
        let leaves = vec![leaf(1)];
        let authority = certified(&leaves);
        let path = artifact(source.path(), &leaves);
        let link = source.path().join("linked-artifact");
        symlink(&path, &link).unwrap();
        let foreign = source
            .path()
            .join("supervisor-v1/jobs")
            .join("22".repeat(32))
            .join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE);
        let before = snapshot(source.path());
        assert!(verify_contributor_payout_artifact(&link, &authority).is_err());
        assert!(matches!(
            verify_contributor_payout_artifact(source.path(), &authority),
            Err(PayoutArtifactError::NotRegularFile)
        ));
        assert!(
            matches!(verify_contributor_payout_artifact(&foreign, &authority), Err(PayoutArtifactError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(!foreign.parent().unwrap().exists());
        assert_eq!(snapshot(source.path()), before);
    }
}

mod receipt_listing {
    use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

    use alloy_primitives::B256;
    use outbe_ocomp::export_receipt::{list_prepared_export_jobs_read_only, ExportReceiptError};

    fn fingerprint(root: &Path) -> Vec<(std::path::PathBuf, u32, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, entries: &mut Vec<(std::path::PathBuf, u32, Vec<u8>)>) {
            let metadata = fs::symlink_metadata(path).unwrap();
            let bytes = if metadata.is_file() {
                fs::read(path).unwrap()
            } else if metadata.file_type().is_symlink() {
                fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else {
                vec![]
            };
            entries.push((
                path.strip_prefix(root).unwrap().to_path_buf(),
                metadata.permissions().mode(),
                bytes,
            ));
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), entries);
                }
            }
        }
        let mut entries = vec![];
        visit(root, root, &mut entries);
        entries.sort();
        entries
    }

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
}

mod materialization {
    use alloy_primitives::B256;
    use outbe_ocomp::nod_materialization::{
        MaterializationReferenceReaderV1, MaterializationReferenceStoreV1,
    };
    use outbe_ocomp_protocol::CasObjectRefV1;
    use std::{fs, os::unix::fs::PermissionsExt as _, path::Path};

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

    fn fingerprint(root: &Path) -> Vec<(std::path::PathBuf, u32, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, entries: &mut Vec<(std::path::PathBuf, u32, Vec<u8>)>) {
            let metadata = fs::symlink_metadata(path).unwrap();
            let bytes = if metadata.is_file() {
                fs::read(path).unwrap()
            } else if metadata.file_type().is_symlink() {
                fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else {
                vec![]
            };
            entries.push((
                path.strip_prefix(root).unwrap().to_path_buf(),
                metadata.permissions().mode(),
                bytes,
            ));
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), entries);
                }
            }
        }
        let mut entries = vec![];
        visit(root, root, &mut entries);
        entries.sort();
        entries
    }

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
        assert!(reader.visit_references(&mut |_,_,_| { calls += 1; Err(outbe_ocomp::nod_materialization::MaterializationReferenceErrorV1::InvalidRecord) }).is_err());
        assert_eq!(calls, 1);
        assert_eq!(fingerprint(source.path()), before);
    }
}
