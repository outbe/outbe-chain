use crate::snapshot::tests::headers::fingerprint;
use crate::snapshot::validation::{ocomp::verify_present_cas, Incomplete};
use outbe_ocomp::cas::{CasLimits, CasWriterRole, FilesystemCas};
use std::{
    fs,
    path::{Path, PathBuf},
};

const LIMITS: CasLimits = CasLimits {
    max_object_bytes: 1024,
    max_total_bytes: 4096,
};

fn publish(root: &Path, bytes: &[u8]) -> PathBuf {
    let cas = FilesystemCas::open(root.join("cas-v1"), CasWriterRole::Supervisor, LIMITS).unwrap();
    let object = cas.publish_bytes(bytes).unwrap();
    let hash = hex::encode(object.transport_digest);
    root.join("cas-v1/objects")
        .join(&hash[..2])
        .join(&hash[2..])
}

#[test]
fn absent_cas_stays_absent_and_unreferenced_native_objects_are_verified() {
    let root = tempfile::tempdir().unwrap();
    let absent = verify_present_cas(root.path(), LIMITS, None).unwrap();
    assert_eq!((absent.objects, absent.bytes), (0, 0));
    assert!(!root.path().join("cas-v1").exists());
    publish(root.path(), b"historical unreferenced bytes");
    publish(root.path(), b"another object");
    let before = fingerprint(root.path());
    let result = verify_present_cas(root.path(), LIMITS, None).unwrap();
    assert_eq!(result.objects, 2);
    assert_eq!(result.bytes, 43);
    assert_eq!(fingerprint(root.path()), before);
    assert!(!root.path().join("supervisor-v1").exists());
}

#[test]
fn changed_unreferenced_object_is_detected_without_job_or_catalog() {
    for replacement in [b"different bytes".as_slice(), b"short".as_slice()] {
        let root = tempfile::tempdir().unwrap();
        let path = publish(root.path(), b"unchanged bytes");
        fs::write(path, replacement).unwrap();
        let before = fingerprint(root.path());
        let error = verify_present_cas(root.path(), LIMITS, None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
        assert!(error.to_string().contains("digest mismatch"), "{error:#}");
        assert_eq!(fingerprint(root.path()), before);
    }
}

#[test]
fn object_count_and_total_byte_budgets_cannot_pass_a_successful_prefix() {
    let root = tempfile::tempdir().unwrap();
    publish(root.path(), &[1; 700]);
    publish(root.path(), &[2; 700]);
    for (limits, maximum) in [
        (LIMITS, Some(1)),
        (
            CasLimits {
                max_total_bytes: 1024,
                ..LIMITS
            },
            None,
        ),
    ] {
        let before = fingerprint(root.path());
        let error = verify_present_cas(root.path(), limits, maximum).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        assert_eq!(fingerprint(root.path()), before);
    }
}
