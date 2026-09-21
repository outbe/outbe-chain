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

mod selection {
    use super::CheckName;
    use crate::snapshot::validation::run::CheckSelection;
    use std::collections::BTreeSet;

    #[test]
    fn native_selection_opens_only_actual_prerequisites() {
        use CheckName::*;
        for (requested, expected, projection) in [
            ("headers", vec![Headers], false),
            ("evm", vec![Headers, Evm], false),
            ("ce", vec![Headers, Ce], false),
            ("bodies", vec![Headers, Ce, Bodies], true),
            ("ocomp", vec![Headers, Evm, Ocomp], true),
            ("evm,headers,evm", vec![Headers, Evm], false),
        ] {
            let selection = CheckSelection::resolve(requested, false, false).unwrap();
            assert_eq!(
                selection.checks,
                expected.into_iter().collect::<BTreeSet<_>>(),
                "{requested}"
            );
            assert_eq!(selection.needs_projection(), projection, "{requested}");
        }
    }

    #[test]
    fn native_all_does_not_claim_unsigned_artifacts_were_checked() {
        use CheckName::*;
        assert_eq!(
            CheckSelection::resolve("all", false, false).unwrap().checks,
            [Headers, Evm, Ce, Bodies, Ocomp].into_iter().collect()
        );
        assert_eq!(
            CheckSelection::resolve("all", true, false).unwrap().checks,
            CheckName::ALL.into_iter().collect()
        );
        // Supplying unrelated metadata does not expand an explicit native check.
        assert_eq!(
            CheckSelection::resolve("evm", true, false).unwrap().checks,
            [Headers, Evm].into_iter().collect()
        );
    }

    #[test]
    fn explicit_artifact_checks_and_expected_signer_remain_required_without_inputs() {
        use CheckName::*;
        assert_eq!(
            CheckSelection::resolve("files", false, false)
                .unwrap()
                .checks,
            [Files, Provenance].into_iter().collect()
        );
        assert!(CheckSelection::resolve("files", false, false)
            .unwrap()
            .needs_projection());
        assert_eq!(
            CheckSelection::resolve("provenance", false, false)
                .unwrap()
                .checks,
            [Provenance].into_iter().collect()
        );
        assert_eq!(
            CheckSelection::resolve("evm", false, true).unwrap().checks,
            [Headers, Evm, Provenance].into_iter().collect()
        );
        let report = super::ValidationReport::new(
            CheckSelection::resolve("evm", false, true).unwrap().checks,
        );
        assert_eq!(
            report.check(Provenance).status,
            super::CheckStatus::Incomplete
        );
        assert!(!report.success());
    }

    #[test]
    fn unknown_or_empty_check_selection_is_an_error() {
        for input in ["", "unknown", "all,evm", "evm,", ",headers", "EVM"] {
            assert!(
                CheckSelection::resolve(input, false, false).is_err(),
                "{input}"
            );
        }
    }
}

mod artifact {
    use crate::snapshot::validation::{
        run::{audit_artifact, ArtifactAudit, ValidationInputs},
        Incomplete,
    };
    use outbe_snapshot::{
        archive::write_archive,
        manifest::manifest_digest,
        provenance::{signing_digest, SignatureEnvelope},
    };
    use std::{fs, io::Write, path::Path};

    fn manifest() -> Vec<u8> {
        let block = |n| serde_json::json!({"number":n,"hash":"11".repeat(32)});
        serde_json::to_vec_pretty(&serde_json::json!({
            "version":1,"chain_id":54322345,"genesis_hash":"22".repeat(32),
            "created_at_unix":1789940000_u64,"creator":null,"source":null,
            "progress":{"finalized":block(100),"execution":block(101),"execution_stage":101,
                "finish_stage":101,"partial_state_trie":null,"unwind":null,"storage_version":2,
                "ce":block(100),"projection":block(98),"ocomp_baseline":block(0),
                "ocomp_previous":block(90),"ocomp_current":block(97)},
            "domains":[{"id":"native","kind":"execution-db","native_root":"chain",
                "native_path":"db","mode":448,"entries":[{"path":"data","kind":"file",
                "size":4,"sha256":hex::encode(manifest_digest(b"data")),"mode":384}]}],
            "file_count":1,"total_bytes":4
        }))
        .unwrap()
    }

    fn sign(raw: &[u8], seed: u8) -> SignatureEnvelope {
        let key = k256::ecdsa::SigningKey::from_bytes((&[seed; 32]).into()).unwrap();
        let (signature, recovery) = key.sign_prehash_recoverable(&signing_digest(raw)).unwrap();
        let mut bytes = [0; 65];
        bytes[..64].copy_from_slice(&signature.to_bytes());
        bytes[64] = recovery.to_byte();
        SignatureEnvelope::from_signature(raw, bytes).unwrap()
    }

    fn archive(root: &Path, raw: &[u8], payload: &[u8]) -> ValidationInputs {
        let path = root.join("snapshot.tar");
        write_archive(
            fs::File::create(&path).unwrap(),
            raw,
            &sign(raw, 7),
            |tar| {
                for (name, bytes, directory, mode) in [
                    ("payload/native", &[][..], true, 0o700),
                    ("payload/native/data", payload, false, 0o600),
                ] {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(bytes.len() as u64);
                    header.set_mode(mode);
                    header.set_entry_type(if directory {
                        tar::EntryType::Directory
                    } else {
                        tar::EntryType::Regular
                    });
                    header.set_cksum();
                    tar.append_data(&mut header, name, bytes)?;
                }
                Ok(())
            },
        )
        .unwrap();
        ValidationInputs {
            archive: Some(path),
            ..Default::default()
        }
    }

    fn detached(root: &Path, raw: &[u8]) -> ValidationInputs {
        let manifest = root.join("manifest.json");
        let signature = root.join("signature.json");
        fs::write(&manifest, raw).unwrap();
        fs::write(&signature, serde_json::to_vec(&sign(raw, 7)).unwrap()).unwrap();
        ValidationInputs {
            manifest: Some(manifest),
            signature: Some(signature),
            ..Default::default()
        }
    }

    fn audit(root: &Path, inputs: &ValidationInputs, files: bool) -> ArtifactAudit {
        let before = super::super::headers::fingerprint(root);
        let audit = audit_artifact(inputs, files);
        assert_eq!(super::super::headers::fingerprint(root), before);
        audit
    }

    fn incomplete(result: &eyre::Result<()>) {
        let error = result.as_ref().unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    }

    #[test]
    fn valid_signature_and_damaged_payload_have_independent_outcomes() {
        let root = tempfile::tempdir().unwrap();
        let inputs = archive(root.path(), &manifest(), b"BAD!");
        let result = audit(root.path(), &inputs, true);
        assert!(result.metadata.is_ok());
        assert!(result.provenance_result.is_ok());
        assert_eq!(result.provenance.signature_valid, Some(true));
        assert_eq!(result.provenance.expected_signer_match, None);
        assert!(result.archive_result.unwrap().is_err());
    }

    #[test]
    fn untrusted_signer_does_not_erase_crypto_validity_or_fail_valid_archive() {
        let root = tempfile::tempdir().unwrap();
        let raw = manifest();
        let mut inputs = archive(root.path(), &raw, b"data");
        inputs.expected_signer = Some(sign(&raw, 8).verify(&raw, None).unwrap());
        let result = audit(root.path(), &inputs, true);
        assert!(result.provenance_result.is_err());
        assert_eq!(result.provenance.signature_valid, Some(true));
        assert_eq!(result.provenance.expected_signer_match, Some(false));
        assert_eq!(result.provenance.signer, Some(sign(&raw, 7).public_key));
        assert!(result.archive_result.unwrap().is_ok());
    }

    #[test]
    fn provenance_only_never_traverses_malformed_payload_header() {
        let root = tempfile::tempdir().unwrap();
        let raw = manifest();
        let inputs = archive(root.path(), &raw, b"data");
        let path = inputs.archive.as_ref().unwrap();
        let mut bytes = fs::read(path).unwrap();
        let offset = bytes
            .windows(b"payload/native".len())
            .position(|w| w == b"payload/native")
            .unwrap();
        bytes[offset..].fill(0xff);
        fs::write(path, bytes).unwrap();
        let result = audit(root.path(), &inputs, false);
        assert!(result.metadata.is_ok());
        assert!(result.provenance_result.is_ok());
        assert!(result.archive_result.is_none());
        assert!(audit(root.path(), &inputs, true)
            .archive_result
            .unwrap()
            .is_err());
    }

    #[test]
    fn raw_whitespace_is_authenticated_and_simultaneous_sidecars_must_agree() {
        let root = tempfile::tempdir().unwrap();
        let raw = manifest();
        let mut inputs = archive(root.path(), &raw, b"data");
        let sides = detached(root.path(), &raw);
        inputs.manifest = sides.manifest;
        inputs.signature = sides.signature;
        let result = audit(root.path(), &inputs, true);
        assert!(result.metadata.is_ok());
        assert!(result.provenance_result.is_ok());
        assert!(result.archive_result.unwrap().is_ok());
        let normalized =
            serde_json::to_vec(&serde_json::from_slice::<serde_json::Value>(&raw).unwrap())
                .unwrap();
        assert_ne!(normalized, raw);
        fs::write(inputs.manifest.as_ref().unwrap(), normalized).unwrap();
        let result = audit(root.path(), &inputs, false);
        assert!(result.metadata.is_err());
        let error = result.provenance_result.unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none());
    }

    #[test]
    fn supplied_signature_cannot_silently_replace_the_archive_signer() {
        let root = tempfile::tempdir().unwrap();
        let raw = manifest();
        let mut inputs = archive(root.path(), &raw, b"data");
        let signature = root.path().join("side-signature.json");
        fs::write(&signature, serde_json::to_vec(&sign(&raw, 8)).unwrap()).unwrap();
        inputs.signature = Some(signature);
        let result = audit(root.path(), &inputs, false);
        assert!(result.provenance_result.is_err());
        assert_eq!(result.provenance.signature_valid, Some(true));
        assert_eq!(result.provenance.signer, Some(sign(&raw, 7).public_key));
    }

    #[test]
    fn signed_malformed_schema_does_not_erase_valid_crypto_evidence() {
        let root = tempfile::tempdir().unwrap();
        let raw = br#"{"version":987}"#;
        let inputs = archive(root.path(), raw, b"data");
        let result = audit(root.path(), &inputs, true);
        assert!(result.metadata.is_err());
        assert!(result.provenance_result.is_ok());
        assert_eq!(result.provenance.signature_valid, Some(true));
        assert!(result.archive_result.unwrap().is_err());
    }

    #[test]
    fn metadata_caps_reject_oversize_sidecars_and_archive_headers() {
        let root = tempfile::tempdir().unwrap();
        for (name, cap) in [
            ("manifest.json", 256_u64 * 1024 * 1024),
            ("signature.json", 64_u64 * 1024),
        ] {
            let inputs = detached(root.path(), &manifest());
            fs::File::create(root.path().join(name))
                .unwrap()
                .set_len(cap + 1)
                .unwrap();
            let result = audit_artifact(&inputs, false);
            assert!(result.provenance_result.is_err());
            let path = root.path().join("oversize.tar");
            let mut file = fs::File::create(&path).unwrap();
            if name == "signature.json" {
                let raw = manifest();
                let mut header = tar::Header::new_gnu();
                header.set_path("manifest.json").unwrap();
                header.set_size(raw.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                file.write_all(header.as_bytes()).unwrap();
                file.write_all(&raw).unwrap();
                file.write_all(&vec![0; (512 - raw.len() % 512) % 512])
                    .unwrap();
            }
            let mut header = tar::Header::new_gnu();
            header.set_path(name).unwrap();
            header.set_size(cap + 1);
            header.set_mode(0o644);
            header.set_cksum();
            file.write_all(header.as_bytes()).unwrap();
            drop(file);
            let result = audit_artifact(
                &ValidationInputs {
                    archive: Some(path),
                    ..Default::default()
                },
                false,
            );
            let error = result.provenance_result.unwrap_err();
            assert!(
                error.downcast_ref::<Incomplete>().is_none(),
                "oversize is malformed: {error:#}"
            );
        }
    }

    #[test]
    fn missing_metadata_is_incomplete_and_existing_manifest_is_still_parseable() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(ValidationInputs::default().checks, "all");
        let result = audit(root.path(), &ValidationInputs::default(), true);
        incomplete(&result.provenance_result);
        assert!(result
            .metadata
            .unwrap_err()
            .downcast_ref::<Incomplete>()
            .is_some());
        assert!(result.archive_result.is_none());
        let inputs = detached(root.path(), &manifest());
        fs::remove_file(inputs.signature.as_ref().unwrap()).unwrap();
        let result = audit(root.path(), &inputs, false);
        assert!(result.metadata.is_ok());
        incomplete(&result.provenance_result);
        assert_eq!(result.provenance.signature_valid, None);
        let inputs = ValidationInputs {
            archive: Some(root.path().join("absent.tar")),
            ..Default::default()
        };
        let result = audit(root.path(), &inputs, true);
        incomplete(&result.provenance_result);
        incomplete(result.archive_result.as_ref().unwrap());
        assert_eq!(result.provenance.signature_valid, None);

        let path = root.path().join("manifest-only.tar");
        let mut archive = tar::Builder::new(fs::File::create(&path).unwrap());
        let raw = manifest();
        let mut header = tar::Header::new_gnu();
        header.set_size(raw.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "manifest.json", raw.as_slice())
            .unwrap();
        archive.finish().unwrap();
        drop(archive);
        let result = audit(
            root.path(),
            &ValidationInputs {
                archive: Some(path),
                ..Default::default()
            },
            true,
        );
        assert!(result.metadata.is_ok());
        incomplete(&result.provenance_result);
        incomplete(result.archive_result.as_ref().unwrap());
    }

    #[test]
    fn detached_bad_signature_fails_without_claiming_an_authenticated_signer() {
        let root = tempfile::tempdir().unwrap();
        let inputs = detached(root.path(), &manifest());
        let wrong = sign(b"different raw bytes", 7);
        fs::write(
            inputs.signature.as_ref().unwrap(),
            serde_json::to_vec(&wrong).unwrap(),
        )
        .unwrap();
        let result = audit(root.path(), &inputs, false);
        assert!(result.metadata.is_ok());
        assert!(result.provenance_result.is_err());
        assert_eq!(result.provenance.signature_valid, Some(false));
        assert_eq!(result.provenance.signer, None);
        assert_eq!(result.provenance.expected_signer_match, None);
    }
}
