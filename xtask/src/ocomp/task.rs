use std::{
    path::Path,
    process::{Command, ExitStatus},
};

use eyre::{bail, Context, Result};

use super::registry;

pub fn run(repository_root: &Path, task: &str) -> Result<()> {
    match task {
        "OCM-00" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-lysis",
                    "--test",
                    "program_v1_reference",
                ],
            )?;
            task_progress(repository_root, task, &["OCM-EVD-001", "OCM-SEM-001"])?;
        }
        "OCM-01" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            cargo(repository_root, &["test", "--locked", "-p", "outbe-lysis"])?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-lysis",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(repository_root, task, &["OCM-EVD-001", "OCM-SEM-001"])?;
        }
        "OCM-02" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-lysis",
                    "--test",
                    "program_v1_reference",
                ],
            )?;
            registry::run(repository_root, true)?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "xtask", "ocomp::registry"],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-ocomp-protocol"],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-ocomp-protocol",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(repository_root, task, &["OCM-EVD-001", "OCM-SEM-001"])?;
        }
        "OCM-03" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            registry::run(repository_root, true)?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-ocomp-protocol",
                    "--test",
                    "foundation_vectors",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-ocomp-protocol",
                    "--test",
                    "typed_protocol",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-ocomp-protocol",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(repository_root, task, &["OCM-EVD-001", "OCM-SEM-001"])?;
        }
        "OCM-04" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            registry::run(repository_root, true)?;
            super::shape::run(repository_root, true)?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "xtask", "ocomp::shape"],
            )?;
            for test in [
                "golden_vectors",
                "abi_and_system_tx",
                "bounded_decode",
                "typed_protocol",
            ] {
                cargo(
                    repository_root,
                    &[
                        "test",
                        "--locked",
                        "-p",
                        "outbe-ocomp-protocol",
                        "--test",
                        test,
                    ],
                )?;
            }
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-ocomp-protocol",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(
                repository_root,
                task,
                &[
                    "OCM-EVD-001",
                    "OCM-SEM-001",
                    "OCM-BYT-001",
                    "OCM-BYT-002",
                    "OCM-BND-003",
                ],
            )?;
        }
        "OCM-05" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            registry::run(repository_root, true)?;
            super::shape::run(repository_root, true)?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-ocomp-protocol"],
            )?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-promislimit",
                    "-p",
                    "outbe-desis",
                    "-p",
                    "outbe-metadosis",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-promislimit",
                    "-p",
                    "outbe-desis",
                    "-p",
                    "outbe-metadosis",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-ocomp-protocol",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(
                repository_root,
                task,
                &[
                    "OCM-EVD-001",
                    "OCM-SEM-001",
                    "OCM-BYT-001",
                    "OCM-BYT-002",
                    "OCM-BND-003",
                ],
            )?;
        }
        "OCM-06" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            registry::run(repository_root, true)?;
            super::shape::run(repository_root, true)?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-ocomp-protocol"],
            )?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-compressed-entities",
                    "-p",
                    "outbe-tribute",
                    "-p",
                    "outbe-fidelity",
                    "-p",
                    "outbe-oracle",
                    "-p",
                    "outbe-metadosis",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-evm",
                    "handlers::update::tests",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-compressed-entities",
                    "-p",
                    "outbe-tribute",
                    "-p",
                    "outbe-fidelity",
                    "-p",
                    "outbe-oracle",
                    "-p",
                    "outbe-metadosis",
                    "-p",
                    "outbe-evm",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-ocomp-protocol",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(
                repository_root,
                task,
                &[
                    "OCM-EVD-001",
                    "OCM-SEM-001",
                    "OCM-BYT-001",
                    "OCM-BYT-002",
                    "OCM-BND-003",
                ],
            )?;
        }
        "OCM-07" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            registry::run(repository_root, true)?;
            super::shape::run(repository_root, true)?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-primitives"],
            )?;
            for integration_test in [
                "system_tx_layout",
                "e2e_system_tx",
                "phase1_safety_verification",
            ] {
                cargo(
                    repository_root,
                    &[
                        "test",
                        "--locked",
                        "-p",
                        "outbe-evm",
                        "--test",
                        integration_test,
                    ],
                )?;
            }
            for test in [
                "ethereum_post_execution_copy_matches_upstream_behavior_matrix",
                "active_terminal_request_is_last_semantic_writer_and_rejects_later_transactions",
                "active_lifecycle_proposer_and_replay_match_receipts_roots_and_header_artifacts",
            ] {
                cargo(
                    repository_root,
                    &["test", "--locked", "-p", "outbe-evm", "--lib", test],
                )?;
            }
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-node",
                    "--test",
                    "consensus_stateless",
                ],
            )?;
            for test in [
                "active_payload_builder_preserves_terminal_reservation_and_replays_exactly",
                "active_payload_builder_rejects_a_user_that_spends_one_unit_of_terminal_reserve",
            ] {
                cargo(
                    repository_root,
                    &["test", "--locked", "-p", "outbe-node", "--lib", test],
                )?;
            }
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-node", "--lib"],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-primitives",
                    "-p",
                    "outbe-evm",
                    "-p",
                    "outbe-node",
                    "-p",
                    "outbe-ocomp-protocol",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(
                repository_root,
                task,
                &[
                    "OCM-EVD-001",
                    "OCM-SEM-001",
                    "OCM-BYT-001",
                    "OCM-BYT-002",
                    "OCM-BND-003",
                ],
            )?;
        }
        "OCM-08" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-lysis",
                    "--test",
                    "program_v1_reference",
                ],
            )?;
            registry::run(repository_root, true)?;
            super::shape::run(repository_root, true)?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-ocomp-protocol"],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-compressed-entities"],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-metadosis"],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-evm", "--lib"],
            )?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-evm",
                    "--test",
                    "ocomp_request_lifecycle",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-compressed-entities",
                    "-p",
                    "outbe-metadosis",
                    "-p",
                    "outbe-evm",
                    "-p",
                    "outbe-node",
                    "-p",
                    "outbe-ocomp-protocol",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(
                repository_root,
                task,
                &[
                    "OCM-EVD-001",
                    "OCM-SEM-001",
                    "OCM-BYT-001",
                    "OCM-BYT-002",
                    "OCM-BND-003",
                    "OCM-FSM-001",
                    "OCM-REQ-001",
                ],
            )?;
        }
        "OCM-09" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            registry::run(repository_root, true)?;
            super::shape::run(repository_root, true)?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-ocomp-protocol"],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-compressed-entities"],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-metadosis"],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-evm", "--lib"],
            )?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-evm",
                    "--test",
                    "ocomp_request_lifecycle",
                ],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-consensus"],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-node", "ocm_pin_001"],
            )?;
            cargo(repository_root, &["test", "--locked", "-p", "outbe-engine"])?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-compressed-entities",
                    "-p",
                    "outbe-metadosis",
                    "-p",
                    "outbe-evm",
                    "-p",
                    "outbe-consensus",
                    "-p",
                    "outbe-node",
                    "-p",
                    "outbe-engine",
                    "-p",
                    "outbe-ocomp-protocol",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(
                repository_root,
                task,
                &[
                    "OCM-EVD-001",
                    "OCM-SEM-001",
                    "OCM-BYT-001",
                    "OCM-BYT-002",
                    "OCM-BND-003",
                    "OCM-FSM-001",
                    "OCM-REQ-001",
                    "OCM-FIN-001",
                    "OCM-PIN-001",
                ],
            )?;
        }
        "OCM-10" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            registry::run(repository_root, true)?;
            super::shape::run(repository_root, true)?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-ocomp-protocol"],
            )?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-compressed-entities",
                    "-p",
                    "outbe-tribute",
                    "-p",
                    "outbe-fidelity",
                    "-p",
                    "outbe-oracle",
                    "-p",
                    "outbe-offchain-data",
                ],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-node", "--lib"],
            )?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-e2e-harness",
                    "--features",
                    "ocomp-integration",
                    "--test",
                    "ocomp_checkpoint_handoff",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-compressed-entities",
                    "-p",
                    "outbe-tribute",
                    "-p",
                    "outbe-fidelity",
                    "-p",
                    "outbe-oracle",
                    "-p",
                    "outbe-offchain-data",
                    "-p",
                    "outbe-node",
                    "-p",
                    "outbe-ocomp-protocol",
                    "-p",
                    "outbe-e2e-harness",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--features",
                    "outbe-e2e-harness/ocomp-integration",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(
                repository_root,
                task,
                &[
                    "OCM-EVD-001",
                    "OCM-SEM-001",
                    "OCM-BYT-001",
                    "OCM-BYT-002",
                    "OCM-BND-003",
                    "OCM-FSM-001",
                    "OCM-REQ-001",
                    "OCM-FIN-001",
                    "OCM-PIN-001",
                ],
            )?;
        }
        "OCM-11" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            registry::run(repository_root, true)?;
            super::shape::run(repository_root, true)?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-ocomp-protocol"],
            )?;
            cargo(repository_root, &["test", "--locked", "-p", "outbe-ocomp"])?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-ocomp",
                    "-p",
                    "outbe-ocomp-protocol",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(
                repository_root,
                task,
                &[
                    "OCM-EVD-001",
                    "OCM-SEM-001",
                    "OCM-BYT-001",
                    "OCM-BYT-002",
                    "OCM-BND-003",
                    "OCM-FSM-001",
                    "OCM-REQ-001",
                    "OCM-FIN-001",
                    "OCM-PIN-001",
                    "OCM-CTL-001",
                ],
            )?;
        }
        "OCM-12" => {
            evidence_verifier(repository_root)?;
            reference(repository_root)?;
            registry::run(repository_root, true)?;
            super::shape::run(repository_root, true)?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-ocomp-protocol"],
            )?;
            cargo(
                repository_root,
                &["test", "--locked", "-p", "outbe-node", "ocm_pin_001"],
            )?;
            cargo(
                repository_root,
                &[
                    "test",
                    "--locked",
                    "-p",
                    "outbe-ocomp",
                    "--",
                    "--test-threads=1",
                ],
            )?;
            cargo(
                repository_root,
                &[
                    "clippy",
                    "--locked",
                    "-p",
                    "outbe-node",
                    "-p",
                    "outbe-ocomp",
                    "-p",
                    "outbe-ocomp-protocol",
                    "-p",
                    "xtask",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            )?;
            task_progress(
                repository_root,
                task,
                &[
                    "OCM-EVD-001",
                    "OCM-SEM-001",
                    "OCM-BYT-001",
                    "OCM-BYT-002",
                    "OCM-BND-003",
                    "OCM-FSM-001",
                    "OCM-REQ-001",
                    "OCM-FIN-001",
                    "OCM-PIN-001",
                    "OCM-CTL-001",
                    "OCM-DIS-001",
                ],
            )?;
        }
        _ => {
            cargo(
                repository_root,
                &[
                    "run",
                    "--locked",
                    "-p",
                    "outbe-e2e-harness",
                    "--bin",
                    "outbe-e2e-evidence",
                    "--",
                    "discover",
                ],
            )?;
            bail!("{task} is MISSING: its implementation gate has not been wired yet");
        }
    }
    Ok(())
}

fn reference(repository_root: &Path) -> Result<()> {
    cargo(
        repository_root,
        &["test", "--locked", "-p", "outbe-lysis-v1-reference"],
    )?;
    cargo(
        repository_root,
        &[
            "run",
            "--locked",
            "-p",
            "outbe-lysis-v1-reference",
            "--",
            "--mode",
            "check",
        ],
    )?;
    cargo(
        repository_root,
        &[
            "clippy",
            "--locked",
            "-p",
            "outbe-lysis-v1-reference",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )
}

fn evidence_verifier(repository_root: &Path) -> Result<()> {
    cargo(
        repository_root,
        &[
            "test",
            "--locked",
            "-p",
            "outbe-e2e-harness",
            "--test",
            "ocomp_evidence_verifier",
        ],
    )
}

fn task_progress(repository_root: &Path, task: &str, passed: &[&str]) -> Result<()> {
    let mut arguments = vec![
        "run",
        "--locked",
        "-p",
        "outbe-e2e-harness",
        "--bin",
        "outbe-e2e-evidence",
        "--",
        "task-progress",
        task,
    ];
    for test_id in passed {
        arguments.push("--passed");
        arguments.push(test_id);
    }
    cargo(repository_root, &arguments)
}

fn cargo(repository_root: &Path, arguments: &[&str]) -> Result<()> {
    eprintln!("+ cargo {}", arguments.join(" "));
    let status = Command::new("cargo")
        .args(arguments)
        .current_dir(repository_root)
        .status()
        .wrap_err_with(|| format!("failed starting cargo {}", arguments.join(" ")))?;
    require_success(status, arguments)
}

fn require_success(status: ExitStatus, arguments: &[&str]) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!(
            "cargo {} exited with {}",
            arguments.join(" "),
            status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string())
        )
    }
}
