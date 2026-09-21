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

mod admission {
    mod support {
        include!("support/mod.rs");
    }
    use alloy_primitives::{Address, B256, U256};
    use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
    use outbe_lysis::program_v1::{
        planner::{
            LysisPlanTopologyV1, LysisPlannerBindingsV1, LysisPlannerV1, PlannedUnitPositionV1,
        },
        result::{
            encode_root_reduce_output, LysisListSubtreeCarrierV1, RootReduceOutputV1,
            RootReduceSummaryV1,
        },
    };
    use outbe_ocomp::{
        admission_catalog::{
            AdmissionCatalogError, AdmissionCatalogReader, AdmissionPositionV1,
            VerifiedAdmissionCatalog,
        },
        bundle::PinnedProtocolBundle,
        cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
        control::poc_schema_limits,
        input_artifacts::{
            poc_input_list_limits, publish_input_artifact_set, InputArtifactContents,
            InputArtifactIdentity,
        },
        input_ref_catalog::{InputRefCatalogError, VerifiedInputChunkRefCatalog},
        lysis_plan_audit::{LocalLysisPlanAuditV1, LysisPlanAuditStepV1},
        lysis_result_catalog::{ExactLysisResultCatalogCursorV1, LysisResultCatalogStepV1},
    };
    use outbe_ocomp_protocol::{
        common::{BoundedBytes, ProofBytes},
        input::{
            materialize_authenticated_openings, CheckpointIdentityV1, InputChunkKind,
            InputManifestV1,
        },
        opening::{
            partition_lysis_opening_subjects, LysisOpeningsProofV1, RawContractOpeningProofV1,
            RawStorageSlotV1,
        },
        registry::ObjectKind,
        result::{ContributorActionV1, NodActionV1, OutputManifestEntryV1, ResultChunkV1},
        unit::{UnitArtifactV1, UnitPhase, WorkOutputHeaderV1},
        ListKind,
    };
    use outbe_primitives::time::WorldwideDay;
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::{symlink, MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
    };
    const CAS_LIMITS: CasLimits = CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: 64 * 1_048_576,
    };
    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }
    struct Fixture {
        _directory: tempfile::TempDir,
        cas_root: PathBuf,
        input_ref_root: PathBuf,
        admission_root: PathBuf,
        limits: outbe_ocomp_protocol::SchemaLimits,
        bundle: PinnedProtocolBundle,
        expected_count: u32,
    }
    // Protocol-shaped fixture using native codecs/planner. Non-root phase payloads
    // are minimal: this checks stored evidence, not real worker execution.
    fn fixture(job_seed: u8) -> Fixture {
        let limits = poc_schema_limits();
        let list_limits = poc_input_list_limits();
        let bundle = support::protocol_bundle();
        let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
        let pinned_bundle = PinnedProtocolBundle::decode(
            &bundle.encode_canonical(&limits).unwrap(),
            bundle_hash,
            &limits,
        )
        .unwrap();
        let job_id = hash(job_seed);
        let tribute_count = 1_u32;
        let day = WorldwideDay::new(20_260_725);

        let mut tributes = (0..tribute_count)
            .map(|index| {
                let mut owner_bytes = [0_u8; 20];
                owner_bytes[16..].copy_from_slice(&(index + 1).to_be_bytes());
                let owner = Address::from(owner_bytes);
                TributeBodyV1 {
                    tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
                    owner,
                    worldwide_day: day,
                    issuance_amount_minor: U256::from(1),
                    issuance_currency: if index % 2 == 0 { 840 } else { 826 },
                    nominal_amount_minor: U256::from((index % 7) + 1),
                    reference_currency: if index % 3 == 0 { 978 } else { 392 },
                    tribute_price_minor: U256::from(1),
                    exclude_from_intex_issuance: false,
                }
            })
            .collect::<Vec<_>>();
        tributes.sort_by_key(|tribute| tribute.tribute_id);
        let mut contributors_by_owner = tributes
            .iter()
            .map(|tribute| ContributorActionV1 {
                owner: tribute.owner,
                source_tribute_id: *tribute.tribute_id,
                nominal_amount_minor: tribute.nominal_amount_minor,
            })
            .collect::<Vec<_>>();
        contributors_by_owner
            .sort_by_key(|contributor| (contributor.owner, contributor.source_tribute_id));
        let nod_action_tributes = tributes.clone();
        let owners = tributes
            .iter()
            .map(|tribute| tribute.owner)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut reference_isos = tributes
            .iter()
            .map(|tribute| tribute.reference_currency)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        reference_isos.push(840);
        reference_isos.sort_unstable();
        reference_isos.dedup();
        let finalized_state_root = hash(0x32);
        let raw_opening = |address, slot_byte| RawContractOpeningProofV1 {
            contract_address: address,
            state_root: finalized_state_root,
            ordered_slots: vec![RawStorageSlotV1 {
                slot: hash(slot_byte),
                value: U256::from(1),
            }],
            account_proof: ProofBytes(vec![0xa1]),
            storage_proof: ProofBytes(vec![0xb1]),
        };
        let mut fidelity_openings = Vec::new();
        let mut oracle_opening = None;
        for subjects in partition_lysis_opening_subjects(&owners, &reference_isos, &limits).unwrap()
        {
            let openings = materialize_authenticated_openings(
                &LysisOpeningsProofV1 {
                    protocol_bundle_hash: bundle_hash,
                    job_id,
                    finalized_block_hash: hash(0x31),
                    finalized_state_root,
                    wwd: day.value(),
                    subjects,
                    fidelity: raw_opening(Address::repeat_byte(0x63), 0x64),
                    oracle: raw_opening(Address::repeat_byte(0x65), 0x66),
                },
                &bundle,
                &limits,
            )
            .unwrap();
            fidelity_openings.push(openings.fidelity);
            match &oracle_opening {
                None => oracle_opening = Some(openings.oracle),
                Some(existing) => assert_eq!(existing, &openings.oracle),
            }
        }

        let directory = support::tempdir().unwrap();
        let cas_root = directory.path().join("cas");
        let input_ref_root = directory.path().join("input-refs");
        let admission_root = directory.path().join("admissions");
        let cas = FilesystemCas::open(&cas_root, CasWriterRole::Supervisor, CAS_LIMITS).unwrap();
        let published = publish_input_artifact_set(
            &cas,
            &input_ref_root,
            &bundle,
            InputArtifactContents {
                identity: InputArtifactIdentity {
                    job_id,
                    attempt: 0,
                    checkpoint: CheckpointIdentityV1 {
                        finalized_block_number: 90,
                        finalized_block_hash: hash(0x31),
                        finalized_state_root,
                        finalized_ce_root: hash(0x33),
                        ce_schema_version: 1,
                    },
                    wwd: day.value(),
                    sealed_tribute_collection_key: hash(0x34),
                    sealed_tribute_collection_root: hash(0x35),
                },
                canonical_tributes: tributes
                    .iter()
                    .map(|tribute| encode_tribute_v1(tribute).unwrap())
                    .collect(),
                fidelity_openings,
                oracle_opening: oracle_opening.unwrap(),
            },
            &limits,
            list_limits,
        )
        .unwrap();
        let manifest = InputManifestV1::decode_canonical(
            cas.read_verified(&published.manifest_ref).unwrap().bytes(),
            &limits,
        )
        .unwrap();
        let input_refs_for_plan = VerifiedInputChunkRefCatalog::open(
            &input_ref_root,
            &cas,
            &published.manifest_ref,
            limits,
            list_limits,
        )
        .unwrap();
        let all_input_refs = input_refs_for_plan
            .exact_cursor()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let tribute_refs = all_input_refs
            .iter()
            .filter(|reference| reference.kind == InputChunkKind::Tribute)
            .cloned()
            .collect::<Vec<_>>();
        drop(input_refs_for_plan);
        let manifest_ref = published.manifest_ref;

        let planner = LysisPlannerV1::new(LysisPlannerBindingsV1 {
            protocol_bundle_hash: bundle_hash,
            job_id,
            attempt: 0,
            input_manifest_hash: manifest.manifest_hash(&limits).unwrap(),
            input_manifest_encoded_bytes: manifest_ref.encoded_bytes,
            fidelity_opening_root: manifest.fidelity_opening_root,
            oracle_opening_root: manifest.oracle_opening_root,
            wwd: manifest.wwd,
            lysis_limit_minor: U256::from(200),
            logical_evaluation_time: 1_784_765_900,
            tribute_count: manifest.tribute_count,
            lysis_program_semantics_hash: bundle.lysis_program_semantics_hash,
            planner_spec_version: bundle.planner_spec_version,
            reducer_spec_version: bundle.reducer_spec_version,
        })
        .unwrap();
        let plan = planner
            .commit_primary_catalog(tribute_refs.clone(), &limits)
            .unwrap();
        let plan_ref = cas
            .publish_bytes(&plan.encode_canonical_record(&limits).unwrap())
            .unwrap();

        let reader = FilesystemCasReader::open(&cas_root, CAS_LIMITS).unwrap();
        let input_refs =
            VerifiedInputChunkRefCatalog::reopen(&input_ref_root, &reader, limits, list_limits)
                .unwrap();
        let mut admissions =
            VerifiedAdmissionCatalog::open(&admission_root, &cas, &plan_ref, &manifest_ref, limits)
                .unwrap();
        let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count).unwrap();
        let plan_hash = plan.plan_hash(&limits).unwrap();

        for plan_ordinal in 0..topology.total_unit_count() {
            let spec = {
                let audit = LocalLysisPlanAuditV1::open(
                    &admissions,
                    &input_refs,
                    &reader,
                    &pinned_bundle,
                    &limits,
                )
                .unwrap();
                audit.candidate_spec_at(plan_ordinal).unwrap()
            };
            let (artifact, result_entry) = match topology.plan_position_at(plan_ordinal).unwrap() {
                PlannedUnitPositionV1::TreeNode {
                    phase: UnitPhase::RootReduce,
                    level: 0,
                    index,
                } => {
                    let start = usize::try_from(index * 256).unwrap();
                    let end = (start + 256).min(tributes.len());
                    let actions = nod_action_tributes[start..end]
                        .iter()
                        .enumerate()
                        .map(|(local, tribute)| {
                            let tribute_id = *tribute.tribute_id;
                            NodActionV1 {
                                raw_ordinal: u32::try_from(start + local).unwrap(),
                                tribute_id,
                                nod_id: tribute_id,
                                owner: tribute.owner,
                                wwd: day.value(),
                                league_id: 1,
                                floor_price_minor: U256::ZERO,
                                gratis_load_minor: U256::from(1),
                                entry_price_minor: U256::ZERO,
                                settlement_cost_minor: U256::from(2),
                                issuance_currency: tribute.issuance_currency,
                                reference_currency: tribute.reference_currency,
                                issued_at: 1_784_765_900,
                                bucket_key: hash(u8::try_from(local % 251).unwrap()),
                            }
                        })
                        .collect::<Vec<_>>();
                    let contributors = contributors_by_owner[start..end].to_vec();
                    let chunk = ResultChunkV1 {
                        protocol_bundle_hash: bundle_hash,
                        job_id,
                        attempt: 0,
                        chunk_ordinal: index,
                        first_nod_ordinal: u32::try_from(start).unwrap(),
                        ordered_nod_actions: actions.clone(),
                        ordered_eligible_contributors: contributors.clone(),
                    };
                    let chunk_hash = chunk.result_chunk_hash(&limits).unwrap();
                    let mut chunk_ref = cas
                        .publish_bytes(&chunk.encode_canonical(&limits).unwrap())
                        .unwrap();
                    chunk_ref.expected_ocb1_kind = Some(ObjectKind::ResultChunkV1.tag());
                    let entry = OutputManifestEntryV1 {
                        chunk_ordinal: index,
                        result_chunk_hash: chunk_hash,
                        result_chunk_ref: chunk_ref,
                    };
                    let nod_records = actions
                        .iter()
                        .map(|action| action.encode_canonical_record(&limits).unwrap())
                        .collect::<Vec<_>>();
                    let bucket_records = (start..end)
                        .map(|ordinal| ordinal.to_be_bytes().to_vec())
                        .collect::<Vec<_>>();
                    let contributor_records = contributors
                        .iter()
                        .map(|contributor| contributor.encode_canonical_record(&limits).unwrap())
                        .collect::<Vec<_>>();
                    let manifest_records = vec![entry.encode_canonical_record(&limits).unwrap()];
                    let chunk_hash_records = vec![chunk_hash.as_slice().to_vec()];
                    let count = u32::try_from(end - start).unwrap();
                    let raw_nominal_total = tributes[start..end]
                        .iter()
                        .fold(U256::ZERO, |total, tribute| {
                            total.checked_add(tribute.nominal_amount_minor).unwrap()
                        });
                    let nod_cost_total = actions.iter().fold(U256::ZERO, |total, action| {
                        total.checked_add(action.settlement_cost_minor).unwrap()
                    });
                    let summary = RootReduceSummaryV1 {
                        protocol_bundle_hash: bundle_hash,
                        job_id,
                        attempt: 0,
                        plan_hash,
                        covered_primary_start: index,
                        covered_primary_count: 1,
                        nod_actions: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::NodActions,
                            index,
                            &nod_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        bucket_records: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::BucketRecords,
                            index,
                            &bucket_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        contributor_actions: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::ContributorActions,
                            index,
                            &contributor_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        output_manifest_entries: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::CompleteOutputManifest,
                            index,
                            &manifest_records,
                            limits.max_bounded_bytes,
                        )
                        .unwrap(),
                        result_chunk_hashes: LysisListSubtreeCarrierV1::from_primary_page(
                            ListKind::ResultChunkHashes,
                            index,
                            &chunk_hash_records,
                            B256::len_bytes(),
                        )
                        .unwrap(),
                        tribute_count: count,
                        nod_count: count,
                        bucket_count: count,
                        contributor_count: u32::try_from(contributors.len()).unwrap(),
                        tribute_nominal_total: raw_nominal_total,
                        eligible_nominal_total: raw_nominal_total,
                        lysis_allocation_minor: U256::from(count),
                        nod_cost_total,
                        first_error_ordinal: None,
                    };
                    let coverage_root = summary.result_chunk_hashes.tree_root;
                    let output_coverage_root = coverage_root;
                    (
                        UnitArtifactV1::from_canonical_output(
                            &spec,
                            WorkOutputHeaderV1 {
                                source_coverage_root: coverage_root,
                                output_coverage_root,
                                source_coverage_count: 1,
                                output_coverage_count: 1,
                            },
                            BoundedBytes(
                                encode_root_reduce_output(
                                    &RootReduceOutputV1::Leaf {
                                        summary,
                                        output_manifest_entry: entry.clone(),
                                    },
                                    &limits,
                                )
                                .unwrap(),
                            ),
                            &limits,
                        )
                        .unwrap(),
                        Some(entry),
                    )
                }
                _ => (
                    UnitArtifactV1::from_canonical_output(
                        &spec,
                        WorkOutputHeaderV1 {
                            source_coverage_root: hash(0xa1),
                            output_coverage_root: hash(0xa2),
                            source_coverage_count: 1,
                            output_coverage_count: 1,
                        },
                        BoundedBytes(vec![0x42]),
                        &limits,
                    )
                    .unwrap(),
                    None,
                ),
            };
            let mut artifact_ref = cas
                .publish_bytes(&artifact.encode_canonical(&limits).unwrap())
                .unwrap();
            artifact_ref.expected_ocb1_kind = Some(ObjectKind::UnitArtifactV1.tag());
            admissions
                .admit_verified_unit(
                    AdmissionPositionV1 { plan_ordinal },
                    &spec,
                    artifact_ref,
                    result_entry,
                )
                .unwrap();
        }
        drop(admissions);
        drop(input_refs);
        drop(reader);
        drop(cas);

        Fixture {
            _directory: directory,
            cas_root,
            input_ref_root,
            admission_root,
            limits,
            bundle: pinned_bundle,
            expected_count: topology.total_unit_count(),
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct SnapshotEntry {
        mode: u32,
        len: u64,
        digest: Option<B256>,
        link: Option<PathBuf>,
    }
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, SnapshotEntry> {
        fn visit(root: &Path, path: &Path, entries: &mut BTreeMap<PathBuf, SnapshotEntry>) {
            let m = fs::symlink_metadata(path).unwrap();
            entries.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                SnapshotEntry {
                    mode: m.mode(),
                    len: m.len(),
                    digest: if m.is_file() {
                        Some(alloy_primitives::keccak256(fs::read(path).unwrap()))
                    } else {
                        None
                    },
                    link: if m.file_type().is_symlink() {
                        Some(fs::read_link(path).unwrap())
                    } else {
                        None
                    },
                },
            );
            if m.is_dir() {
                for child in fs::read_dir(path).unwrap() {
                    visit(root, &child.unwrap().path(), entries);
                }
            }
        }
        let mut entries = BTreeMap::new();
        visit(root, root, &mut entries);
        entries
    }

    fn audit_read_only(f: &Fixture) -> Result<(u32, u32), String> {
        let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).map_err(|e| e.to_string())?;
        let inputs = VerifiedInputChunkRefCatalog::reopen(
            &f.input_ref_root,
            &cas,
            f.limits,
            poc_input_list_limits(),
        )
        .map_err(|e| e.to_string())?;
        let admissions = AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits)
            .map_err(|e| e.to_string())?;
        let audit =
            LocalLysisPlanAuditV1::open_read_only(&admissions, &inputs, &cas, &f.bundle, &f.limits)
                .map_err(|e| e.to_string())?;
        let mut plan_complete = 0;
        for step in audit.audit_cursor().map_err(|e| e.to_string())? {
            if matches!(
                step.map_err(|e| e.to_string())?,
                LysisPlanAuditStepV1::Complete
            ) {
                plan_complete += 1;
            }
        }
        assert_eq!(plan_complete, 1);
        let mut chunks = 0;
        let mut results_complete = 0;
        for step in ExactLysisResultCatalogCursorV1::open(&audit).map_err(|e| e.to_string())? {
            match step.map_err(|e| e.to_string())? {
                LysisResultCatalogStepV1::Chunk(_) => chunks += 1,
                LysisResultCatalogStepV1::Complete => results_complete += 1,
                _ => {}
            }
        }
        Ok((chunks, results_complete))
    }

    #[test]
    fn shared_admission_readers_hold_existing_lock_and_preserve_source() {
        let f = fixture(0x30);
        let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
        let lock = f.admission_root.join("catalog.lock");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o400)).unwrap();
        let before = snapshot(f._directory.path());
        let first =
            AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
        let second =
            AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
        assert!(!first.is_abstained());
        let records = first
            .exact_plan_cursor()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(records.len(), f.expected_count as usize);
        assert_eq!(first.read(0).unwrap(), records[0]);
        assert_eq!(second.read(0).unwrap(), records[0]);
        // Restore write access solely for the native writer's exclusion check.
        assert_eq!(snapshot(f._directory.path()), before);
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            VerifiedAdmissionCatalog::reopen(&f.admission_root, &cas, f.limits),
            Err(AdmissionCatalogError::LockHeld(_))
        ));
        drop(first);
        assert!(matches!(
            VerifiedAdmissionCatalog::reopen(&f.admission_root, &cas, f.limits),
            Err(AdmissionCatalogError::LockHeld(_))
        ));
        drop(second);
        let writer = VerifiedAdmissionCatalog::reopen(&f.admission_root, &cas, f.limits).unwrap();
        assert!(matches!(
            AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits),
            Err(AdmissionCatalogError::LockHeld(_))
        ));
        drop(writer);
        AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
    }

    #[test]
    fn concrete_read_only_bridge_completes_plan_and_results_without_mutation() {
        let f = fixture(0x30);
        let before = snapshot(f._directory.path());
        assert_eq!(audit_read_only(&f).unwrap(), (1, 1));
        assert_eq!(snapshot(f._directory.path()), before);
        let lock = f.input_ref_root.join("catalog.lock");
        fs::remove_file(&lock).unwrap();
        let before = snapshot(f._directory.path());
        assert!(audit_read_only(&f).is_err());
        assert!(!lock.exists());
        assert_eq!(snapshot(f._directory.path()), before);
    }

    #[test]
    #[allow(unsafe_code)]
    fn input_ref_reader_rejects_fifo_lock_with_bounded_wait() {
        const CHILD_ROOT: &str = "OUTBE_INPUT_REF_FIFO_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let cas = FilesystemCasReader::open(root.join("cas"), CAS_LIMITS).unwrap();
            assert!(matches!(
                VerifiedInputChunkRefCatalog::reopen(
                    root.join("input-refs"),
                    &cas,
                    poc_schema_limits(),
                    poc_input_list_limits(),
                ),
                Err(InputRefCatalogError::InvalidEnvelope)
            ));
            return;
        }

        let f = fixture(0x30);
        let lock = f.input_ref_root.join("catalog.lock");
        fs::remove_file(&lock).unwrap();
        let fifo_path = std::ffi::CString::new(lock.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: the NUL-terminated path remains alive for the entire call.
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
        let before = snapshot(f._directory.path());
        // A blocking open must fail this test without hanging the test runner.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "admission::input_ref_reader_rejects_fifo_lock_with_bounded_wait",
                "--test-threads=1",
            ])
            .env(CHILD_ROOT, f._directory.path())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(snapshot(f._directory.path()), before);
        assert!(
            status.is_some_and(|status| status.success()),
            "read-only input-ref open blocked on FIFO or failed to reject it"
        );
    }

    #[test]
    fn input_ref_reader_rejects_directory_lock_without_mutation() {
        let f = fixture(0x30);
        let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
        let lock = f.input_ref_root.join("catalog.lock");
        fs::remove_file(&lock).unwrap();
        fs::create_dir(&lock).unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o500)).unwrap();
        let before = snapshot(f._directory.path());
        assert!(matches!(
            VerifiedInputChunkRefCatalog::reopen(
                &f.input_ref_root,
                &cas,
                f.limits,
                poc_input_list_limits(),
            ),
            Err(InputRefCatalogError::InvalidEnvelope)
        ));
        assert_eq!(snapshot(f._directory.path()), before);
    }

    #[test]
    fn input_ref_reader_preserves_shared_and_exclusive_lock_behavior() {
        let f = fixture(0x30);
        let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
        let lock = f.input_ref_root.join("catalog.lock");
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o400)).unwrap();
        let before = snapshot(f._directory.path());
        let reopen = || {
            VerifiedInputChunkRefCatalog::reopen(
                &f.input_ref_root,
                &cas,
                f.limits,
                poc_input_list_limits(),
            )
        };
        let first = reopen().unwrap();
        let second = reopen().unwrap();
        let exclusive = fs::File::open(&lock).unwrap();
        assert!(exclusive.try_lock().is_err());
        drop(first);
        assert!(exclusive.try_lock().is_err());
        drop(second);
        exclusive.try_lock().unwrap();
        assert!(matches!(reopen(), Err(InputRefCatalogError::LockHeld(path)) if path == lock));
        exclusive.unlock().unwrap();
        drop(reopen().unwrap());
        assert_eq!(snapshot(f._directory.path()), before);
    }

    #[test]
    fn admission_reader_missing_and_unsafe_inputs_fail_without_repair() {
        let f = fixture(0x30);
        let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
        let missing = f._directory.path().join("absent");
        let before = snapshot(f._directory.path());
        assert!(AdmissionCatalogReader::open_existing(&missing, &cas, f.limits).is_err());
        assert_eq!(snapshot(f._directory.path()), before);
        let alias = f._directory.path().join("alias");
        symlink(&f.admission_root, &alias).unwrap();
        let before = snapshot(f._directory.path());
        assert!(AdmissionCatalogReader::open_existing(&alias, &cas, f.limits).is_err());
        assert_eq!(snapshot(f._directory.path()), before);
        fs::remove_file(alias).unwrap();
        for name in ["catalog.lock", "catalog.header"] {
            let path = f.admission_root.join(name);
            let backup = f._directory.path().join("backup");
            fs::rename(&path, &backup).unwrap();
            let before = snapshot(f._directory.path());
            assert!(
                AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).is_err()
            );
            assert!(!path.exists());
            assert_eq!(snapshot(f._directory.path()), before);
            symlink(&backup, &path).unwrap();
            let before = snapshot(f._directory.path());
            assert!(
                AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).is_err()
            );
            assert_eq!(snapshot(f._directory.path()), before);
            fs::remove_file(&path).unwrap();
            fs::rename(backup, path).unwrap();
        }
        let header = f.admission_root.join("catalog.header");
        let bytes = fs::read(&header).unwrap();
        fs::write(&header, b"corrupt").unwrap();
        let before = snapshot(f._directory.path());
        assert!(AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).is_err());
        assert_eq!(snapshot(f._directory.path()), before);
        fs::write(header, bytes).unwrap();
        let temp = f.admission_root.join("0000000000.admission.tmp");
        fs::write(&temp, b"interrupted").unwrap();
        let before = snapshot(f._directory.path());
        assert!(matches!(
            AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits),
            Err(AdmissionCatalogError::AmbiguousTemporary(_))
        ));
        assert_eq!(snapshot(f._directory.path()), before);
        fs::remove_file(temp).unwrap();
        fs::write(f.admission_root.join("catalog.abstained"), b"latched").unwrap();
        let before = snapshot(f._directory.path());
        let reader =
            AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
        assert!(reader.is_abstained());
        assert!(reader.read(0).is_err());
        assert!(reader.exact_plan_cursor().is_err());
        drop(reader);
        assert!(audit_read_only(&f).is_err());
        assert_eq!(snapshot(f._directory.path()), before);
    }

    #[test]
    fn concrete_read_only_bridge_rejects_missing_duplicate_and_foreign_admissions() {
        let f = fixture(0x30);
        let foreign = fixture(0x40);
        let path = f.admission_root.join("0000000000.admission");
        let original = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let before = snapshot(f._directory.path());
        let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
        let reader =
            AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
        assert!(reader.read(0).is_err());
        assert!(reader.exact_plan_cursor().is_err());
        drop(reader);
        assert!(audit_read_only(&f).is_err());
        assert_eq!(snapshot(f._directory.path()), before);
        fs::write(&path, &original).unwrap();
        let duplicate = f
            .admission_root
            .join(format!("{:010}.admission", f.expected_count));
        fs::write(&duplicate, &original).unwrap();
        let before = snapshot(f._directory.path());
        let reader =
            AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
        assert!(reader.exact_plan_cursor().is_err());
        drop(reader);
        assert!(audit_read_only(&f).is_err());
        assert_eq!(snapshot(f._directory.path()), before);
        fs::remove_file(duplicate).unwrap();
        fs::copy(foreign.admission_root.join("0000000000.admission"), &path).unwrap();
        let before = snapshot(f._directory.path());
        let reader =
            AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits).unwrap();
        assert!(reader.exact_plan_cursor().unwrap().next().unwrap().is_err());
        drop(reader);
        assert!(audit_read_only(&f).is_err());
        assert_eq!(snapshot(f._directory.path()), before);
    }

    #[test]
    #[allow(unsafe_code)]
    fn admission_reader_rejects_fifo_lock_with_bounded_wait() {
        const CHILD_ROOT: &str = "OUTBE_ADMISSION_FIFO_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let cas = FilesystemCasReader::open(root.join("cas"), CAS_LIMITS).unwrap();
            assert!(matches!(
                AdmissionCatalogReader::open_existing(
                    root.join("admissions"),
                    &cas,
                    poc_schema_limits(),
                ),
                Err(AdmissionCatalogError::InvalidEnvelope)
            ));
            return;
        }

        let f = fixture(0x30);
        let lock = f.admission_root.join("catalog.lock");
        fs::remove_file(&lock).unwrap();
        let fifo_path = std::ffi::CString::new(lock.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: the NUL-terminated path remains alive for the entire call.
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
        let before = snapshot(f._directory.path());
        // Isolate the potentially blocking open so a regression cannot hang the
        // test suite. The parent always kills and reaps a timed-out child.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "admission::admission_reader_rejects_fifo_lock_with_bounded_wait",
                "--test-threads=1",
            ])
            .env(CHILD_ROOT, f._directory.path())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(snapshot(f._directory.path()), before);
        assert!(
            status.is_some_and(|status| status.success()),
            "read-only admission open blocked on FIFO or failed to reject it"
        );
    }

    #[test]
    fn admission_reader_rejects_directory_lock_without_mutation() {
        let f = fixture(0x30);
        let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
        let lock = f.admission_root.join("catalog.lock");
        fs::remove_file(&lock).unwrap();
        fs::create_dir(&lock).unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o500)).unwrap();
        let before = snapshot(f._directory.path());
        assert!(matches!(
            AdmissionCatalogReader::open_existing(&f.admission_root, &cas, f.limits),
            Err(AdmissionCatalogError::InvalidEnvelope)
        ));
        assert_eq!(snapshot(f._directory.path()), before);
    }
}
mod export_binding {
    mod support {
        include!("support/mod.rs");
    }
    use alloy_primitives::{Address, B256, U256};
    use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
    use outbe_ocomp::{
        cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
        control::poc_schema_limits,
        export_binding::{
            ExportBindingCandidate, ExportedManifestBindingReader, ExportedManifestBindingStore,
        },
        input_artifacts::derive_input_chunk_ref,
        input_ref_catalog::VerifiedInputChunkRefCatalog,
        supervisor::DiscoveryRecord,
    };
    use outbe_ocomp_protocol::{
        common::BoundedBytes,
        control::FinalizedJobSpecV1,
        input::{
            AuthenticatedInputChunkV1, CheckpointIdentityV1, Compression, InputChunkKind,
            InputManifestV1,
        },
        intent::JobIntentV1,
        profile::ProtocolBundleV1,
        CasObjectRefV1, ListKind, ObjectKind, OrderedListLimits, SchemaLimits,
        SnapshotExportCommittedV1,
    };
    use outbe_primitives::time::WorldwideDay;
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::{symlink, MetadataExt, PermissionsExt},
        path::{Path, PathBuf},
    };

    struct Fixture {
        directory: tempfile::TempDir,
        binding_root: PathBuf,
        cas_root: PathBuf,
        reader: FilesystemCasReader,
        catalog: VerifiedInputChunkRefCatalog,
        limits: SchemaLimits,
        bundle: ProtocolBundleV1,
        spec: FinalizedJobSpecV1,
        binding_ref: CasObjectRefV1,
        manifest_ref: CasObjectRefV1,
        chunk_ref: CasObjectRefV1,
        committed: SnapshotExportCommittedV1,
    }

    fn fixture(seed: u8) -> Fixture {
        let limits = poc_schema_limits();
        let list_limits = OrderedListLimits::new(16, 4096, 4096);
        let bundle = support::protocol_bundle();
        let spec = support::finalized_job_spec(seed, 90, 1, B256::repeat_byte(250));
        let intent = JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &limits).unwrap();
        let directory = support::tempdir().unwrap();
        let cas_root = directory.path().join("cas");
        let binding_root = directory.path().join("binding");
        let cas_limits = CasLimits {
            max_object_bytes: 1_048_576,
            max_total_bytes: 8_388_608,
        };
        let cas =
            FilesystemCas::open(&cas_root, CasWriterRole::SnapshotExporter, cas_limits).unwrap();
        let reader = FilesystemCasReader::open(&cas_root, cas_limits).unwrap();
        let day = WorldwideDay::new(intent.wwd);
        let owner = Address::repeat_byte(1);
        let tribute = TributeBodyV1 {
            tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
            owner,
            worldwide_day: day,
            issuance_amount_minor: U256::from(1),
            issuance_currency: 840,
            nominal_amount_minor: U256::from(1),
            reference_currency: 978,
            tribute_price_minor: U256::from(1),
            exclude_from_intex_issuance: false,
        };
        let chunk = AuthenticatedInputChunkV1 {
            protocol_bundle_hash: spec.summary.protocol_bundle_hash,
            job_id: spec.summary.job_id,
            kind: InputChunkKind::Tribute,
            ordinal: 0,
            canonical_records_or_openings: vec![BoundedBytes(encode_tribute_v1(&tribute).unwrap())],
        };
        let mut chunk_ref = cas
            .publish_bytes(&chunk.encode_canonical(&limits).unwrap())
            .unwrap();
        chunk_ref.expected_ocb1_kind = Some(ObjectKind::AuthenticatedInputChunkV1.tag());
        let input_ref =
            derive_input_chunk_ref(&reader.read_verified(&chunk_ref).unwrap(), &bundle, &limits)
                .unwrap()
                .reference;
        let manifest = InputManifestV1 {
            protocol_bundle_hash: spec.summary.protocol_bundle_hash,
            job_id: spec.summary.job_id,
            attempt: intent.attempt,
            checkpoint: CheckpointIdentityV1 {
                finalized_block_number: spec.summary.cursor,
                finalized_block_hash: spec.summary.finalized_block_hash,
                finalized_state_root: spec.summary.finalized_state_root,
                finalized_ce_root: intent.ce_sealed_root,
                ce_schema_version: 1,
            },
            wwd: intent.wwd,
            sealed_tribute_collection_key: intent.sealed_tribute_collection_key,
            sealed_tribute_collection_root: intent.sealed_tribute_collection_root,
            tribute_count: intent.authenticated_day_count,
            tribute_nominal_total: intent.authenticated_day_nominal,
            input_chunk_count: 1,
            input_chunk_list_root: outbe_ocomp_protocol::ordered_list_root(
                ListKind::InputChunkReferences,
                &[input_ref.encode_canonical_record(&limits).unwrap()],
                list_limits,
            )
            .unwrap(),
            fidelity_opening_root: B256::repeat_byte(201),
            oracle_opening_root: B256::repeat_byte(202),
            exact_encoded_bytes: input_ref.encoded_bytes,
            exact_record_count: input_ref.record_count,
            body_codec_id: bundle.tribute_body_codec_id,
            opening_codec_registry_hash: bundle.opening_codec_registry_hash().unwrap(),
            compression: Compression::None,
        };
        let mut manifest_ref = cas
            .publish_bytes(&manifest.encode_canonical(&limits).unwrap())
            .unwrap();
        manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
        let mut catalog = VerifiedInputChunkRefCatalog::open(
            directory.path().join("input-refs"),
            &cas,
            &manifest_ref,
            limits,
            list_limits,
        )
        .unwrap();
        catalog.admit(&input_ref).unwrap();
        let committed = SnapshotExportCommittedV1 {
            job_id: spec.summary.job_id,
            pin_generation: 12,
            record_hash: B256::repeat_byte(203),
        };
        // Only the native producer uses the legacy discovery record. The offline
        // consumer below retains the authenticated immutable spec, not this record.
        let discovery = DiscoveryRecord {
            generation: 7,
            cursor: spec.summary.cursor,
            spec: spec.clone(),
        };
        let binding_ref = {
            let mut store = ExportedManifestBindingStore::open(&binding_root, limits).unwrap();
            store
                .seal(
                    &cas,
                    &reader,
                    ExportBindingCandidate {
                        discovery: &discovery,
                        job_id: spec.summary.job_id,
                        source_pin_generation: 11,
                        lease_generation: 17,
                        checkpoint: &manifest.checkpoint,
                        manifest_ref: &manifest_ref,
                        committed: &committed,
                        bundle: &bundle,
                        input_refs: &catalog,
                    },
                )
                .unwrap()
                .1
                .binding_ref()
        };
        Fixture {
            directory,
            binding_root,
            cas_root,
            reader,
            catalog,
            limits,
            bundle,
            spec,
            binding_ref,
            manifest_ref,
            chunk_ref,
            committed,
        }
    }

    type Fingerprint = BTreeMap<PathBuf, (u32, Option<PathBuf>, Vec<u8>)>;
    fn snapshot(root: &Path) -> Fingerprint {
        fn visit(root: &Path, path: &Path, output: &mut Fingerprint) {
            let metadata = fs::symlink_metadata(path).unwrap();
            let link = metadata
                .file_type()
                .is_symlink()
                .then(|| fs::read_link(path).unwrap());
            let bytes = if metadata.is_file() {
                fs::read(path).unwrap()
            } else {
                Vec::new()
            };
            output.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                (metadata.mode(), link, bytes),
            );
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), output);
                }
            }
        }
        let mut output = BTreeMap::new();
        visit(root, root, &mut output);
        output
    }
    fn cas_path(f: &Fixture, reference: &CasObjectRefV1) -> PathBuf {
        let digest = hex::encode(reference.transport_digest.as_slice());
        f.cas_root
            .join("objects")
            .join(&digest[..2])
            .join(&digest[2..])
    }
    fn load(
        f: &Fixture,
        spec: &FinalizedJobSpecV1,
    ) -> Result<
        outbe_ocomp::export_binding::VerifiedExportedManifestBinding,
        outbe_ocomp::export_binding::ExportBindingError,
    > {
        ExportedManifestBindingReader::open_existing(&f.binding_root, f.limits)?
            .load_exact(&f.reader, spec, &f.bundle, &f.catalog)
    }

    #[test]
    fn authentic_spec_reads_native_binding_without_discovery_spool_or_binding_lock() {
        let f = fixture(20);
        fs::remove_file(f.binding_root.join("binding.lock")).unwrap();
        fs::set_permissions(&f.binding_root, fs::Permissions::from_mode(0o750)).unwrap();
        fs::set_permissions(
            f.binding_root.join("binding.ref"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        assert!(!f.directory.path().join("discovery-spool-v1").exists());
        let before = snapshot(f.directory.path());
        for _ in 0..2 {
            let verified = load(&f, &f.spec).unwrap();
            assert_eq!(verified.binding_ref(), f.binding_ref);
            assert_eq!(verified.manifest_ref(), f.manifest_ref);
            assert_eq!(verified.job_id(), f.spec.summary.job_id);
            assert_eq!(verified.commit_replay_request().pin_generation, 11);
            assert_eq!(verified.commit_replay_request().lease_generation, 17);
            verified.require_exact_node_replay(&f.committed).unwrap();
        }
        assert_eq!(snapshot(f.directory.path()), before);
    }

    #[test]
    fn substitutions_of_spec_catalog_and_commit_are_rejected_without_source_changes() {
        let f = fixture(20);
        let other = fixture(40);
        let before = snapshot(f.directory.path());
        for field in 0..9 {
            let mut spec = f.spec.clone();
            match field {
                0 => spec.summary.cursor += 1,
                1 => spec.summary.job_id = other.spec.summary.job_id,
                2 => spec.summary.finalized_block_hash = other.spec.summary.finalized_block_hash,
                3 => spec.summary.finalized_state_root = other.spec.summary.finalized_state_root,
                4 => spec.summary.protocol_bundle_hash = B256::repeat_byte(249),
                5 => spec.summary.intent_id = other.spec.summary.intent_id,
                6 => spec.summary.open_height += 1,
                7 => spec.summary.deadline_height += 1,
                _ => spec.canonical_job_intent = other.spec.canonical_job_intent.clone(),
            }
            assert!(load(&f, &spec).is_err(), "spec field {field}");
        }
        let reader =
            ExportedManifestBindingReader::open_existing(&f.binding_root, f.limits).unwrap();
        assert!(reader
            .load_exact(&f.reader, &f.spec, &f.bundle, &other.catalog)
            .is_err());
        let binding = load(&f, &f.spec).unwrap();
        let wrong = SnapshotExportCommittedV1 {
            pin_generation: 13,
            ..f.committed.clone()
        };
        assert!(binding.require_exact_node_replay(&wrong).is_err());
        assert_eq!(snapshot(f.directory.path()), before);
    }

    #[test]
    fn missing_corrupt_and_symlinked_binding_paths_are_never_repaired() {
        let f = fixture(20);
        let missing = f.directory.path().join("missing");
        assert!(ExportedManifestBindingReader::open_existing(&missing, f.limits).is_err());
        assert!(!missing.exists());
        let alias = f.directory.path().join("alias");
        symlink(&f.binding_root, &alias).unwrap();
        let before = snapshot(f.directory.path());
        assert!(ExportedManifestBindingReader::open_existing(&alias, f.limits).is_err());
        assert_eq!(snapshot(f.directory.path()), before);
        for name in ["binding.ref.tmp", "binding.abstained", "unexpected"] {
            let path = f.binding_root.join(name);
            fs::write(&path, b"preserved evidence").unwrap();
            let before = snapshot(f.directory.path());
            assert!(load(&f, &f.spec).is_err(), "{name}");
            assert_eq!(snapshot(f.directory.path()), before);
            fs::remove_file(path).unwrap();
        }
        let locator = f.binding_root.join("binding.ref");
        let bytes = fs::read(&locator).unwrap();
        fs::remove_file(&locator).unwrap();
        let before = snapshot(f.directory.path());
        assert!(load(&f, &f.spec).is_err());
        assert_eq!(snapshot(f.directory.path()), before);
        for corrupted in [
            vec![],
            bytes[..bytes.len() - 1].to_vec(),
            [bytes.as_slice(), b"x"].concat(),
            vec![0; bytes.len()],
        ] {
            fs::write(&locator, corrupted).unwrap();
            let before = snapshot(f.directory.path());
            assert!(load(&f, &f.spec).is_err());
            assert_eq!(snapshot(f.directory.path()), before);
        }
        fs::remove_file(&locator).unwrap();
        let target = f.directory.path().join("external-locator");
        fs::write(&target, bytes).unwrap();
        symlink(&target, &locator).unwrap();
        let before = snapshot(f.directory.path());
        assert!(load(&f, &f.spec).is_err());
        assert_eq!(snapshot(f.directory.path()), before);
    }

    #[test]
    fn cas_binding_manifest_and_chunk_corruption_fail_without_mutation() {
        let f = fixture(20);
        for reference in [&f.binding_ref, &f.manifest_ref, &f.chunk_ref] {
            let path = cas_path(&f, reference);
            let original = fs::read(&path).unwrap();
            let mut corrupt = original.clone();
            corrupt[0] ^= 0xff;
            fs::write(&path, corrupt).unwrap();
            let before = snapshot(f.directory.path());
            assert!(load(&f, &f.spec).is_err());
            assert_eq!(snapshot(f.directory.path()), before);
            fs::remove_file(&path).unwrap();
            let before = snapshot(f.directory.path());
            assert!(load(&f, &f.spec).is_err());
            assert_eq!(snapshot(f.directory.path()), before);
            fs::write(&path, original).unwrap();
        }
        load(&f, &f.spec).unwrap();
    }

    #[test]
    fn runtime_cursor_rejection_survives_shared_validation_and_reader_does_not_lock() {
        let f = fixture(20);
        let store = ExportedManifestBindingStore::open(&f.binding_root, f.limits).unwrap();
        let mut discovery = DiscoveryRecord {
            generation: 99,
            cursor: f.spec.summary.cursor,
            spec: f.spec.clone(),
        };
        // Legacy journal generation is intentionally not immutable authority.
        store
            .load_exact(&f.reader, &discovery, &f.bundle, &f.catalog)
            .unwrap();
        discovery.cursor += 1;
        assert!(store
            .load_exact(&f.reader, &discovery, &f.bundle, &f.catalog)
            .is_err());
        let before = snapshot(f.directory.path());
        load(&f, &f.spec).unwrap();
        assert_eq!(snapshot(f.directory.path()), before);
    }
}
