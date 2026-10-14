use super::RecordSorter;
use crate::persistence::{
    audit::{CeAuditError, CeAuditLimits, CeAuditWork},
    TreeKey,
};
use alloy_primitives::B256;
use std::{fs, path::PathBuf};

fn work(records_per_run: usize, merge_fan_in: usize) -> (tempfile::TempDir, CeAuditWork) {
    let parent = tempfile::tempdir().unwrap();
    let work = CeAuditWork::create(
        parent.path().join("sort"),
        CeAuditLimits {
            records_per_run,
            merge_fan_in,
        },
    )
    .unwrap();
    (parent, work)
}

fn only_run(work: &CeAuditWork) -> PathBuf {
    let child = fs::read_dir(&work.root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut files = fs::read_dir(child).unwrap();
    let path = files.next().unwrap().unwrap().path();
    assert!(files.next().is_none());
    path
}

#[test]
fn sorter_boundaries_multipass_and_reverse_input_have_identical_output() {
    for count in [0_u16, 1, 2, 3, 4, 5, 17, 33] {
        for reverse in [false, true] {
            let (_parent, work) = work(2, 2);
            let mut sorter = RecordSorter::<3>::new(&work, 2).unwrap();
            let expected = (0..count)
                .map(|value| {
                    let [a, b] = value.to_be_bytes();
                    [a, b, 99]
                })
                .collect::<Vec<_>>();
            let mut input = expected.clone();
            if reverse {
                input.reverse();
            }
            for record in input {
                sorter.push(record).unwrap();
            }
            assert_eq!(sorter.peak_buffered_records(), usize::from(count).min(2));
            let mut records = sorter.finish().unwrap();
            let actual = records.by_ref().collect::<Result<Vec<_>, _>>().unwrap();
            assert_eq!(actual, expected);
            assert!(records.next().is_none());
            assert!(records.next().is_none());
        }
    }
}

#[test]
fn sorter_rejects_duplicate_identity_with_different_payload_across_runs() {
    let (_parent, work) = work(2, 2);
    let mut sorter = RecordSorter::<2>::new(&work, 1).unwrap();
    for record in [[1, 10], [3, 30], [2, 20], [1, 11]] {
        sorter.push(record).unwrap();
    }
    let result = sorter
        .finish()
        .and_then(|records| records.collect::<Result<Vec<_>, _>>());
    assert!(result.is_err(), "duplicate prefix survived separate runs");
}

#[test]
fn sorter_rejects_duplicate_identity_within_one_run() {
    let (_parent, work) = work(3, 2);
    let mut sorter = RecordSorter::<2>::new(&work, 1).unwrap();
    sorter.push([1, 10]).unwrap();
    sorter.push([1, 11]).unwrap();
    let result = sorter
        .finish()
        .and_then(|records| records.collect::<Result<Vec<_>, _>>());
    assert!(result.is_err());
}

#[test]
fn sorter_uses_reversed_native_tree_key_bytes_in_body_tuples() {
    let (_parent, work) = work(1, 2);
    let mut low_lex = [0_u8; 32];
    low_lex[31] = 1;
    let mut high_lex = [0_u8; 32];
    high_lex[0] = 1;
    assert!(low_lex < high_lex);
    let low = TreeKey::try_from(B256::from(low_lex)).unwrap();
    let high = TreeKey::try_from(B256::from(high_lex)).unwrap();
    assert!(high < low);
    let tuple = |mut key: [u8; 32], payload: u8| {
        let mut record = [0; 101];
        record[0] = 1; // CollectionShard namespace tag.
        key.reverse();
        record[37..69].copy_from_slice(&key);
        record[100] = payload;
        record
    };
    let first = tuple(low.encode(), 1);
    let second = tuple(high.encode(), 2);
    let mut sorter = RecordSorter::<101>::new(&work, 69).unwrap();
    sorter.push(first).unwrap();
    sorter.push(second).unwrap();
    assert_eq!(
        sorter
            .finish()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        vec![second, first]
    );
}

#[test]
fn sorter_instances_share_work_without_overwriting_each_other() {
    let (parent, work) = work(1, 2);
    let sentinel = parent.path().join("protected");
    fs::write(&sentinel, b"keep").unwrap();
    let mut first = RecordSorter::<2>::new(&work, 1).unwrap();
    let mut second = RecordSorter::<2>::new(&work, 1).unwrap();
    first.push([1, 2]).unwrap();
    second.push([3, 4]).unwrap();
    let first = first
        .finish()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let second = second
        .finish()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(first, vec![[1, 2]]);
    assert_eq!(second, vec![[3, 4]]);
    drop(work);
    assert_eq!(fs::read(sentinel).unwrap(), b"keep");
}

#[test]
fn sorter_rejects_invalid_limits_before_creating_files_or_allocating() {
    let (_parent, mut work) = work(2, 2);
    assert!(RecordSorter::<0>::new(&work, 0).is_err());
    assert!(RecordSorter::<2>::new(&work, 0).is_err());
    assert!(RecordSorter::<2>::new(&work, 3).is_err());
    for (records_per_run, merge_fan_in) in [(0, 2), (2, 1), (usize::MAX, 2), (2, usize::MAX)] {
        work.limits = CeAuditLimits {
            records_per_run,
            merge_fan_in,
        };
        assert!(RecordSorter::<2>::new(&work, 1).is_err());
    }
    assert!(fs::read_dir(&work.root).unwrap().next().is_none());
}

#[test]
fn sorter_rejects_corrupt_run_metadata_lengths_and_identity_order() {
    // Header: magic/version[8], record width[8], key length[8], count[8].
    for corruption in 0..9 {
        let (_parent, work) = work(2, 2);
        let mut sorter = RecordSorter::<2>::new(&work, 1).unwrap();
        sorter.push([1, 10]).unwrap();
        sorter.push([2, 20]).unwrap();
        let path = only_run(&work);
        let mut bytes = fs::read(&path).unwrap();
        match corruption {
            0 => bytes[0] ^= 0xff,
            1 => bytes[8..16].copy_from_slice(&3_u64.to_be_bytes()),
            2 => bytes[16..24].copy_from_slice(&2_u64.to_be_bytes()),
            3 => bytes[24..32].copy_from_slice(&3_u64.to_be_bytes()),
            4 => bytes[24..32].copy_from_slice(&u64::MAX.to_be_bytes()),
            5 => {
                bytes.pop();
            }
            6 => bytes.push(0),
            7 => {
                bytes[32] = 2;
                bytes[34] = 1;
            }
            8 => bytes[34] = 1,
            _ => unreachable!(),
        }
        fs::write(path, bytes).unwrap();
        let result = sorter
            .finish()
            .and_then(|records| records.collect::<Result<Vec<_>, _>>());
        assert!(result.is_err(), "accepted corruption {corruption}");
    }
}

#[test]
fn sorter_iterator_emits_one_terminal_error_then_fuses() {
    let (_parent, work) = work(2, 2);
    let mut sorter = RecordSorter::<2>::new(&work, 1).unwrap();
    sorter.push([1, 10]).unwrap();
    sorter.push([2, 20]).unwrap();
    let path = only_run(&work);
    let mut records = sorter.finish().unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(32)
        .unwrap();
    assert!(matches!(records.next(), Some(Err(CeAuditError::Io(_)))));
    assert!(records.next().is_none());
    assert!(records.next().is_none());
}
