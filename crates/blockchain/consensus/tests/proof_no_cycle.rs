//! AC1/AC2/AC3: the V2 proof crate must NOT depend on `outbe-consensus` or
//! `outbe-evm`, and MUST depend on `commonware-consensus`. These rules are the
//! whole reason for the crate's existence — they break the EVM↔consensus
//! cycle.
//!
//! This test shells out to `cargo tree` and asserts on the resolved dep graph.
//! Cargo subprocesses inside `cargo test` are slow and sometimes disallowed in
//! sandboxed CI, so the test is `#[ignore]`-tagged by default and opt-in via
//! `OUTBE_RUN_CARGO_TREE_TEST=1`. Audit cycles
//! (`make audit-quick` / `make audit-full`) opt in.

use std::process::Command;

fn run_cargo_tree() -> String {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "-p",
            "outbe-consensus-proof",
            "--prefix",
            "none",
            "--no-dedupe",
        ])
        .output()
        .expect("cargo tree must succeed");
    assert!(
        output.status.success(),
        "cargo tree exited with status {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("cargo tree output must be UTF-8")
}

#[test]
fn cargo_tree_outbe_consensus_proof_has_no_outbe_consensus_or_evm_dependency() {
    if std::env::var_os("OUTBE_RUN_CARGO_TREE_TEST").is_none() {
        eprintln!(
            "skipping: set OUTBE_RUN_CARGO_TREE_TEST=1 to enable the cargo-tree-backed \
             dependency-cycle check"
        );
        return;
    }
    let tree = run_cargo_tree();

    // Trailing space avoids false matches on `outbe-consensus-proof`.
    assert!(
        !tree
            .lines()
            .any(|line| line.starts_with("outbe-consensus ")),
        "outbe-consensus must not appear in outbe-consensus-proof's dep tree:\n{tree}",
    );
    assert!(
        !tree.lines().any(|line| line.starts_with("outbe-evm ")),
        "outbe-evm must not appear in outbe-consensus-proof's dep tree:\n{tree}",
    );
    assert!(
        tree.lines()
            .any(|line| line.starts_with("commonware-consensus ")),
        "commonware-consensus must appear in outbe-consensus-proof's dep tree:\n{tree}",
    );
}
