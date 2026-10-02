//! authority regression scenarios.
use super::*;

#[test]
fn corrupt_present_catalog_bundle_does_not_fall_back_to_valid_single_bundle() {
    let public = tempfile::tempdir().unwrap();
    let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
    let catalog = public
        .path()
        .join("protocol-bundles-v1")
        .join(format!("{}.ocb1", hex::encode(f.bundle.hash())));
    let fallback = public.path().join("protocol-bundle-v1.ocb1");
    fs::copy(&catalog, &fallback).unwrap();
    assert_eq!(
        PinnedProtocolBundle::decode(
            &fs::read(&fallback).unwrap(),
            f.bundle.hash(),
            &poc_schema_limits()
        )
        .unwrap(),
        f.bundle
    );
    fs::write(&catalog, b"invalid OCB1 bundle").unwrap();
    let before = fingerprint(public.path());
    with_owner_storage(2, canonical_owner(&[(&f, 0)], None), |state, source| {
        let scratch = tempfile::tempdir().unwrap();
        let inventory =
            CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
        let error = inventory
            .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
            .expect_err("present corrupt catalog must not be hidden by fallback");
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    });
    assert_eq!(fingerprint(public.path()), before);
}

#[test]
fn canonical_tribute_count_and_program_semantics_mismatches_are_failed() {
    for damage in ["tribute_count", "program_semantics"] {
        let public = tempfile::tempdir().unwrap();
        let f = fixture(public.path(), 0x30, WorldwideDay::new(20_260_725), 10);
        let mut owner = canonical_owner(&[(&f, 0)], None);
        StorageHandle::enter(&mut owner, |storage| {
            let nod = NodContract::new(storage);
            if damage == "tribute_count" {
                // Keep the canonical NOD/tribute counts mutually consistent,
                // but different from the native plan's ten tributes.
                let mut projection = nod.ocomp_certified_generation(f.day).unwrap().unwrap();
                projection.tribute_count = 11;
                projection.nod_count = 11;
                write_projection(&nod, &projection);
            } else {
                nod.ocomp_materialization_program_semantics_hash
                    .write(&f.day, hash(0x99))
                    .unwrap();
            }
        });
        let before = fingerprint(public.path());
        with_owner_storage(2, owner, |state, source| {
            let scratch = tempfile::tempdir().unwrap();
            let inventory =
                CanonicalInventory::scan(state, scratch.path(), &source.protected, None).unwrap();
            let error = inventory
                .verify_nod_inputs(public.path(), CAS_LIMITS, 3, None)
                .expect_err("canonical authority mismatch must fail");
            assert!(
                error.downcast_ref::<Incomplete>().is_none(),
                "{damage}: {error:#}"
            );
        });
        assert_eq!(fingerprint(public.path()), before);
    }
}
