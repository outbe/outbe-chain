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
