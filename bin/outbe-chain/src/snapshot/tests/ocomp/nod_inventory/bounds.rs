//! bounds regression scenarios.
use super::*;

#[test]
fn batch_budget_reports_incomplete_and_empty_fifo_needs_no_public_data() {
    let public = tempfile::tempdir().unwrap();
    let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let before = fingerprint(public.path());
    with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        let error = inventory
            .verify_nod_inputs(public.path(), CAS_LIMITS, 3, Some(1))
            .expect_err("second batch exceeds budget");
        assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
        assert!(
            error.to_string().contains("8/10"),
            "exact ordinal bounds: {error:#}"
        );
    });
    assert_eq!(fingerprint(public.path()), before);
    with_owner_storage(2, queued_owner(0), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        let missing = public.path().join("absent-root");
        let audit = inventory
            .verify_nod_inputs(&missing, CAS_LIMITS, 3, Some(0))
            .unwrap();
        assert_eq!((audit.jobs, audit.batches, audit.actions), (0, 0, 0));
        assert!(!missing.exists());
    });
    assert_eq!(fingerprint(public.path()), before);
}
