#[test]
fn full_canonical_composition_checks_payout_after_independent_inventory() {
    use crate::snapshot::validation::{ocomp::verify_canonical_obligations, Incomplete};
    let leaves = leaves();
    let root = contributor_list_root(257, leaves.iter().map(encode_contributor_leaf)).unwrap();
    let active = generation(root);
    for version in [1, 2] {
        for deleted in [false, true] {
            super::with_canonical_frontiers(
                version,
                |layout| {
                    write_payout(&layout.ocomp_root, active.job_id, &leaves);
                    if deleted {
                        fs::remove_dir_all(job_root(&layout.ocomp_root, active.job_id)).unwrap();
                    }
                },
                |_| native_owner(&active, root),
                |state, source, layout, scratch| {
                    let result =
                        verify_canonical_obligations(state, source, layout, scratch, None, None);
                    if deleted {
                        let error = result
                            .err()
                            .expect("whole missing certified payout job cannot pass");
                        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
                        assert!(
                            error.to_string().contains("missing payout file"),
                            "{error:#}"
                        );
                    } else {
                        let audit = result.unwrap();
                        assert_eq!(audit.bounds.unpaid_days, 1);
                        assert_eq!(audit.payout_days, 1);
                        assert_eq!(audit.bounds.active_intents, 0);
                    }
                },
            );
        }
    }
}

use super::super::headers::fingerprint;
use super::{payout_owner, with_owner_storage, CanonicalInventory, Incomplete, DAY};
use alloy_primitives::{Address, B256, U256};
use outbe_intex::{
    payout::{contributor_list_root, encode_contributor_leaf, ContributorLeafData},
    schema::IntexContract,
};
use outbe_ocomp::payout_artifact::CONTRIBUTOR_PAYOUT_ARTIFACT_FILE;
use outbe_ocomp_protocol::{
    profile::poc_schema_limits, result::ExactCountsV1, state::ActiveGenerationV1,
};
use outbe_primitives::{
    addresses::METADOSIS_ADDRESS,
    error::{PrecompileError, Result},
    storage::{
        hashmap::HashMapStorageProvider,
        readonly::{ReadOnlyStorageProvider, StorageReader},
        types::StorageBytes,
        StorageHandle,
    },
};
use std::{
    cell::Cell,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    rc::Rc,
};

fn leaves() -> Vec<ContributorLeafData> {
    (0..257_u32)
        .map(|index| {
            let mut owner = [0; 20];
            owner[16..].copy_from_slice(&(index + 1).to_be_bytes());
            ContributorLeafData {
                owner: Address::from(owner),
                source_tribute_id: (U256::from(DAY.value()) << 224) | U256::from(index + 1),
                nominal: U256::from(if index == 0 { 744 } else { 1 }),
            }
        })
        .collect()
}

fn generation(root: B256) -> ActiveGenerationV1 {
    ActiveGenerationV1 {
        job_id: B256::repeat_byte(0x71),
        program_semantics_hash: B256::repeat_byte(2),
        nod_root: B256::repeat_byte(3),
        bucket_root: B256::repeat_byte(4),
        contributor_root: root,
        output_manifest_root: B256::repeat_byte(6),
        exact_counts: ExactCountsV1 {
            tribute_count: 257,
            nod_count: 257,
            bucket_count: 1,
            contributor_count: 257,
            semantic_event_count: 3,
        },
        result_evidence_hash: B256::repeat_byte(7),
        availability_certificate_hash: None,
    }
}

fn native_owner(active: &ActiveGenerationV1, root: B256) -> HashMapStorageProvider {
    struct AbsentGenerationReader(Rc<Cell<Option<U256>>>);
    impl StorageReader for AbsentGenerationReader {
        fn read_storage(&self, address: Address, key: B256) -> Result<U256> {
            assert_eq!(address, METADOSIS_ADDRESS);
            assert!(self.0.replace(Some(U256::from_be_bytes(key.0))).is_none());
            Ok(U256::ZERO)
        }
    }
    // Observe the public getter's native length slot, as canonical_state tests do.
    let observed = Rc::new(Cell::new(None));
    let mut reader = ReadOnlyStorageProvider::new(AbsentGenerationReader(observed.clone()));
    let absent =
        outbe_metadosis::api::get_active_lysis_generation(StorageHandle::new(&mut reader), DAY);
    assert!(
        matches!(absent,Err(PrecompileError::Revert(message)) if message=="ActiveGenerationV1 not found")
    );
    let mut owner = payout_owner(true, true);
    StorageHandle::enter(&mut owner, |storage| {
        IntexContract::new(storage.clone())
            .ocomp_contributor_root
            .write(&DAY, root)
            .unwrap();
        StorageBytes::new(observed.get().unwrap(), METADOSIS_ADDRESS, storage)
            .write(&active.encode_canonical(&poc_schema_limits()).unwrap())
            .unwrap();
    });
    owner
}

fn job_root(root: &Path, job: B256) -> PathBuf {
    root.join("supervisor-v1")
        .join("jobs")
        .join(format!("{job:x}"))
}

fn write_payout(root: &Path, job: B256, leaves: &[ContributorLeafData]) -> PathBuf {
    let directory = job_root(root, job);
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join(CONTRIBUTOR_PAYOUT_ARTIFACT_FILE);
    let bytes = leaves
        .iter()
        .flat_map(encode_contributor_leaf)
        .collect::<Vec<_>>();
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn unpaid_old_day_uses_exact_canonical_job_without_intermediate_catalogs() {
    let leaves = leaves();
    let root = contributor_list_root(257, leaves.iter().map(encode_contributor_leaf)).unwrap();
    let active = generation(root);
    for version in [1, 2] {
        with_owner_storage(version, native_owner(&active, root), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            assert_eq!(inventory.bounds.unpaid_days, 1);
            let public = tempfile::tempdir().unwrap();
            let path = write_payout(public.path(), active.job_id, &leaves);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
            assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
            let before = fingerprint(public.path());
            assert_eq!(inventory.verify_payout_files(public.path()).unwrap(), 1);
            assert_eq!(fingerprint(public.path()), before);
            // Existing with_owner_storage fingerprints canonical source bytes/modes too.
        });
    }
}

#[test]
fn absent_entire_job_or_file_and_wrong_job_substitute_are_incomplete() {
    let leaves = leaves();
    let root = contributor_list_root(257, leaves.iter().map(encode_contributor_leaf)).unwrap();
    let active = generation(root);
    with_owner_storage(2, native_owner(&active, root), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        let public = tempfile::tempdir().unwrap();
        let missing = public.path().join("absent-ocomp");
        let before = fingerprint(public.path());
        let error = inventory.verify_payout_files(&missing).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        assert!(!missing.exists());
        assert_eq!(fingerprint(public.path()), before);
        for stage in ["no-job", "wrong-job", "no-file"] {
            if stage == "wrong-job" {
                write_payout(public.path(), B256::repeat_byte(0x72), &leaves);
            } else if stage == "no-file" {
                fs::create_dir_all(job_root(public.path(), active.job_id)).unwrap();
            }
            let before = fingerprint(public.path());
            let error = inventory.verify_payout_files(public.path()).unwrap_err();
            assert!(
                error.downcast_ref::<Incomplete>().is_some(),
                "{stage}: {error:#}"
            );
            assert_eq!(fingerprint(public.path()), before);
        }
    });
}

#[test]
fn corrupt_canonical_payout_leaf_is_failed_and_not_missing_input() {
    let leaves = leaves();
    let root = contributor_list_root(257, leaves.iter().map(encode_contributor_leaf)).unwrap();
    let active = generation(root);
    with_owner_storage(2, native_owner(&active, root), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        let public = tempfile::tempdir().unwrap();
        let path = write_payout(public.path(), active.job_id, &leaves);
        let mut bytes = fs::read(&path).unwrap();
        bytes[83] ^= 1;
        fs::write(path, bytes).unwrap();
        let before = fingerprint(public.path());
        let error = inventory.verify_payout_files(public.path()).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        assert_eq!(fingerprint(public.path()), before);
    });
}

#[test]
fn active_generation_requires_job_and_matching_certified_contributors() {
    let leaves = leaves();
    let root = contributor_list_root(257, leaves.iter().map(encode_contributor_leaf)).unwrap();
    for damage in ["root", "count", "zero-job"] {
        let mut active = generation(root);
        match damage {
            "count" => active.exact_counts.contributor_count = 256,
            "zero-job" => active.job_id = B256::ZERO,
            _ => active.contributor_root = B256::repeat_byte(0x99),
        }
        with_owner_storage(2, native_owner(&active, root), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let public = tempfile::tempdir().unwrap();
            write_payout(public.path(), active.job_id, &leaves);
            let before = fingerprint(public.path());
            let error = inventory.verify_payout_files(public.path()).unwrap_err();
            assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
            assert_eq!(fingerprint(public.path()), before);
        });
    }
}

#[test]
fn fully_paid_and_no_round_days_require_no_files_or_active_generation() {
    for (unpaid, round) in [(false, true), (true, false)] {
        with_owner_storage(2, payout_owner(unpaid, round), |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            assert_eq!(inventory.bounds.unpaid_days, 0);
            let public = tempfile::tempdir().unwrap();
            let missing = public.path().join("absent-ocomp");
            let before = fingerprint(public.path());
            assert_eq!(inventory.verify_payout_files(&missing).unwrap(), 0);
            assert!(!missing.exists());
            assert_eq!(fingerprint(public.path()), before);
        });
    }
}
