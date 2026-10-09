// OCOMP-TEST-ID: OCM-SIG-001

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

use alloy_primitives::B256;
use outbe_metadosis::config::poc_schema_limits;
use outbe_ocomp_protocol::{
    activation::SignOncePurpose,
    vote::{ResultVotePrefixV1, ResultVoteSigningSubjectV1, VoteSigningDomain},
};

use outbe_ocomp::sign_once::{
    PersistenceBoundary, SignOnceDurability, SignOnceError, SignOnceStore, SignOnceSubjectV1,
};

struct FailAfterBoundary {
    boundary: PersistenceBoundary,
    fired: AtomicBool,
}

struct NoSpaceDurability;

impl SignOnceDurability for NoSpaceDurability {
    fn sync_file(&self, _file: &File) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(28))
    }

    fn sync_directory(&self, directory: &File) -> io::Result<()> {
        directory.sync_all()
    }

    fn reached(&self, _boundary: PersistenceBoundary) -> io::Result<()> {
        Ok(())
    }
}

impl SignOnceDurability for FailAfterBoundary {
    fn sync_file(&self, file: &File) -> io::Result<()> {
        file.sync_all()
    }

    fn sync_directory(&self, directory: &File) -> io::Result<()> {
        directory.sync_all()
    }

    fn reached(&self, boundary: PersistenceBoundary) -> io::Result<()> {
        if boundary == self.boundary && !self.fired.swap(true, Ordering::SeqCst) {
            return Err(io::Error::other(format!(
                "injected failure after {boundary:?}"
            )));
        }
        Ok(())
    }
}

fn subject(result_digest: B256) -> SignOnceSubjectV1 {
    SignOnceSubjectV1 {
        domain: VoteSigningDomain {
            chain_id: 42,
            genesis_hash: B256::repeat_byte(0x10),
            fork_id: B256::repeat_byte(0x20),
        },
        prefix: ResultVotePrefixV1 {
            job_id: B256::repeat_byte(0x11),
            attempt: 0,
            protocol_bundle_hash: B256::repeat_byte(0x22),
            result_validator_set_epoch: 7,
            result_committee_set_hash: B256::repeat_byte(0x33),
            result_ocomp_binding_hash: B256::repeat_byte(0x34),
            ocomp_key_hash: B256::repeat_byte(0x35),
            key_epoch: 1,
        },
        result_digest,
    }
}

fn expected_signing_digest(subject: SignOnceSubjectV1) -> B256 {
    ResultVoteSigningSubjectV1 {
        chain_id: subject.domain.chain_id,
        genesis_hash: subject.domain.genesis_hash,
        fork_id: subject.domain.fork_id,
        protocol_bundle_hash: subject.prefix.protocol_bundle_hash,
        job_id: subject.prefix.job_id,
        attempt: subject.prefix.attempt,
        result_validator_set_epoch: subject.prefix.result_validator_set_epoch,
        result_committee_set_hash: subject.prefix.result_committee_set_hash,
        result_ocomp_binding_hash: subject.prefix.result_ocomp_binding_hash,
        ocomp_key_hash: subject.prefix.ocomp_key_hash,
        key_epoch: subject.prefix.key_epoch,
        purpose: SignOncePurpose::ResultSignature as u8,
        result_digest: subject.result_digest,
    }
    .signing_digest()
    .expect("fixture result-vote signing digest")
}

#[test]
fn ocm_sig_001_exact_replay_and_equivocation_survive_restart() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("sign-once");
    let signed = AtomicUsize::new(0);
    let first_subject = subject(B256::repeat_byte(0x44));
    let owner_uid = std::fs::metadata(temporary.path())
        .expect("temporary directory metadata")
        .uid();

    let store =
        SignOnceStore::open(root.clone(), owner_uid, poc_schema_limits()).expect("open store");
    let first = store
        .record_or_replay(first_subject, |digest| {
            assert_eq!(digest, expected_signing_digest(first_subject));
            signed.fetch_add(1, Ordering::SeqCst);
            Ok([0x55; 64])
        })
        .expect("record first signature");
    assert_eq!(first.result_digest, first_subject.result_digest);
    assert_eq!(first.signature_rs, [0x55; 64]);

    let replay = store
        .record_or_replay(first_subject, |_| {
            panic!("an exact replay must return the durable signature without signing again")
        })
        .expect("replay exact signature");
    assert_eq!(replay, first);
    assert_eq!(signed.load(Ordering::SeqCst), 1);

    drop(store);
    let reopened =
        SignOnceStore::open(root.clone(), owner_uid, poc_schema_limits()).expect("reopen store");
    let replay_after_restart = reopened
        .record_or_replay(first_subject, |_| {
            panic!("restart replay must not sign again")
        })
        .expect("replay after restart");
    assert_eq!(replay_after_restart, first);

    let conflicting = subject(B256::repeat_byte(0x66));
    assert!(matches!(
        reopened.record_or_replay(conflicting, |_| {
            panic!("an equivocation attempt must be refused before signing")
        }),
        Err(SignOnceError::Equivocation { .. })
    ));

    drop(reopened);
    let reopened = SignOnceStore::open(root, owner_uid, poc_schema_limits())
        .expect("reopen after equivocation");
    assert!(matches!(
        reopened.record_or_replay(conflicting, |_| {
            panic!("a restart must not erase equivocation protection")
        }),
        Err(SignOnceError::Equivocation { .. })
    ));
}

#[test]
fn ocm_sig_001_every_persistence_boundary_fails_closed_and_recovers_only_when_published() {
    for boundary in [
        PersistenceBoundary::Created,
        PersistenceBoundary::Written,
        PersistenceBoundary::FileSynced,
        PersistenceBoundary::Linked,
        PersistenceBoundary::DirectorySynced,
        PersistenceBoundary::PendingRemoved,
        PersistenceBoundary::CleanupDirectorySynced,
    ] {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("sign-once");
        let owner_uid = std::fs::metadata(temporary.path())
            .expect("temporary directory metadata")
            .uid();
        let first_subject = subject(B256::repeat_byte(0x77));
        let store = SignOnceStore::open_with_durability(
            root.clone(),
            owner_uid,
            poc_schema_limits(),
            Arc::new(FailAfterBoundary {
                boundary,
                fired: AtomicBool::new(false),
            }),
        )
        .expect("open faulted store");

        assert!(matches!(
            store.record_or_replay(first_subject, |_| Ok([0x88; 64])),
            Err(SignOnceError::Io { .. })
        ));
        assert!(matches!(
            store.record_or_replay(first_subject, |_| {
                panic!("an uncertain in-process store must not sign again")
            }),
            Err(SignOnceError::Disabled(_))
        ));
        drop(store);

        if matches!(
            boundary,
            PersistenceBoundary::Created
                | PersistenceBoundary::Written
                | PersistenceBoundary::FileSynced
        ) {
            assert!(matches!(
                SignOnceStore::open(root, owner_uid, poc_schema_limits()),
                Err(SignOnceError::UncertainState { .. })
            ));
        } else {
            let reopened = SignOnceStore::open(root, owner_uid, poc_schema_limits())
                .expect("published record must reconcile");
            let recovered = reopened
                .record_or_replay(first_subject, |_| {
                    panic!("a reconciled published record must not sign again")
                })
                .expect("replay reconciled record");
            assert_eq!(recovered.signature_rs, [0x88; 64]);
        }
    }
}

#[test]
fn ocm_sig_001_durable_reservation_precedes_signing_and_survives_restart() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("sign-once");
    let owner_uid = fs::metadata(temporary.path())
        .expect("temporary directory metadata")
        .uid();
    let first_subject = subject(B256::repeat_byte(0x79));
    let store = SignOnceStore::open_with_durability(
        root.clone(),
        owner_uid,
        poc_schema_limits(),
        Arc::new(FailAfterBoundary {
            boundary: PersistenceBoundary::Reserved,
            fired: AtomicBool::new(false),
        }),
    )
    .expect("open faulted store");

    assert!(matches!(
        store.record_or_replay(first_subject, |_| {
            panic!("the key must not be called before the reservation is durable")
        }),
        Err(SignOnceError::Io { .. })
    ));
    drop(store);

    let reopened = SignOnceStore::open(root, owner_uid, poc_schema_limits())
        .expect("reopen durable reservation");
    let conflicting = subject(B256::repeat_byte(0x7a));
    assert!(matches!(
        reopened.record_or_replay(conflicting, |_| {
            panic!("a conflicting digest must be rejected from the reservation")
        }),
        Err(SignOnceError::Equivocation { .. })
    ));
    let recovered = reopened
        .record_or_replay(first_subject, |_| Ok([0x7b; 64]))
        .expect("complete the reserved digest after restart");
    assert_eq!(recovered.signature_rs, [0x7b; 64]);
}

#[test]
fn ocm_sig_001_lost_response_reconnect_replays_the_durable_signature() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("sign-once");
    let owner_uid = fs::metadata(temporary.path())
        .expect("temporary directory metadata")
        .uid();
    let first_subject = subject(B256::repeat_byte(0x89));
    let store =
        SignOnceStore::open(root.clone(), owner_uid, poc_schema_limits()).expect("open store");
    store
        .record_or_replay(first_subject, |_| Ok([0x8a; 64]))
        .expect("durably publish signature before simulated response loss");
    drop(store);

    let reconnected =
        SignOnceStore::open(root, owner_uid, poc_schema_limits()).expect("reopen after disconnect");
    let replay = reconnected
        .record_or_replay(first_subject, |_| {
            panic!("reconnect must replay without another key operation")
        })
        .expect("replay durable signature");
    assert_eq!(replay.signature_rs, [0x8a; 64]);
}

#[test]
fn ocm_sig_001_full_corrupt_and_unsafe_storage_fail_closed() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let owner_uid = fs::metadata(temporary.path())
        .expect("temporary directory metadata")
        .uid();
    let full_root = temporary.path().join("full");
    let full = SignOnceStore::open_with_durability(
        full_root,
        owner_uid,
        poc_schema_limits(),
        Arc::new(NoSpaceDurability),
    )
    .expect("open capacity-fault store");
    let first_subject = subject(B256::repeat_byte(0x91));
    assert!(matches!(
        full.record_or_replay(first_subject, |_| Ok([0x92; 64])),
        Err(SignOnceError::Io { .. })
    ));
    assert!(matches!(
        full.record_or_replay(first_subject, |_| {
            panic!("a capacity-faulted store must disable signing")
        }),
        Err(SignOnceError::Disabled(_))
    ));

    let corrupt_root = temporary.path().join("corrupt");
    let store = SignOnceStore::open(corrupt_root.clone(), owner_uid, poc_schema_limits())
        .expect("open corruption fixture store");
    store
        .record_or_replay(first_subject, |_| Ok([0x93; 64]))
        .expect("install corruption fixture record");
    drop(store);
    let record_path = fs::read_dir(&corrupt_root)
        .expect("read sign-once directory")
        .next()
        .expect("one record")
        .expect("record entry")
        .path();
    let mut record = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&record_path)
        .expect("open record for corruption");
    record
        .write_all(b"not-a-canonical-sign-once-record")
        .expect("corrupt record");
    record.sync_all().expect("sync corrupted record");
    assert!(matches!(
        SignOnceStore::open(corrupt_root, owner_uid, poc_schema_limits()),
        Err(SignOnceError::CorruptRecord { .. })
    ));

    let unsafe_root = temporary.path().join("unsafe");
    fs::create_dir(&unsafe_root).expect("create unsafe root");
    fs::set_permissions(&unsafe_root, fs::Permissions::from_mode(0o755))
        .expect("set unsafe root mode");
    assert!(matches!(
        SignOnceStore::open(unsafe_root, owner_uid, poc_schema_limits()),
        Err(SignOnceError::UnsafeStore { .. })
    ));
}

// Characterization of the sign-once file format and of the replay guard. The
// expected values were computed independently of the store: OCB1 header
// (magic, object tag 0x001b, schema version 1, body length) and big-endian
// fields in SignOnceRecordV1 order. Keccak-256 over the registered hash-domain
// frames gives the slot id and the signing digest.

const PINNED_SLOT_HEX: &str = "7fe40aeaede6b482c0c81dc1eedb2b1e3fc98dabcb62b704060a6ba656c01cdc";
const PINNED_SIGNING_DIGEST_HEX: &str =
    "d023bf217b65b0feb9cab243c4423ae90423b563d44610b7df5f0edd86791ea9";
const PINNED_RECORD_PREFIX_HEX: &str = concat!(
    "4f434231001b00010000015d",
    "000000000000002a",
    "1010101010101010101010101010101010101010101010101010101010101010",
    "2020202020202020202020202020202020202020202020202020202020202020",
    "01",
    "1111111111111111111111111111111111111111111111111111111111111111",
    "00000000",
    "2222222222222222222222222222222222222222222222222222222222222222",
    "0000000000000007",
    "3333333333333333333333333333333333333333333333333333333333333333",
    "3434343434343434343434343434343434343434343434343434343434343434",
    "3535353535353535353535353535353535353535353535353535353535353535",
    "0000000000000001",
    "4444444444444444444444444444444444444444444444444444444444444444",
);

fn pinned_bytes(hex_text: &str) -> Vec<u8> {
    alloy_primitives::hex::decode(hex_text).expect("pinned hex")
}

fn pinned_record(signature_rs: [u8; 64]) -> Vec<u8> {
    let mut record = pinned_bytes(PINNED_RECORD_PREFIX_HEX);
    record.extend_from_slice(&signature_rs);
    assert_eq!(record.len(), 361, "12-byte OCB1 header and 349-byte body");
    record
}

fn sign_once_file_names(root: &std::path::Path) -> Vec<String> {
    let mut names = fs::read_dir(root)
        .expect("read sign-once directory")
        .map(|entry| {
            entry
                .expect("sign-once directory entry")
                .file_name()
                .into_string()
                .expect("UTF-8 sign-once file name")
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[derive(Clone, Copy)]
enum FieldMismatch {
    SameSlotEquivocation,
    NewSlot(&'static str),
    KeyEpochRejected,
}

type SubjectMutation = (&'static str, fn(&mut SignOnceSubjectV1), FieldMismatch);

fn field_mutations() -> [SubjectMutation; 12] {
    use FieldMismatch::{KeyEpochRejected, NewSlot, SameSlotEquivocation};
    [
        (
            "chain_id",
            |s| s.domain.chain_id = 43,
            NewSlot("04c9d14520f2e18e17163c8787f53a92f64cf5343a02b794b8b71e4f7a782e11"),
        ),
        (
            "genesis_hash",
            |s| s.domain.genesis_hash = B256::repeat_byte(0xa1),
            SameSlotEquivocation,
        ),
        (
            "fork_id",
            |s| s.domain.fork_id = B256::repeat_byte(0xa2),
            SameSlotEquivocation,
        ),
        (
            "job_id",
            |s| s.prefix.job_id = B256::repeat_byte(0xa3),
            NewSlot("64e1e4029e5ee24c0c140917c6e0b2f9f32f56aed0e2a1730d4abc176db14e51"),
        ),
        (
            "attempt",
            |s| s.prefix.attempt = 1,
            NewSlot("19fde6d2059d047e34ab6c6a87005d07b7dad719042f72a908a0e2a5894d9e7d"),
        ),
        (
            "protocol_bundle_hash",
            |s| s.prefix.protocol_bundle_hash = B256::repeat_byte(0xa4),
            SameSlotEquivocation,
        ),
        (
            "result_validator_set_epoch",
            |s| s.prefix.result_validator_set_epoch = 8,
            SameSlotEquivocation,
        ),
        (
            "result_committee_set_hash",
            |s| s.prefix.result_committee_set_hash = B256::repeat_byte(0xa5),
            SameSlotEquivocation,
        ),
        (
            "result_ocomp_binding_hash",
            |s| s.prefix.result_ocomp_binding_hash = B256::repeat_byte(0xa6),
            SameSlotEquivocation,
        ),
        (
            "ocomp_key_hash",
            |s| s.prefix.ocomp_key_hash = B256::repeat_byte(0xa7),
            SameSlotEquivocation,
        ),
        ("key_epoch", |s| s.prefix.key_epoch = 2, KeyEpochRejected),
        (
            "result_digest",
            |s| s.result_digest = B256::repeat_byte(0xa8),
            SameSlotEquivocation,
        ),
    ]
}

#[test]
fn ocm_sig_001_reservation_record_name_and_digest_bytes_are_pinned() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("sign-once");
    let owner_uid = fs::metadata(temporary.path())
        .expect("temporary directory metadata")
        .uid();
    let base = subject(B256::repeat_byte(0x44));
    let reservation_name = format!("{PINNED_SLOT_HEX}.reserved.ocb1");
    let record_name = format!("{PINNED_SLOT_HEX}.ocb1");
    let store =
        SignOnceStore::open(root.clone(), owner_uid, poc_schema_limits()).expect("open store");

    let record = store
        .record_or_replay(base, |digest| {
            assert_eq!(digest.as_slice(), pinned_bytes(PINNED_SIGNING_DIGEST_HEX));
            assert_eq!(digest, expected_signing_digest(base));
            assert_eq!(
                sign_once_file_names(&root),
                std::slice::from_ref(&reservation_name)
            );
            assert_eq!(
                fs::read(root.join(&reservation_name)).expect("read reservation"),
                pinned_record([0; 64]),
                "the durable reservation is the canonical record with a zero signature"
            );
            Ok([0x55; 64])
        })
        .expect("record pinned subject");

    assert_eq!(
        sign_once_file_names(&root),
        std::slice::from_ref(&record_name)
    );
    assert_eq!(
        fs::read(root.join(&record_name)).expect("read record"),
        pinned_record([0x55; 64])
    );
    assert_eq!(
        record
            .encode_canonical(&poc_schema_limits())
            .expect("encode returned record"),
        pinned_record([0x55; 64])
    );
    assert_eq!(
        record.slot_id().expect("slot id").as_slice(),
        pinned_bytes(PINNED_SLOT_HEX)
    );
    assert_eq!(
        record.signing_digest().expect("record signing digest"),
        expected_signing_digest(base)
    );
}

#[test]
fn ocm_sig_001_every_subject_field_is_guarded_against_a_durable_record() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("sign-once");
    let owner_uid = fs::metadata(temporary.path())
        .expect("temporary directory metadata")
        .uid();
    let base = subject(B256::repeat_byte(0x44));
    let store =
        SignOnceStore::open(root.clone(), owner_uid, poc_schema_limits()).expect("open store");
    let first = store
        .record_or_replay(base, |_| Ok([0x55; 64]))
        .expect("record base subject");
    let mut expected_names = vec![format!("{PINNED_SLOT_HEX}.ocb1")];

    for (field, mutate, expected) in field_mutations() {
        let mut changed = base;
        mutate(&mut changed);
        assert_ne!(changed, base, "{field} mutation must change the subject");
        let signed = AtomicUsize::new(0);
        let result = store.record_or_replay(changed, |digest| {
            assert_eq!(digest, expected_signing_digest(changed), "{field}");
            signed.fetch_add(1, Ordering::SeqCst);
            Ok([0x56; 64])
        });
        match expected {
            FieldMismatch::SameSlotEquivocation => {
                match result {
                    Err(SignOnceError::Equivocation {
                        job_id,
                        attempt,
                        recorded_digest,
                        requested_digest,
                    }) => {
                        assert_eq!(job_id, base.prefix.job_id, "{field}");
                        assert_eq!(attempt, base.prefix.attempt, "{field}");
                        assert_eq!(recorded_digest, base.result_digest, "{field}");
                        assert_eq!(requested_digest, changed.result_digest, "{field}");
                    }
                    other => panic!("{field}: expected equivocation, got {other:?}"),
                }
                assert_eq!(signed.load(Ordering::SeqCst), 0, "{field}");
            }
            FieldMismatch::NewSlot(slot_hex) => {
                let record = result.unwrap_or_else(|error| panic!("{field}: {error}"));
                assert_eq!(record.signature_rs, [0x56; 64], "{field}");
                assert_eq!(signed.load(Ordering::SeqCst), 1, "{field}");
                expected_names.push(format!("{slot_hex}.ocb1"));
            }
            FieldMismatch::KeyEpochRejected => {
                assert!(
                    matches!(result, Err(SignOnceError::Protocol(_))),
                    "{field}: expected a protocol rejection, got {result:?}"
                );
                assert_eq!(signed.load(Ordering::SeqCst), 0, "{field}");
            }
        }
        expected_names.sort();
        assert_eq!(sign_once_file_names(&root), expected_names, "{field}");
        let replay = store
            .record_or_replay(base, |_| {
                panic!("{field}: a refused or unrelated request must not disable exact replay")
            })
            .unwrap_or_else(|error| panic!("{field}: exact replay failed: {error}"));
        assert_eq!(replay, first, "{field}");
    }
    assert_eq!(
        fs::read(root.join(format!("{PINNED_SLOT_HEX}.ocb1"))).expect("read base record"),
        pinned_record([0x55; 64])
    );
}

#[test]
fn ocm_sig_001_every_same_slot_field_is_guarded_against_a_durable_reservation() {
    for (field, mutate, expected) in field_mutations() {
        if !matches!(expected, FieldMismatch::SameSlotEquivocation) {
            continue;
        }
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("sign-once");
        let owner_uid = fs::metadata(temporary.path())
            .expect("temporary directory metadata")
            .uid();
        let base = subject(B256::repeat_byte(0x44));
        let store = SignOnceStore::open_with_durability(
            root.clone(),
            owner_uid,
            poc_schema_limits(),
            Arc::new(FailAfterBoundary {
                boundary: PersistenceBoundary::Reserved,
                fired: AtomicBool::new(false),
            }),
        )
        .expect("open faulted store");
        assert!(matches!(
            store.record_or_replay(base, |_| {
                panic!("{field}: the key must not be called before the reservation is durable")
            }),
            Err(SignOnceError::Io { .. })
        ));
        drop(store);
        let reservation_name = format!("{PINNED_SLOT_HEX}.reserved.ocb1");
        assert_eq!(
            sign_once_file_names(&root),
            std::slice::from_ref(&reservation_name)
        );
        assert_eq!(
            fs::read(root.join(&reservation_name)).expect("read reservation"),
            pinned_record([0; 64])
        );

        let reopened = SignOnceStore::open(root.clone(), owner_uid, poc_schema_limits())
            .expect("reopen durable reservation");
        let mut changed = base;
        mutate(&mut changed);
        match reopened.record_or_replay(changed, |_| {
            panic!("{field}: a reservation mismatch must be refused before signing")
        }) {
            Err(SignOnceError::Equivocation {
                job_id,
                attempt,
                recorded_digest,
                requested_digest,
            }) => {
                assert_eq!(job_id, base.prefix.job_id, "{field}");
                assert_eq!(attempt, base.prefix.attempt, "{field}");
                assert_eq!(recorded_digest, base.result_digest, "{field}");
                assert_eq!(requested_digest, changed.result_digest, "{field}");
            }
            other => panic!("{field}: expected equivocation, got {other:?}"),
        }
        assert_eq!(sign_once_file_names(&root), [reservation_name], "{field}");
        let completed = reopened
            .record_or_replay(base, |_| Ok([0x57; 64]))
            .expect("complete the reserved subject");
        assert_eq!(completed.signature_rs, [0x57; 64], "{field}");
        assert_eq!(
            fs::read(root.join(format!("{PINNED_SLOT_HEX}.ocb1"))).expect("read record"),
            pinned_record([0x57; 64])
        );
    }
}
