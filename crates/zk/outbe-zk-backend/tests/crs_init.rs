use outbe_zk_backend::barretenberg::init_crs;

#[test]
fn crs_initialization_is_idempotent() {
    init_crs().expect("first CRS initialization");
    init_crs().expect("repeated CRS initialization");
}
