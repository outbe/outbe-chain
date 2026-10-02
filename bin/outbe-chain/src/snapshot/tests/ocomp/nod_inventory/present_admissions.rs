mod reference_membership;

use super::*;
use crate::snapshot::validation::ocomp::verify_present_admissions;
use outbe_ocomp::admission_catalog::VerifiedAdmissionRecordV1;

struct Expected {
    manifest: InputManifestV1,
    lysis_limit: U256,
    evaluation_time: u64,
    topology: LysisPlanTopologyV1,
    records: Vec<VerifiedAdmissionRecordV1>,
}

fn admission_root(root: &Path, f: &Fixture) -> PathBuf {
    root.join("supervisor-v1/jobs")
        .join(hex::encode(f.job_id))
        .join("admissions")
}
fn record_path(root: &Path, f: &Fixture, ordinal: u32) -> PathBuf {
    admission_root(root, f).join(format!("{ordinal:010}.admission"))
}
fn cas_path(f: &Fixture, reference: &CasObjectRefV1) -> PathBuf {
    let digest = hex::encode(reference.transport_digest);
    f.cas_root
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..])
}

fn expected(root: &Path, f: &Fixture) -> Expected {
    let limits = poc_schema_limits();
    let cas = FilesystemCasReader::open(&f.cas_root, CAS_LIMITS).unwrap();
    let inputs = VerifiedInputChunkRefCatalog::reopen(
        root.join("exporter-v1/input-refs")
            .join(hex::encode(f.job_id)),
        &cas,
        limits,
        poc_input_list_limits(),
    )
    .unwrap();
    let admissions =
        AdmissionCatalogReader::open_existing(admission_root(root, f), &cas, limits).unwrap();
    let audit =
        LocalLysisPlanAuditV1::open_read_only(&admissions, &inputs, &cas, &f.bundle, &limits)
            .unwrap();
    let topology = LysisPlanTopologyV1::new(audit.plan().primary_work_unit_count).unwrap();
    Expected {
        manifest: audit.manifest().clone(),
        lysis_limit: audit.plan().lysis_limit_minor,
        evaluation_time: audit.plan().logical_evaluation_time,
        topology,
        records: (0..topology.total_unit_count())
            .map(|ordinal| admissions.read(ordinal).unwrap())
            .collect(),
    }
}

fn keep_only(root: &Path, f: &Fixture, expected: &Expected, ordinals: &[u32]) {
    for ordinal in 0..expected.topology.total_unit_count() {
        if !ordinals.contains(&ordinal) {
            fs::remove_file(record_path(root, f, ordinal)).unwrap();
        }
    }
}

fn run(
    root: &Path,
    expected: &Expected,
    maximum: Option<u64>,
    visitor: &mut impl FnMut(&CasObjectRefV1) -> eyre::Result<()>,
) -> eyre::Result<(u32, u32, u32)> {
    let before = fingerprint(root);
    let result = verify_present_admissions(
        root,
        &expected.manifest,
        expected.lysis_limit,
        expected.evaluation_time,
        CAS_LIMITS,
        maximum,
        visitor,
    )
    .map(|audit| (audit.expected, audit.present, audit.result_chunks));
    assert_eq!(fingerprint(root), before);
    result
}

fn sorted(mut refs: Vec<CasObjectRefV1>) -> Vec<CasObjectRefV1> {
    refs.sort_by_key(|reference| {
        (
            reference.transport_digest,
            reference.encoded_bytes,
            reference.expected_ocb1_kind,
        )
    });
    refs
}

fn references(records: &[VerifiedAdmissionRecordV1]) -> Vec<CasObjectRefV1> {
    records
        .iter()
        .flat_map(|record| {
            std::iter::once(record.artifact_ref.clone()).chain(
                record
                    .result_chunk
                    .as_ref()
                    .map(|chunk| chunk.output_manifest_entry.result_chunk_ref.clone()),
            )
        })
        .collect()
}

#[test]
fn complete_present_catalog_emits_exact_native_artifact_and_result_references() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let expected = expected(root.path(), &f);
    let total = expected.topology.total_unit_count();
    let mut visited = Vec::new();
    let counts = run(
        root.path(),
        &expected,
        Some(u64::from(total)),
        &mut |reference| {
            visited.push(reference.clone());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(counts, (total, total, 1));
    assert_eq!(sorted(visited), sorted(references(&expected.records)));
}

#[test]
fn independent_partial_tail_and_empty_catalog_are_valid_without_full_plan_cursor() {
    for keep_first in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let expected = expected(root.path(), &f);
        let first = expected
            .topology
            .plan_ordinal_of(PlannedUnitPositionV1::Primary {
                phase: UnitPhase::Enumerate,
                ordinal: 0,
            })
            .unwrap();
        let keep = if keep_first { vec![first] } else { vec![] };
        keep_only(root.path(), &f, &expected, &keep);
        let mut visited = Vec::new();
        let counts = run(
            root.path(),
            &expected,
            Some(keep.len() as u64),
            &mut |reference| {
                visited.push(reference.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            counts,
            (expected.topology.total_unit_count(), keep.len() as u32, 0)
        );
        let wanted = if keep_first {
            vec![expected.records[first as usize].artifact_ref.clone()]
        } else {
            vec![]
        };
        assert_eq!(visited, wanted);
    }
}

#[test]
fn absent_independent_earlier_work_does_not_hide_corrupt_later_present_artifact() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 257);
    let expected = expected(root.path(), &f);
    let later = expected
        .topology
        .plan_ordinal_of(PlannedUnitPositionV1::Primary {
            phase: UnitPhase::Enumerate,
            ordinal: 1,
        })
        .unwrap();
    assert!(later > 0);
    keep_only(root.path(), &f, &expected, &[later]);
    assert_eq!(
        run(root.path(), &expected, None, &mut |_| Ok(())).unwrap(),
        (expected.topology.total_unit_count(), 1, 0)
    );
    let path = cas_path(&f, &expected.records[later as usize].artifact_ref);
    let mut bytes = fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(path, bytes).unwrap();
    let error = run(root.path(), &expected, None, &mut |_| Ok(())).unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
}

#[test]
fn present_consumer_without_required_producer_admission_is_incomplete() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let expected = expected(root.path(), &f);
    let consumer = expected
        .topology
        .plan_ordinal_of(PlannedUnitPositionV1::Primary {
            phase: UnitPhase::FidelityMap,
            ordinal: 0,
        })
        .unwrap();
    keep_only(root.path(), &f, &expected, &[consumer]);
    let mut calls = 0;
    let error = run(root.path(), &expected, None, &mut |_| {
        calls += 1;
        Ok(())
    })
    .unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    assert_eq!(
        calls, 0,
        "unbound consumer must not emit a verified reference"
    );
}

#[test]
fn present_root_leaf_requires_its_exact_result_chunk_and_cas_corruption_fails() {
    for missing in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let expected = expected(root.path(), &f);
        let path = cas_path(&f, &f.result_chunk_refs[0]);
        if missing {
            fs::remove_file(path).unwrap();
        } else {
            let mut bytes = fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            fs::write(path, bytes).unwrap();
        }
        let error = run(root.path(), &expected, None, &mut |_| Ok(())).unwrap_err();
        assert_eq!(
            error.downcast_ref::<Incomplete>().is_some(),
            missing,
            "{error:#}"
        );
    }
}

#[test]
fn present_foreign_admission_and_noncanonical_extra_locators_are_failed() {
    for damage in ["foreign", "out_of_range", "malformed"] {
        let root = tempfile::tempdir().unwrap();
        let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let expected = expected(root.path(), &f);
        match damage {
            "foreign" => {
                let foreign = fixture(root.path(), 0x40, WorldwideDay::new(20_260_726), 10);
                fs::copy(
                    record_path(root.path(), &foreign, 0),
                    record_path(root.path(), &f, 0),
                )
                .unwrap();
            }
            "out_of_range" => {
                fs::copy(
                    record_path(root.path(), &f, 0),
                    record_path(root.path(), &f, expected.topology.total_unit_count()),
                )
                .unwrap();
            }
            "malformed" => {
                fs::write(
                    admission_root(root.path(), &f).join("1.admission"),
                    b"foreign entry",
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let error = run(root.path(), &expected, None, &mut |_| Ok(())).unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_none(),
            "{damage}: {error:#}"
        );
    }
}

#[test]
fn canonical_export_manifest_and_frozen_plan_scalar_mismatches_are_failed() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    for damage in ["manifest_checkpoint", "lysis_limit", "evaluation_time"] {
        let mut expected = expected(root.path(), &f);
        match damage {
            "manifest_checkpoint" => expected.manifest.checkpoint.finalized_block_number += 1,
            "lysis_limit" => expected.lysis_limit += U256::ONE,
            "evaluation_time" => expected.evaluation_time += 1,
            _ => unreachable!(),
        }
        let mut calls = 0;
        let error = run(root.path(), &expected, None, &mut |_| {
            calls += 1;
            Ok(())
        })
        .unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_none(),
            "{damage}: {error:#}"
        );
        assert_eq!(
            calls, 0,
            "authority must be checked before reference callbacks"
        );
    }
}

#[test]
fn budget_and_visitor_failures_are_incomplete_not_partial_success() {
    let root = tempfile::tempdir().unwrap();
    let f = fixture(root.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let expected = expected(root.path(), &f);
    for maximum in [0, u64::from(expected.topology.total_unit_count() - 1)] {
        let error = run(root.path(), &expected, Some(maximum), &mut |_| Ok(())).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    }
    let mut calls = 0;
    let error = run(root.path(), &expected, None, &mut |_| {
        calls += 1;
        Err(Incomplete("reference visitor resource bound".into()).into())
    })
    .unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    assert!(
        error
            .to_string()
            .contains("reference visitor resource bound"),
        "{error:#}"
    );
    assert_eq!(calls, 1);
}
