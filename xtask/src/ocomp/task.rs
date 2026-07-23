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
