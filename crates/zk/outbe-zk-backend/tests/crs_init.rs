use outbe_zk_backend::barretenberg::{
    init_crs, preinit_srs, BarretenbergHandler, CANONICAL_SRS_POINTS,
};

#[test]
fn crs_initialization_is_idempotent() {
    // Concurrent embedding lifecycles must install only one native CRS and handler.
    std::thread::scope(|scope| {
        let callers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    init_crs().expect("CRS initialization");
                    BarretenbergHandler::global().unwrap()
                })
            })
            .collect();
        for caller in callers {
            let initialized = caller.join().unwrap();
            assert!(std::ptr::eq(
                initialized,
                BarretenbergHandler::global().unwrap()
            ));
        }
    });
    init_crs().expect("repeated CRS initialization");
    preinit_srs(CANONICAL_SRS_POINTS - 1).expect("smaller capacity remains available");
    let error = preinit_srs(CANONICAL_SRS_POINTS + 1).unwrap_err();
    assert!(error.to_string().contains("one-shot"));
    init_crs().expect("capacity rejection does not stop the owner");
}
