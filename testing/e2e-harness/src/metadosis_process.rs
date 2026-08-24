//! Linux process lanes used by the Metadosis Citadel evidence runner.
//!
//! This module deliberately owns the build/run orchestration in Rust. The
//! evidence runner invokes the same binary through typed subcommands, so there
//! is no parallel shell-script contract.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use eyre::{ensure, Result, WrapErr};
#[cfg(feature = "ocomp-integration")]
use outbe_metadosis::config::OcompForkInstallV1;
#[cfg(feature = "ocomp-integration")]
use outbe_ocomp_protocol::profile::poc_schema_limits;

use crate::metadosis_p0::{
    capture_outbe_chain_feature_tree, current_clean_git_revision, verify_metadosis_p0_parity,
    MetadosisP0Case, MetadosisP0CaseInput, REMOVED_OWNER_FAILPOINT,
};

const FRESH_SCENARIO: &str =
    "A public Tribute completes real OCOMP, FullNode verification, NOD, replay, and contributor payout";
const P0_SCENARIO: &str = "A shard-cap-plus-one public population is completely processed";

fn fresh_node_build_args() -> [&'static str; 7] {
    [
        "--release",
        "-p",
        "outbe-chain",
        "--features",
        "e2e-test",
        "--bin",
        "outbe-chain",
    ]
}

pub fn run_fresh_devnet(repo: &Path, output: &Path) -> Result<()> {
    let lane = Lane::prepare(repo, output)?;
    fs::create_dir(lane.output.join("evidence"))
        .wrap_err("create fresh-devnet scenario evidence directory")?;
    let build = tempfile::tempdir().wrap_err("create fresh-devnet build root")?;
    let data = tempfile::tempdir().wrap_err("create fresh-devnet data root")?;
    let target = build.path();
    let build_log = lane.output.join("build.log");

    cargo_build(&lane, target, &fresh_node_build_args(), &build_log)?;
    cargo_build(
        &lane,
        target,
        &["--release", "-p", "outbe-ocomp", "--bin", "outbe-ocomp"],
        &build_log,
    )?;
    cargo_build(
        &lane,
        target,
        &["--release", "-p", "outbe-feeder", "--bin", "outbe-feeder"],
        &build_log,
    )?;
    cargo_build(
        &lane,
        target,
        &["--release", "-p", "outbe-cli", "--bin", "outbe-cli"],
        &build_log,
    )?;
    cargo_build(
        &lane,
        target,
        &["--release", "-p", "outbe-keygen", "--bin", "outbe-keygen"],
        &build_log,
    )?;
    cargo_build(
        &lane,
        target,
        &[
            "--release",
            "-p",
            "outbe-tee-enclave",
            "--bin",
            "outbe-tee-enclave",
        ],
        &build_log,
    )?;
    cargo_build(
        &lane,
        target,
        &[
            "--release",
            "-p",
            "outbe-e2e-harness",
            "--features",
            "ocomp-integration",
            "--bin",
            "outbe-e2e",
        ],
        &build_log,
    )?;

    let release = target.join("release");
    let artifacts = lane.output.join("artifacts");
    fs::create_dir(&artifacts).wrap_err("create fresh-devnet artifact directory")?;
    for binary in [
        "outbe-chain",
        "outbe-ocomp",
        "outbe-feeder",
        "outbe-cli",
        "outbe-keygen",
        "outbe-tee-enclave",
        "outbe-e2e",
    ] {
        fs::copy(release.join(binary), artifacts.join(binary))
            .wrap_err_with(|| format!("retain exact fresh-devnet {binary}"))?;
    }

    let mut command = Command::new(artifacts.join("outbe-e2e"));
    configure_inner_harness(&mut command, &lane.repo)
        .args(["--data-dir", path(data.path())])
        .args(["--evidence-dir", path(&lane.output.join("evidence"))])
        .args(["--chain-bin", path(&artifacts.join("outbe-chain"))])
        .args(["--ocomp-bin", path(&artifacts.join("outbe-ocomp"))])
        .args(["--feeder-bin", path(&artifacts.join("outbe-feeder"))])
        .args(["--cli-bin", path(&artifacts.join("outbe-cli"))])
        .args(["--keygen-bin", path(&artifacts.join("outbe-keygen"))])
        .args(["--enclave-bin", path(&artifacts.join("outbe-tee-enclave"))])
        .args([
            "--input",
            path(&lane.repo.join("testing/e2e-harness/features/ocomp.feature")),
        ])
        .args(["--name", FRESH_SCENARIO]);
    lane.run_logged(&mut command, &lane.output.join("run.log"))?;

    let scenarios = scenario_members(&lane.output.join("evidence"))?;
    ensure!(
        scenarios.len() == 1,
        "fresh-devnet run retained {} scenario receipts, expected one",
        scenarios.len()
    );
    retain_fresh_genesis_artifacts(&data.path().join("scenario-1/genesis.json"), &artifacts)?;
    Ok(())
}

#[cfg(feature = "ocomp-integration")]
fn retain_fresh_genesis_artifacts(genesis_path: &Path, artifacts: &Path) -> Result<()> {
    let genesis = fs::read(genesis_path)
        .wrap_err_with(|| format!("read fresh genesis {}", genesis_path.display()))?;
    let document: serde_json::Value =
        serde_json::from_slice(&genesis).wrap_err("decode fresh genesis")?;
    let canonical_hex = document["config"]["ocompForkInstallV1"]["canonicalBytes"]
        .as_str()
        .and_then(|value| value.strip_prefix("0x"))
        .ok_or_else(|| eyre::eyre!("fresh genesis has no canonical OCOMP install bytes"))?;
    let canonical_install =
        hex::decode(canonical_hex).wrap_err("decode canonical OCOMP install from fresh genesis")?;
    let limits = poc_schema_limits();
    let install = OcompForkInstallV1::decode_canonical(&canonical_install, &limits)
        .wrap_err("decode retained fresh OCOMP install")?;

    fs::write(artifacts.join("genesis.json"), genesis)?;
    fs::write(artifacts.join("fork-install-v1.ocb1"), canonical_install)?;
    fs::write(
        artifacts.join("protocol-bundle-v1.ocb1"),
        install.protocol_bundle.encode_canonical(&limits)?,
    )?;
    fs::write(
        artifacts.join("capacity-profile-v1.ocb1"),
        install
            .request_profile
            .capacity_profile
            .encode_canonical(&limits)?,
    )?;
    Ok(())
}

#[cfg(not(feature = "ocomp-integration"))]
fn retain_fresh_genesis_artifacts(_genesis_path: &Path, _artifacts: &Path) -> Result<()> {
    eyre::bail!("fresh-devnet evidence requires the ocomp-integration feature");
}

pub fn run_p0_parity(repo: &Path, output: &Path) -> Result<()> {
    let lane = Lane::prepare(repo, output)?;
    let work = tempfile::tempdir().wrap_err("create P0 work root")?;
    let node_target = work.path().join("node");
    let tools_target = work.path().join("tools");
    let data_root = work.path().join("data");
    fs::create_dir_all(&data_root)?;
    let artifacts = lane.output.join("artifacts");
    let cases_root = lane.output.join("cases");
    fs::create_dir(&artifacts)?;
    fs::create_dir(&cases_root)?;
    let build_log = lane.output.join("build.log");

    lane.record_logical(
        "cargo",
        &[
            "tree",
            "--locked",
            "-p",
            "outbe-chain",
            "-e",
            "features",
            "-i",
            "outbe-metadosis",
        ],
        &[],
    )?;
    let feature_tree = capture_outbe_chain_feature_tree(&lane.repo)?;
    fs::write(artifacts.join("outbe-chain-features.txt"), &feature_tree)?;

    cargo_build(
        &lane,
        &node_target,
        &["-p", "outbe-chain", "--bin", "outbe-chain"],
        &build_log,
    )?;
    let debug_node = artifacts.join("outbe-chain-debug");
    fs::copy(node_target.join("debug/outbe-chain"), &debug_node)
        .wrap_err("retain debug outbe-chain")?;
    cargo_build(
        &lane,
        &node_target,
        &["--release", "-p", "outbe-chain", "--bin", "outbe-chain"],
        &build_log,
    )?;
    let release_node = artifacts.join("outbe-chain-release");
    fs::copy(node_target.join("release/outbe-chain"), &release_node)
        .wrap_err("retain release outbe-chain")?;

    cargo_build(
        &lane,
        &tools_target,
        &["-p", "outbe-ocomp", "--bin", "outbe-ocomp"],
        &build_log,
    )?;
    cargo_build(
        &lane,
        &tools_target,
        &["-p", "outbe-cli", "--bin", "outbe-cli"],
        &build_log,
    )?;
    cargo_build(
        &lane,
        &tools_target,
        &["-p", "outbe-keygen", "--bin", "outbe-keygen"],
        &build_log,
    )?;
    cargo_build(
        &lane,
        &tools_target,
        &[
            "--release",
            "-p",
            "outbe-tee-enclave",
            "--bin",
            "outbe-tee-enclave",
        ],
        &build_log,
    )?;
    cargo_build(
        &lane,
        &tools_target,
        &[
            "--release",
            "-p",
            "outbe-e2e-harness",
            "--features",
            "ocomp-integration",
            "--bin",
            "outbe-e2e",
        ],
        &build_log,
    )?;

    for case in MetadosisP0Case::ALL {
        let node = if case.label().starts_with("debug_") {
            &debug_node
        } else {
            &release_node
        };
        run_p0_case(&lane, &tools_target, &data_root, &cases_root, case, node)?;
    }

    let source_revision = current_clean_git_revision(&lane.repo)?;
    let inputs = MetadosisP0Case::ALL
        .into_iter()
        .map(|case| MetadosisP0CaseInput {
            case,
            evidence_dir: cases_root.join(case.label()),
        })
        .collect::<Vec<_>>();
    let evidence = verify_metadosis_p0_parity(
        &source_revision,
        &feature_tree,
        &debug_node,
        &release_node,
        &inputs,
    )?;
    let mut bytes = serde_json::to_vec_pretty(&evidence)?;
    bytes.push(b'\n');
    fs::write(lane.output.join("metadosis-p0-parity.json"), bytes)?;
    fs::write(
        lane.output.join("verifier.log"),
        format!("Metadosis P0 parity PASS at {source_revision}\n"),
    )?;
    Ok(())
}

fn run_p0_case(
    lane: &Lane,
    tools_target: &Path,
    data_root: &Path,
    cases_root: &Path,
    case: MetadosisP0Case,
    node: &Path,
) -> Result<()> {
    let case_dir = cases_root.join(case.label());
    let data_dir = data_root.join(case.label());
    fs::create_dir(&case_dir)?;
    fs::create_dir(&data_dir)?;
    let mut command = Command::new(tools_target.join("release/outbe-e2e"));
    configure_inner_harness(&mut command, &lane.repo)
        .args(["--metadosis-p0-case", case.cli_value()])
        .args(["--data-dir", path(&data_dir)])
        .args(["--evidence-dir", path(&case_dir)])
        .args(["--chain-bin", path(node)])
        .args(["--ocomp-bin", path(&tools_target.join("debug/outbe-ocomp"))])
        .args(["--cli-bin", path(&tools_target.join("debug/outbe-cli"))])
        .args([
            "--keygen-bin",
            path(&tools_target.join("debug/outbe-keygen")),
        ])
        .args([
            "--enclave-bin",
            path(&tools_target.join("release/outbe-tee-enclave")),
        ])
        .args([
            "--input",
            path(&lane.repo.join("testing/e2e-harness/features/ocomp.feature")),
        ])
        .args(["--name", P0_SCENARIO]);
    match case {
        MetadosisP0Case::DebugUnset | MetadosisP0Case::ReleaseUnset => {
            command.env_remove(REMOVED_OWNER_FAILPOINT);
        }
        MetadosisP0Case::DebugNamed | MetadosisP0Case::ReleaseNamed => {
            command.env(REMOVED_OWNER_FAILPOINT, "nod_receipt_root");
        }
        MetadosisP0Case::DebugArbitrary | MetadosisP0Case::ReleaseArbitrary => {
            command.env(
                REMOVED_OWNER_FAILPOINT,
                "metadosis-p0-arbitrary-unrecognized-value",
            );
        }
    }
    lane.run_logged(
        &mut command,
        &lane.output.join(format!("{}.log", case.label())),
    )
}

fn configure_inner_harness<'a>(command: &'a mut Command, repo: &Path) -> &'a mut Command {
    command
        .args(["--repo", path(repo)])
        // The outer evidence container receives the current Docker socket and
        // its group explicitly. Escalating again inside that container is both
        // unnecessary and non-portable.
        .arg("--no-sudo")
        .args(["--tee", "gramine-direct"])
        .args(["--validators", "4", "--all"])
}

fn cargo_build(lane: &Lane, target: &Path, args: &[&str], output: &Path) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .arg("build")
        .arg("--locked")
        .args(args)
        .env("CARGO_TARGET_DIR", target);
    lane.run_logged(&mut command, output)
}

struct Lane {
    repo: PathBuf,
    output: PathBuf,
    commands: PathBuf,
}

impl Lane {
    fn prepare(repo: &Path, output: &Path) -> Result<Self> {
        require_linux_lane()?;
        let repo = repo.canonicalize().wrap_err("canonicalize repository")?;
        current_clean_git_revision(&repo)?;
        ensure!(!output.exists(), "process evidence output already exists");
        let parent = output
            .parent()
            .ok_or_else(|| eyre::eyre!("process output has no parent"))?
            .canonicalize()
            .wrap_err("canonicalize process output parent")?;
        let output = parent.join(
            output
                .file_name()
                .ok_or_else(|| eyre::eyre!("process output has no file name"))?,
        );
        ensure!(
            !output.starts_with(&repo),
            "process evidence output must be outside the source checkout"
        );
        fs::create_dir(&output)?;
        Ok(Self {
            repo,
            commands: output.join("commands.log"),
            output,
        })
    }

    fn run_logged(&self, command: &mut Command, output: &Path) -> Result<()> {
        self.record(command)?;
        let file = append_file(output)?;
        let stderr = file.try_clone()?;
        command
            .current_dir(&self.repo)
            .stdout(Stdio::from(file))
            .stderr(Stdio::from(stderr));
        let status = command
            .status()
            .wrap_err_with(|| format!("execute {:?}", command.get_program()))?;
        ensure!(
            status.success(),
            "process command {:?} failed with {status}; see {}",
            command.get_program(),
            output.display()
        );
        Ok(())
    }

    fn record(&self, command: &Command) -> Result<()> {
        let program = command.get_program().to_string_lossy().into_owned();
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let env = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<Vec<_>>();
        self.record_logical(
            &program,
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            &env,
        )
    }

    fn record_logical(
        &self,
        program: &str,
        args: &[&str],
        env: &[(String, Option<String>)],
    ) -> Result<()> {
        let mut log = append_file(&self.commands)?;
        serde_json::to_writer(
            &mut log,
            &serde_json::json!({
                "cwd": self.repo,
                "program": program,
                "args": args,
                "env": env,
            }),
        )?;
        log.write_all(b"\n")?;
        log.sync_all()?;
        Ok(())
    }
}

fn append_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .wrap_err_with(|| format!("open process log {}", path.display()))
}

fn require_linux_lane() -> Result<()> {
    ensure!(
        cfg!(target_os = "linux"),
        "Metadosis process evidence requires Linux"
    );
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "Metadosis process evidence requires x86_64"
    );
    let release = fs::read_to_string("/etc/os-release").wrap_err("read /etc/os-release")?;
    let fields = release
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key, value.trim_matches('"')))
        .collect::<std::collections::BTreeMap<_, _>>();
    ensure!(
        fields.get("ID") == Some(&"ubuntu") && fields.get("VERSION_ID") == Some(&"24.04"),
        "Metadosis process evidence requires Ubuntu 24.04"
    );
    Ok(())
}

fn scenario_members(root: &Path) -> Result<Vec<PathBuf>> {
    let mut members = fs::read_dir(root)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("scenario-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    members.sort();
    Ok(members)
}

fn path(path: &Path) -> &str {
    path.to_str().expect("evidence path must be UTF-8")
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    #[test]
    fn process_lanes_reject_non_linux_hosts_before_orchestration() {
        let error = super::require_linux_lane().expect_err("non-Linux host must be rejected");
        assert!(error.to_string().contains("requires Linux"));
    }
}

#[cfg(test)]
mod command_tests {
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn nested_e2e_uses_the_mounted_docker_socket_without_sudo() {
        let mut command = Command::new("outbe-e2e");
        super::configure_inner_harness(&mut command, Path::new("/host/repo"));
        let args = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            [
                "--repo",
                "/host/repo",
                "--no-sudo",
                "--tee",
                "gramine-direct",
                "--validators",
                "4",
                "--all",
            ]
        );
    }

    #[test]
    fn fresh_devnet_node_build_enables_only_the_required_test_arming_surface() {
        assert_eq!(
            super::fresh_node_build_args(),
            [
                "--release",
                "-p",
                "outbe-chain",
                "--features",
                "e2e-test",
                "--bin",
                "outbe-chain",
            ]
        );
    }
}
