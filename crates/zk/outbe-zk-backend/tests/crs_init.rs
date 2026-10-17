use outbe_zk_backend::barretenberg::{BarretenbergHandler, CANONICAL_SRS_POINTS};

#[test]
fn crs_initialization_is_idempotent() {
    // Concurrent embedding lifecycles must install only one native CRS and handler.
    std::thread::scope(|scope| {
        let callers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    BarretenbergHandler::init().expect("CRS initialization");
                    BarretenbergHandler::global()
                })
            })
            .collect();
        for caller in callers {
            let initialized = caller.join().unwrap();
            assert!(std::ptr::eq(initialized, BarretenbergHandler::global()));
        }
    });
    BarretenbergHandler::init().expect("repeated CRS initialization");
    let handler = BarretenbergHandler::global();
    handler
        .ensure_capacity(CANONICAL_SRS_POINTS - 1)
        .expect("smaller capacity remains available");
    let error = handler
        .ensure_capacity(CANONICAL_SRS_POINTS + 1)
        .unwrap_err();
    assert!(error.to_string().contains("one-shot"));
    handler
        .ensure_capacity(CANONICAL_SRS_POINTS)
        .expect("capacity rejection does not stop the owner");
}
