use outbe_zk_backend::barretenberg::{init_crs, preinit_srs, CANONICAL_SRS_POINTS};

#[test]
fn crs_initialization_is_idempotent() {
    init_crs().expect("first CRS initialization");
    init_crs().expect("repeated CRS initialization");
    preinit_srs(CANONICAL_SRS_POINTS - 1).expect("smaller capacity remains available");
    let error = preinit_srs(CANONICAL_SRS_POINTS + 1).unwrap_err();
    assert!(error.to_string().contains("one-shot"));
    init_crs().expect("capacity rejection does not stop the owner");
}
