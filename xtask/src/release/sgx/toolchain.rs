use super::is_lower_hex;
use super::BundleSpec;

use eyre::bail;

use eyre::Result;
use eyre::WrapErr;

use std::path::Path;

use std::process::Command;
use std::process::Output;

pub(super) fn build_project_toolchain_image(
    repo_root: &Path,
    spec: &BundleSpec,
    source_commit: &str,
) -> Result<String> {
    if !is_lower_hex(source_commit, 40) {
        bail!("project toolchain image requires an exact source commit");
    }
    let image = format!("outbe-project-toolchain:{source_commit}");
    let dockerfile = repo_root.join("Dockerfile.project-toolchain");
    let mut command = Command::new("docker");
    command
        .args(["build", "--platform", &spec.platform, "--file"])
        .arg(&dockerfile)
        .args(["--target", "toolchain", "--tag", &image])
        .arg(repo_root);
    run_status(&mut command, "build project toolchain image")?;
    Ok(image)
}

pub(super) fn docker_command(spec: &BundleSpec, repo_root: &Path) -> Result<Command> {
    let uid = current_id("-u")?;
    let gid = current_id("-g")?;
    let mut command = Command::new("docker");
    command
        .args(["run", "--rm", "--platform", &spec.platform])
        .args(["--user", &format!("{uid}:{gid}")])
        .args(["--entrypoint", "bash"])
        .args(["-v", &format!("{}:/source:ro", repo_root.display())]);
    Ok(command)
}

fn current_id(flag: &str) -> Result<String> {
    let mut command = Command::new("id");
    command.arg(flag);
    Ok(run_output(&mut command, "resolve current Unix identity")?
        .trim()
        .to_owned())
}

pub(super) fn container_adapter() -> &'static str {
    "/source/scripts/release/build-sgx-bundle-in-container.sh"
}

pub(super) fn run_status(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .wrap_err_with(|| format!("failed to start command: {description}"))?;
    if !status.success() {
        bail!("{description} failed with {status}");
    }
    Ok(())
}

pub(super) fn run_output(command: &mut Command, description: &str) -> Result<String> {
    let Output {
        status,
        stdout,
        stderr,
    } = command
        .output()
        .wrap_err_with(|| format!("failed to start command: {description}"))?;
    if !status.success() {
        bail!(
            "{description} failed with {status}: {}",
            String::from_utf8_lossy(&stderr).trim()
        );
    }
    String::from_utf8(stdout).wrap_err_with(|| format!("{description} emitted non-UTF-8 output"))
}
