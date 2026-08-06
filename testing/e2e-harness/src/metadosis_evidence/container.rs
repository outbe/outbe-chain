//! Portable host launcher for the pinned Metadosis Linux evidence lane.
//!
//! The host process only controls Docker and records its raw inspection facts.
//! Test execution, evidence assembly, and policy verification all run through
//! this crate inside the exact Linux/amd64 image.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::{
    fs,
    os::unix::fs::{FileTypeExt as _, MetadataExt as _},
};

use eyre::{bail, ensure, Result, WrapErr};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::runner::{digest_member, write_new, write_new_json, RUNNER_RECEIPT_MEMBER};
use super::schema::MetadosisRunReceiptV1;
use crate::verification_ledger::{verify_member_digests, MemberDigestV1};

pub const PROVENANCE_SCHEMA_VERSION: u32 = 1;
pub const PROVENANCE_KIND: &str = "outbe.metadosis-container-provenance";
pub const PROVENANCE_MEMBER: &str = "container/container-provenance.json";
const IMAGE_INSPECT_MEMBER: &str = "container/image-inspect.json";
const CONTAINER_INSPECT_MEMBER: &str = "container/container-inspect.json";
const RUNNER_STDOUT_MEMBER: &str = "container/runner.stdout";
const RUNNER_STDERR_MEMBER: &str = "container/runner.stderr";
const CONTAINER_DOCKER_SOCKET: &str = "/var/run/docker.sock";
const EVIDENCE_BINARY: &str = "outbe-metadosis-evidence";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerProvenanceV1 {
    pub schema_version: u32,
    pub kind: String,
    pub requested_image: String,
    pub requested_digest: String,
    pub resolved_image_id: String,
    pub container_id: String,
    pub launcher_nonce: String,
    pub target_path: String,
    pub docker_socket_path: String,
    pub inner_argv: Vec<String>,
    pub image_inspect: MemberDigestV1,
    pub container_inspect: MemberDigestV1,
    pub runner_stdout: MemberDigestV1,
    pub runner_stderr: MemberDigestV1,
}

pub struct RunnerBinding<'a> {
    pub linux_image_digest: &'a str,
    pub launcher_nonce: &'a str,
    pub repo: &'a Path,
    pub index: &'a Path,
    pub bundle_root: &'a Path,
}

/// Run one immutable evidence attempt and finalize it in the same pinned image.
/// This function is intentionally host-portable: it never links node runtime
/// code or substitutes a host result for the Linux evidence lane.
pub fn run_in_pinned_linux_container(
    repo: &Path,
    index_path: &Path,
    output: &Path,
    image: &str,
) -> Result<PathBuf> {
    let requested_digest = exact_image_digest(image)?;
    let repo = repo.canonicalize().wrap_err("canonicalize repository")?;
    let index = index_path
        .canonicalize()
        .wrap_err("canonicalize verification-ledger index")?;
    let index_relative = index
        .strip_prefix(&repo)
        .wrap_err("verification-ledger index must be inside the mounted repository")?;
    ensure_safe_relative(index_relative, "verification-ledger index")?;
    ensure!(!output.exists(), "evidence output already exists");
    let output_parent = output
        .parent()
        .ok_or_else(|| eyre::eyre!("evidence output has no parent"))?
        .canonicalize()
        .wrap_err("canonicalize evidence output parent")?;
    let output_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| eyre::eyre!("evidence output name is not UTF-8"))?;
    ensure!(
        Path::new(output_name).components().count() == 1,
        "evidence output must have one file-name component"
    );
    let output = output_parent.join(output_name);
    ensure!(
        !output.starts_with(&repo),
        "Metadosis evidence output must be outside the source checkout"
    );

    let target = tempfile::Builder::new()
        .prefix("outbe-metadosis-target-")
        .tempdir()
        .wrap_err("create container Cargo target directory")?;
    let target_path = target
        .path()
        .canonicalize()
        .wrap_err("canonicalize container Cargo target directory")?;
    fs::create_dir(target_path.join("tmp")).wrap_err("create shared container temp directory")?;
    let docker_socket = current_docker_socket()?;
    let nonce = random_nonce();
    let inner_argv = runner_argv(&repo, &index, &output, requested_digest, &nonce);

    let image_inspect_output = docker_output(["image", "inspect", image])?;
    ensure_success(&image_inspect_output, "inspect pinned Docker image")?;
    let image_inspect: Value = serde_json::from_slice(&image_inspect_output.stdout)
        .wrap_err("decode Docker image inspection")?;
    let resolved_image_id = inspected_image_id(&image_inspect, image)?;

    let create_args = create_args(
        &repo,
        &output_parent,
        &target_path,
        &docker_socket,
        image,
        &inner_argv,
    )?;
    let created = docker_output(create_args.iter().map(String::as_str))?;
    ensure_success(&created, "create pinned Metadosis evidence container")?;
    let container_id = String::from_utf8(created.stdout)
        .wrap_err("Docker container id is not UTF-8")?
        .trim()
        .to_owned();
    validate_container_id(&container_id)?;
    let cleanup = ContainerCleanup::new(container_id.clone());

    let execution = docker_output(["start", "--attach", container_id.as_str()])?;
    let container_inspect_output = docker_output(["container", "inspect", container_id.as_str()])?;
    ensure_success(
        &container_inspect_output,
        "inspect completed Metadosis evidence container",
    )?;
    let container_inspect: Value = serde_json::from_slice(&container_inspect_output.stdout)
        .wrap_err("decode Docker container inspection")?;

    std::fs::create_dir_all(output.join("container"))
        .wrap_err("create retained container evidence directory")?;
    write_new(
        &output.join(IMAGE_INSPECT_MEMBER),
        &image_inspect_output.stdout,
    )?;
    write_new(
        &output.join(CONTAINER_INSPECT_MEMBER),
        &container_inspect_output.stdout,
    )?;
    write_new(&output.join(RUNNER_STDOUT_MEMBER), &execution.stdout)?;
    write_new(&output.join(RUNNER_STDERR_MEMBER), &execution.stderr)?;
    let provenance = ContainerProvenanceV1 {
        schema_version: PROVENANCE_SCHEMA_VERSION,
        kind: PROVENANCE_KIND.to_owned(),
        requested_image: image.to_owned(),
        requested_digest: requested_digest.to_owned(),
        resolved_image_id,
        container_id: container_id.clone(),
        launcher_nonce: nonce,
        target_path: path(&target_path)?,
        docker_socket_path: path(&docker_socket)?,
        inner_argv,
        image_inspect: digest_member(&output, IMAGE_INSPECT_MEMBER)?,
        container_inspect: digest_member(&output, CONTAINER_INSPECT_MEMBER)?,
        runner_stdout: digest_member(&output, RUNNER_STDOUT_MEMBER)?,
        runner_stderr: digest_member(&output, RUNNER_STDERR_MEMBER)?,
    };
    write_new_json(&output.join(PROVENANCE_MEMBER), &provenance)?;

    validate_provenance_documents(
        &provenance,
        &image_inspect,
        &container_inspect,
        RunnerBinding {
            linux_image_digest: requested_digest,
            launcher_nonce: &provenance.launcher_nonce,
            repo: &repo,
            index: &index,
            bundle_root: &output,
        },
    )?;
    ensure_success(&execution, "execute Metadosis evidence lane")?;
    validate_retained_provenance(&repo, &index, &output, &decode_runner_receipt(&output)?)?;

    cleanup.remove()?;
    let finalize_argv = finalizer_argv(&repo, &index, &output);
    let finalized = docker_output(
        run_args(&repo, &output_parent, &target_path, image, &finalize_argv)?
            .iter()
            .map(String::as_str),
    )?;
    ensure_success(&finalized, "assemble and verify Metadosis evidence")?;
    Ok(output.join("domain-evidence.json"))
}

pub(crate) fn retained_provenance_members(
    repo: &Path,
    index: &Path,
    bundle_root: &Path,
    receipt: &MetadosisRunReceiptV1,
) -> Result<Vec<MemberDigestV1>> {
    let provenance = validate_retained_provenance(repo, index, bundle_root, receipt)?;
    let provenance_member = digest_member(bundle_root, PROVENANCE_MEMBER)?;
    let members = vec![
        provenance_member,
        provenance.image_inspect,
        provenance.container_inspect,
        provenance.runner_stdout,
        provenance.runner_stderr,
    ];
    verify_member_digests(bundle_root, &members)?;
    Ok(members)
}

fn validate_retained_provenance(
    repo: &Path,
    index: &Path,
    bundle_root: &Path,
    receipt: &MetadosisRunReceiptV1,
) -> Result<ContainerProvenanceV1> {
    let provenance: ContainerProvenanceV1 = serde_json::from_slice(
        &std::fs::read(bundle_root.join(PROVENANCE_MEMBER))
            .wrap_err("read retained container provenance")?,
    )
    .wrap_err("decode retained container provenance")?;
    let image_bytes = std::fs::read(bundle_root.join(IMAGE_INSPECT_MEMBER))
        .wrap_err("read retained Docker image inspection")?;
    let container_bytes = std::fs::read(bundle_root.join(CONTAINER_INSPECT_MEMBER))
        .wrap_err("read retained Docker container inspection")?;
    let image_inspect =
        serde_json::from_slice(&image_bytes).wrap_err("decode retained Docker image inspection")?;
    let container_inspect = serde_json::from_slice(&container_bytes)
        .wrap_err("decode retained Docker container inspection")?;
    verify_member_digests(
        bundle_root,
        &[
            provenance.image_inspect.clone(),
            provenance.container_inspect.clone(),
            provenance.runner_stdout.clone(),
            provenance.runner_stderr.clone(),
        ],
    )?;
    validate_provenance_documents(
        &provenance,
        &image_inspect,
        &container_inspect,
        RunnerBinding {
            linux_image_digest: &receipt.linux_image_digest,
            launcher_nonce: &receipt.launcher_nonce,
            repo,
            index,
            bundle_root,
        },
    )?;
    Ok(provenance)
}

pub fn validate_provenance_documents(
    provenance: &ContainerProvenanceV1,
    image_inspect: &Value,
    container_inspect: &Value,
    runner: RunnerBinding<'_>,
) -> Result<()> {
    ensure!(
        provenance.schema_version == PROVENANCE_SCHEMA_VERSION
            && provenance.kind == PROVENANCE_KIND,
        "unsupported container provenance envelope"
    );
    let requested_digest = exact_image_digest(&provenance.requested_image)?;
    ensure!(
        provenance.requested_digest == requested_digest
            && provenance.requested_digest == runner.linux_image_digest,
        "container image digest differs from the runner receipt"
    );
    validate_lower_hex_32(&provenance.launcher_nonce, "launcher nonce")?;
    ensure!(
        provenance.launcher_nonce == runner.launcher_nonce,
        "container launcher nonce differs from the runner receipt"
    );
    validate_image_id(&provenance.resolved_image_id)?;
    validate_container_id(&provenance.container_id)?;
    validate_inner_argv(
        &provenance.inner_argv,
        &provenance.requested_digest,
        &provenance.launcher_nonce,
        runner.repo,
        runner.index,
        runner.bundle_root,
    )?;
    let target_path = Path::new(&provenance.target_path);
    let docker_socket_path = Path::new(&provenance.docker_socket_path);
    ensure!(
        target_path.is_absolute()
            && docker_socket_path.is_absolute()
            && !target_path.starts_with(runner.repo)
            && !target_path.starts_with(runner.bundle_root),
        "container target/socket provenance contains unsafe paths"
    );
    validate_fixed_member(&provenance.image_inspect, IMAGE_INSPECT_MEMBER)?;
    validate_fixed_member(&provenance.container_inspect, CONTAINER_INSPECT_MEMBER)?;
    validate_fixed_member(&provenance.runner_stdout, RUNNER_STDOUT_MEMBER)?;
    validate_fixed_member(&provenance.runner_stderr, RUNNER_STDERR_MEMBER)?;

    let image = one_inspection(image_inspect, "image")?;
    ensure!(
        image["Id"].as_str() == Some(provenance.resolved_image_id.as_str())
            && image["Os"] == "linux"
            && image["Architecture"] == "amd64",
        "Docker image inspection is not the exact Linux/amd64 image"
    );
    let repo_digests = image["RepoDigests"]
        .as_array()
        .ok_or_else(|| eyre::eyre!("Docker image inspection has no RepoDigests"))?;
    ensure!(
        repo_digests
            .iter()
            .filter_map(Value::as_str)
            .any(|digest| digest == provenance.requested_image),
        "Docker image inspection does not contain the requested repo digest"
    );

    let container = one_inspection(container_inspect, "container")?;
    ensure!(
        container["Id"].as_str() == Some(provenance.container_id.as_str())
            && container["Image"].as_str() == Some(provenance.resolved_image_id.as_str())
            && container["Config"]["Image"].as_str() == Some(provenance.requested_image.as_str())
            && container["State"]["Running"] == false
            && container["State"]["ExitCode"].as_i64() == Some(0),
        "Docker container inspection is not a successful exact-image execution"
    );
    let entrypoint = string_array(&container["Config"]["Entrypoint"], "container entrypoint")?;
    let command = string_array(&container["Config"]["Cmd"], "container command")?;
    let observed_argv = entrypoint.into_iter().chain(command).collect::<Vec<_>>();
    ensure!(
        observed_argv == provenance.inner_argv,
        "Docker container command differs from the retained inner argv"
    );
    ensure!(
        container["Config"]["WorkingDir"].as_str() == runner.repo.to_str()
            && container["HostConfig"]["NetworkMode"] == "host"
            && container["HostConfig"]["Privileged"] == false
            && container["HostConfig"]["CapAdd"].is_null(),
        "Docker container privileges or network differ from the fixed runner contract"
    );
    validate_mounts(container, provenance, &runner)?;
    Ok(())
}

fn runner_argv(repo: &Path, index: &Path, output: &Path, digest: &str, nonce: &str) -> Vec<String> {
    let mut argv = cargo_prefix();
    argv.extend([
        "--repo".to_owned(),
        repo.display().to_string(),
        "--index".to_owned(),
        index.display().to_string(),
        "run".to_owned(),
        "--output".to_owned(),
        output.display().to_string(),
        "--linux-image-digest".to_owned(),
        digest.to_owned(),
        "--launcher-nonce".to_owned(),
        nonce.to_owned(),
    ]);
    argv
}

fn finalizer_argv(repo: &Path, index: &Path, output: &Path) -> Vec<String> {
    let mut argv = cargo_prefix();
    argv.extend([
        "--repo".to_owned(),
        repo.display().to_string(),
        "--index".to_owned(),
        index.display().to_string(),
        "finalize".to_owned(),
        "--bundle-root".to_owned(),
        output.display().to_string(),
        "--output".to_owned(),
        "domain-evidence.json".to_owned(),
    ]);
    argv
}

fn cargo_prefix() -> Vec<String> {
    [
        "cargo",
        "run",
        "--locked",
        "--offline",
        "-p",
        "outbe-e2e-harness",
        "--features",
        "ocomp-integration",
        "--bin",
        EVIDENCE_BINARY,
        "--",
    ]
    .map(ToOwned::to_owned)
    .to_vec()
}

fn create_args(
    repo: &Path,
    output_parent: &Path,
    target: &Path,
    docker_socket: &Path,
    image: &str,
    inner_argv: &[String],
) -> Result<Vec<String>> {
    let mut args =
        docker_container_prefix("create", repo, output_parent, target, Some(docker_socket))?;
    args.extend([
        "--entrypoint".to_owned(),
        "cargo".to_owned(),
        image.to_owned(),
    ]);
    args.extend(inner_argv.iter().skip(1).cloned());
    Ok(args)
}

fn run_args(
    repo: &Path,
    output_parent: &Path,
    target: &Path,
    image: &str,
    inner_argv: &[String],
) -> Result<Vec<String>> {
    let mut args = docker_container_prefix("run", repo, output_parent, target, None)?;
    args.push("--rm".to_owned());
    args.extend([
        "--entrypoint".to_owned(),
        "cargo".to_owned(),
        image.to_owned(),
    ]);
    args.extend(inner_argv.iter().skip(1).cloned());
    Ok(args)
}

fn docker_container_prefix(
    operation: &str,
    repo: &Path,
    output_parent: &Path,
    target: &Path,
    docker_socket: Option<&Path>,
) -> Result<Vec<String>> {
    let repo_mount = bind_mount(repo, path(repo)?, true)?;
    let evidence_mount = bind_mount(output_parent, path(output_parent)?, false)?;
    let target_mount = bind_mount(target, path(target)?, false)?;
    // SAFETY: these libc calls have no preconditions and only read the current
    // process credentials used for bind-mount ownership inside Docker.
    #[allow(unsafe_code)]
    let user = unsafe { format!("{}:{}", libc::geteuid(), libc::getegid()) };
    let mut args = vec![
        operation.to_owned(),
        "--platform".to_owned(),
        "linux/amd64".to_owned(),
        "--user".to_owned(),
        user,
        "--workdir".to_owned(),
        path(repo)?,
        "--mount".to_owned(),
        repo_mount,
        "--mount".to_owned(),
        evidence_mount,
        "--mount".to_owned(),
        target_mount,
        "--env".to_owned(),
        format!("CARGO_TARGET_DIR={}", target.display()),
        "--env".to_owned(),
        format!("TMPDIR={}", target.join("tmp").display()),
    ];
    if let Some(socket) = docker_socket {
        let metadata = fs::metadata(socket).wrap_err("stat Docker context socket")?;
        args.extend([
            "--network".to_owned(),
            "host".to_owned(),
            "--group-add".to_owned(),
            metadata.gid().to_string(),
            "--env".to_owned(),
            format!("DOCKER_HOST=unix://{CONTAINER_DOCKER_SOCKET}"),
            "--mount".to_owned(),
            bind_mount(socket, CONTAINER_DOCKER_SOCKET, false)?,
        ]);
    }
    Ok(args)
}

fn bind_mount(source: &Path, destination: impl AsRef<str>, read_only: bool) -> Result<String> {
    let source = source
        .to_str()
        .ok_or_else(|| eyre::eyre!("Docker bind source is not UTF-8"))?;
    ensure!(!source.contains(','), "Docker bind source contains a comma");
    let destination = destination.as_ref();
    ensure!(
        !destination.contains(','),
        "Docker bind destination contains a comma"
    );
    Ok(format!(
        "type=bind,src={source},dst={destination}{}",
        if read_only { ",readonly" } else { "" }
    ))
}

fn current_docker_socket() -> Result<PathBuf> {
    let endpoint = if let Some(value) = std::env::var_os("DOCKER_HOST") {
        value
            .into_string()
            .map_err(|_| eyre::eyre!("DOCKER_HOST is not UTF-8"))?
    } else {
        let inspected = docker_output(["context", "inspect"])?;
        ensure_success(&inspected, "inspect current Docker context")?;
        let contexts: Value = serde_json::from_slice(&inspected.stdout)
            .wrap_err("decode current Docker context inspection")?;
        one_inspection(&contexts, "context")?["Endpoints"]["docker"]["Host"]
            .as_str()
            .ok_or_else(|| eyre::eyre!("current Docker context has no daemon endpoint"))?
            .to_owned()
    };
    let socket = endpoint.strip_prefix("unix://").ok_or_else(|| {
        eyre::eyre!("Metadosis container lane requires a local unix Docker socket")
    })?;
    let socket = Path::new(socket)
        .canonicalize()
        .wrap_err("canonicalize current Docker context socket")?;
    ensure!(
        fs::metadata(&socket)?.file_type().is_socket(),
        "current Docker context endpoint is not a unix socket"
    );
    Ok(socket)
}

fn validate_mounts(
    container: &Value,
    provenance: &ContainerProvenanceV1,
    runner: &RunnerBinding<'_>,
) -> Result<()> {
    let mounts = container["Mounts"]
        .as_array()
        .ok_or_else(|| eyre::eyre!("Docker container inspection has no mounts"))?;
    ensure!(
        mounts.len() == 4,
        "Docker runner must have exactly four bind mounts"
    );
    let output_parent = runner
        .bundle_root
        .parent()
        .ok_or_else(|| eyre::eyre!("evidence bundle has no parent"))?;
    for (source, destination, read_write) in [
        (path(runner.repo)?, path(runner.repo)?, false),
        (path(output_parent)?, path(output_parent)?, true),
        (
            provenance.target_path.clone(),
            provenance.target_path.clone(),
            true,
        ),
        (
            provenance.docker_socket_path.clone(),
            CONTAINER_DOCKER_SOCKET.to_owned(),
            true,
        ),
    ] {
        ensure!(
            mounts.iter().any(|mount| {
                mount["Type"] == "bind"
                    && mount["Source"].as_str() == Some(source.as_str())
                    && mount["Destination"].as_str() == Some(destination.as_str())
                    && mount["RW"].as_bool() == Some(read_write)
            }),
            "Docker runner is missing the exact bind mount {source} -> {destination}"
        );
    }
    let env = string_array(&container["Config"]["Env"], "container environment")?;
    ensure!(
        env.iter()
            .any(|value| value == &format!("CARGO_TARGET_DIR={}", provenance.target_path))
            && env.iter().any(|value| {
                value
                    == &format!(
                        "TMPDIR={}/tmp",
                        provenance.target_path.trim_end_matches('/')
                    )
            })
            && env
                .iter()
                .any(|value| value == "DOCKER_HOST=unix:///var/run/docker.sock"),
        "Docker runner does not bind Cargo/temp writes to the same-path target mount"
    );
    let groups = string_array(
        &container["HostConfig"]["GroupAdd"],
        "container supplementary groups",
    )?;
    ensure!(
        groups.len() == 1 && groups[0].parse::<u32>().is_ok(),
        "Docker runner must add exactly the unix socket group"
    );
    let user = container["Config"]["User"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("Docker runner has no numeric user"))?;
    let (uid, gid) = user
        .split_once(':')
        .ok_or_else(|| eyre::eyre!("Docker runner user is not uid:gid"))?;
    ensure!(
        uid.parse::<u32>().is_ok() && gid.parse::<u32>().is_ok(),
        "Docker runner user is not numeric uid:gid"
    );
    Ok(())
}

fn path(value: &Path) -> Result<String> {
    value
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| eyre::eyre!("Docker path is not UTF-8"))
}

fn docker_output<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<Output> {
    let args = args.into_iter().collect::<Vec<_>>();
    Command::new("docker")
        .args(&args)
        .output()
        .wrap_err_with(|| format!("execute docker {}", args.join(" ")))
}

fn ensure_success(output: &Output, operation: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    bail!(
        "{operation} failed with {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
}

fn decode_runner_receipt(bundle_root: &Path) -> Result<MetadosisRunReceiptV1> {
    serde_json::from_slice(
        &std::fs::read(bundle_root.join(RUNNER_RECEIPT_MEMBER))
            .wrap_err("read container runner receipt")?,
    )
    .wrap_err("decode container runner receipt")
}

fn inspected_image_id(image_inspect: &Value, requested_image: &str) -> Result<String> {
    let image = one_inspection(image_inspect, "image")?;
    ensure!(
        image["Os"] == "linux" && image["Architecture"] == "amd64",
        "pinned Docker image must be Linux/amd64"
    );
    ensure!(
        image["RepoDigests"]
            .as_array()
            .is_some_and(|digests| digests.iter().any(|value| value == requested_image)),
        "requested repo digest is absent from Docker image inspection"
    );
    let image_id = image["Id"]
        .as_str()
        .ok_or_else(|| eyre::eyre!("Docker image inspection has no Id"))?
        .to_owned();
    validate_image_id(&image_id)?;
    Ok(image_id)
}

fn exact_image_digest(image: &str) -> Result<&str> {
    let (repository, digest) = image
        .rsplit_once('@')
        .ok_or_else(|| eyre::eyre!("Docker image must be pinned as repository@sha256:digest"))?;
    ensure!(
        !repository.is_empty() && !repository.contains('@'),
        "Docker image repository is invalid"
    );
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| eyre::eyre!("Docker image digest must start with sha256:"))?;
    validate_lower_hex_32(hex, "Docker image digest")?;
    Ok(digest)
}

fn validate_inner_argv(
    argv: &[String],
    digest: &str,
    nonce: &str,
    repo: &Path,
    index: &Path,
    output: &Path,
) -> Result<()> {
    ensure!(
        argv == runner_argv(repo, index, output, digest, nonce),
        "container runner argv differs from the fixed evidence command"
    );
    Ok(())
}

fn ensure_safe_relative(path: &Path, what: &str) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && path
                .components()
                .all(|component| { matches!(component, Component::Normal(_) | Component::CurDir) }),
        "{what} is not a safe repository-relative path"
    );
    Ok(())
}

fn one_inspection<'a>(value: &'a Value, what: &str) -> Result<&'a Value> {
    let values = value
        .as_array()
        .ok_or_else(|| eyre::eyre!("Docker {what} inspection is not an array"))?;
    ensure!(
        values.len() == 1,
        "Docker {what} inspection must contain exactly one object"
    );
    Ok(&values[0])
}

fn string_array(value: &Value, what: &str) -> Result<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| eyre::eyre!("{what} is not an array"))?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| eyre::eyre!("{what} contains a non-string"))
        })
        .collect()
}

fn validate_fixed_member(member: &MemberDigestV1, expected: &str) -> Result<()> {
    ensure!(
        member.path == expected,
        "container provenance member path differs from {expected}"
    );
    validate_lower_hex_32(&member.sha256, "container member digest")?;
    Ok(())
}

fn validate_image_id(value: &str) -> Result<()> {
    let digest = value
        .strip_prefix("sha256:")
        .ok_or_else(|| eyre::eyre!("Docker image id must start with sha256:"))?;
    validate_lower_hex_32(digest, "Docker image id")
}

fn validate_container_id(value: &str) -> Result<()> {
    validate_lower_hex_32(value, "Docker container id")
}

fn validate_lower_hex_32(value: &str, what: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{what} is not exact lowercase 32-byte hex"
    );
    Ok(())
}

fn random_nonce() -> String {
    let mut nonce = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut nonce);
    hex::encode(nonce)
}

struct ContainerCleanup {
    id: String,
    removed: bool,
}

impl ContainerCleanup {
    fn new(id: String) -> Self {
        Self { id, removed: false }
    }

    fn remove(mut self) -> Result<()> {
        let removed = docker_output(["container", "rm", "--force", self.id.as_str()])?;
        ensure_success(&removed, "remove temporary Metadosis evidence container")?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for ContainerCleanup {
    fn drop(&mut self) {
        if !self.removed {
            let _ = Command::new("docker")
                .args(["container", "rm", "--force", self.id.as_str()])
                .output();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verification_ledger::SourceIdentityV1;

    fn fixture(
        repo: &Path,
        index: &Path,
        bundle: &Path,
        target: &Path,
        socket: &Path,
    ) -> (ContainerProvenanceV1, Value, Value) {
        let digest = format!("sha256:{}", "a".repeat(64));
        let image = format!("registry.example/outbe-evidence@{digest}");
        let image_id = format!("sha256:{}", "b".repeat(64));
        let container_id = "c".repeat(64);
        let nonce = "e".repeat(64);
        let argv = runner_argv(repo, index, bundle, &digest, &nonce);
        let member = MemberDigestV1 {
            path: IMAGE_INSPECT_MEMBER.to_owned(),
            length: 1,
            sha256: "d".repeat(64),
        };
        (
            ContainerProvenanceV1 {
                schema_version: PROVENANCE_SCHEMA_VERSION,
                kind: PROVENANCE_KIND.to_owned(),
                requested_image: image.clone(),
                requested_digest: digest.clone(),
                resolved_image_id: image_id.clone(),
                container_id: container_id.clone(),
                launcher_nonce: nonce,
                target_path: target.display().to_string(),
                docker_socket_path: socket.display().to_string(),
                inner_argv: argv.clone(),
                image_inspect: member.clone(),
                container_inspect: MemberDigestV1 {
                    path: CONTAINER_INSPECT_MEMBER.to_owned(),
                    ..member.clone()
                },
                runner_stdout: MemberDigestV1 {
                    path: RUNNER_STDOUT_MEMBER.to_owned(),
                    ..member.clone()
                },
                runner_stderr: MemberDigestV1 {
                    path: RUNNER_STDERR_MEMBER.to_owned(),
                    ..member
                },
            },
            serde_json::json!([{
                "Id": image_id,
                "RepoDigests": [image],
                "Architecture": "amd64",
                "Os": "linux"
            }]),
            serde_json::json!([{
                "Id": container_id,
                "Image": image_id,
                "Config": {
                    "Image": image,
                    "Entrypoint": ["cargo"],
                    "Cmd": argv[1..],
                    "WorkingDir": repo,
                    "User": "501:20",
                    "Env": [
                        format!("CARGO_TARGET_DIR={}", target.display()),
                        format!("TMPDIR={}/tmp", target.display()),
                        "DOCKER_HOST=unix:///var/run/docker.sock"
                    ]
                },
                "HostConfig": {
                    "NetworkMode": "host",
                    "Privileged": false,
                    "CapAdd": null,
                    "GroupAdd": ["20"]
                },
                "Mounts": [
                    {"Type": "bind", "Source": repo, "Destination": repo, "RW": false},
                    {
                        "Type": "bind",
                        "Source": bundle.parent().unwrap(),
                        "Destination": bundle.parent().unwrap(),
                        "RW": true
                    },
                    {"Type": "bind", "Source": target, "Destination": target, "RW": true},
                    {
                        "Type": "bind",
                        "Source": socket,
                        "Destination": CONTAINER_DOCKER_SOCKET,
                        "RW": true
                    }
                ],
                "State": {"Running": false, "ExitCode": 0}
            }]),
        )
    }

    fn fixed_paths() -> (
        &'static Path,
        &'static Path,
        &'static Path,
        &'static Path,
        &'static Path,
    ) {
        (
            Path::new("/host/repo"),
            Path::new("/host/repo/outbe-plan/verification-ledger.yaml"),
            Path::new("/host/evidence/run-1"),
            Path::new("/host/target"),
            Path::new("/host/docker.sock"),
        )
    }

    #[test]
    fn container_provenance_accepts_exact_linux_amd64_execution() {
        let (repo, index, bundle, target, socket) = fixed_paths();
        let (provenance, image, container) = fixture(repo, index, bundle, target, socket);
        validate_provenance_documents(
            &provenance,
            &image,
            &container,
            RunnerBinding {
                linux_image_digest: &provenance.requested_digest,
                launcher_nonce: &provenance.launcher_nonce,
                repo,
                index,
                bundle_root: bundle,
            },
        )
        .unwrap();
    }

    #[test]
    fn container_provenance_rejects_image_command_and_runner_substitution() {
        let (repo, index, bundle, target, socket) = fixed_paths();
        let (provenance, image, container) = fixture(repo, index, bundle, target, socket);

        let mut wrong_image = container.clone();
        wrong_image[0]["Image"] = serde_json::json!(format!("sha256:{}", "f".repeat(64)));
        assert!(validate_provenance_documents(
            &provenance,
            &image,
            &wrong_image,
            RunnerBinding {
                linux_image_digest: &provenance.requested_digest,
                launcher_nonce: &provenance.launcher_nonce,
                repo,
                index,
                bundle_root: bundle,
            },
        )
        .is_err());

        let mut wrong_command = container.clone();
        wrong_command[0]["Config"]["Cmd"][0] = serde_json::json!("test");
        assert!(validate_provenance_documents(
            &provenance,
            &image,
            &wrong_command,
            RunnerBinding {
                linux_image_digest: &provenance.requested_digest,
                launcher_nonce: &provenance.launcher_nonce,
                repo,
                index,
                bundle_root: bundle,
            },
        )
        .is_err());

        let mut wrong_network = container.clone();
        wrong_network[0]["HostConfig"]["NetworkMode"] = serde_json::json!("bridge");
        assert!(validate_provenance_documents(
            &provenance,
            &image,
            &wrong_network,
            RunnerBinding {
                linux_image_digest: &provenance.requested_digest,
                launcher_nonce: &provenance.launcher_nonce,
                repo,
                index,
                bundle_root: bundle,
            },
        )
        .is_err());

        let mut wrong_socket = container.clone();
        wrong_socket[0]["Mounts"][3]["Source"] = serde_json::json!("/host/other.sock");
        assert!(validate_provenance_documents(
            &provenance,
            &image,
            &wrong_socket,
            RunnerBinding {
                linux_image_digest: &provenance.requested_digest,
                launcher_nonce: &provenance.launcher_nonce,
                repo,
                index,
                bundle_root: bundle,
            },
        )
        .is_err());

        let mut wrong_docker_host = container.clone();
        wrong_docker_host[0]["Config"]["Env"][2] =
            serde_json::json!("DOCKER_HOST=unix:///tmp/substitute.sock");
        assert!(validate_provenance_documents(
            &provenance,
            &image,
            &wrong_docker_host,
            RunnerBinding {
                linux_image_digest: &provenance.requested_digest,
                launcher_nonce: &provenance.launcher_nonce,
                repo,
                index,
                bundle_root: bundle,
            },
        )
        .is_err());

        assert!(validate_provenance_documents(
            &provenance,
            &image,
            &container,
            RunnerBinding {
                linux_image_digest: &provenance.requested_digest,
                launcher_nonce: &"0".repeat(64),
                repo,
                index,
                bundle_root: bundle,
            },
        )
        .is_err());
    }

    #[test]
    fn retained_container_provenance_rejects_raw_inspect_tampering() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("container")).unwrap();
        let repo = Path::new("/host/repo");
        let index = Path::new("/host/repo/outbe-plan/verification-ledger.yaml");
        let target = Path::new("/host/target");
        let socket = Path::new("/host/docker.sock");
        let (mut provenance, image, container) = fixture(repo, index, root.path(), target, socket);
        write_new(
            &root.path().join(IMAGE_INSPECT_MEMBER),
            &serde_json::to_vec(&image).unwrap(),
        )
        .unwrap();
        write_new(
            &root.path().join(CONTAINER_INSPECT_MEMBER),
            &serde_json::to_vec(&container).unwrap(),
        )
        .unwrap();
        write_new(&root.path().join(RUNNER_STDOUT_MEMBER), b"runner ok\n").unwrap();
        write_new(&root.path().join(RUNNER_STDERR_MEMBER), b"").unwrap();
        provenance.image_inspect = digest_member(root.path(), IMAGE_INSPECT_MEMBER).unwrap();
        provenance.container_inspect =
            digest_member(root.path(), CONTAINER_INSPECT_MEMBER).unwrap();
        provenance.runner_stdout = digest_member(root.path(), RUNNER_STDOUT_MEMBER).unwrap();
        provenance.runner_stderr = digest_member(root.path(), RUNNER_STDERR_MEMBER).unwrap();
        write_new_json(&root.path().join(PROVENANCE_MEMBER), &provenance).unwrap();
        let receipt = MetadosisRunReceiptV1 {
            schema_version: super::super::schema::METADOSIS_RUN_RECEIPT_SCHEMA_VERSION,
            kind: super::super::schema::METADOSIS_RUN_RECEIPT_KIND.to_owned(),
            namespace: super::super::schema::METADOSIS_NAMESPACE.to_owned(),
            source: SourceIdentityV1 {
                sha: "1".repeat(40),
                tracked_dirty: false,
                untracked_dirty: false,
                cargo_lock_sha256: "2".repeat(64),
                rust_toolchain_sha256: "3".repeat(64),
                dependency_metadata_sha256: "4".repeat(64),
            },
            artifact_profile: "profile".to_owned(),
            lane: "METADOSIS-LINUX".to_owned(),
            linux_image_digest: provenance.requested_digest.clone(),
            launcher_nonce: provenance.launcher_nonce.clone(),
            tests: Vec::new(),
        };

        assert!(retained_provenance_members(repo, index, root.path(), &receipt).is_ok());
        std::fs::write(root.path().join(IMAGE_INSPECT_MEMBER), b"[]").unwrap();
        assert!(retained_provenance_members(repo, index, root.path(), &receipt).is_err());
    }
}
