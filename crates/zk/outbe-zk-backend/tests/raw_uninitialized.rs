//! Global access requires successful launch initialization. It must never initialize CRS data.

use outbe_zk_backend::barretenberg::{set_srs_path, BarretenbergHandler, CANONICAL_SRS_POINTS};
use outbe_zk_backend::errors::{CrsError, HandlerError};

#[test]
fn global_requires_successful_launch_initialization() {
    assert!(std::panic::catch_unwind(BarretenbergHandler::global).is_err());
    let path = std::env::temp_dir().join(format!("outbe-invalid-crs-{}.dat", std::process::id()));
    let file = std::fs::File::create_new(&path).unwrap();
    file.set_len(u64::from(CANONICAL_SRS_POINTS) * 64).unwrap();
    drop(file);
    set_srs_path(path.clone());
    assert!(matches!(
        BarretenbergHandler::init(),
        Err(HandlerError::Initialization(
            CrsError::SrsChecksumMismatch { .. }
        ))
    ));
    assert!(std::panic::catch_unwind(BarretenbergHandler::global).is_err());
    std::fs::remove_file(path).unwrap();
}
