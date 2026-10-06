//! Fast guard for the consensus crate's direct dependency boundary.
//!
//! `mise run audit-consensus-deps` checks the resolved transitive graph
//! separately. Running Cargo recursively from a Rust test can deadlock on
//! Cargo's package-cache lock. For this reason, this default-suite test reads
//! the package manifest directly instead.

const MANIFEST: &str = include_str!("../Cargo.toml");

#[test]
fn consensus_manifest_has_no_direct_evm_dependency() {
    assert!(
        !MANIFEST.lines().any(|line| {
            line.trim_start().starts_with("outbe-evm.")
                || line.trim_start().starts_with("outbe-evm ")
                || line.trim_start().starts_with("outbe-evm=")
        }),
        "outbe-consensus must not directly depend on outbe-evm",
    );
    assert!(
        MANIFEST
            .lines()
            .any(|line| line.trim_start().starts_with("commonware-consensus.")),
        "outbe-consensus must directly depend on commonware-consensus",
    );
}
