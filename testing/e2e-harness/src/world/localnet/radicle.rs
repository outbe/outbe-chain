//! Operator-owned Radicle sidecars for release LocalNet scenarios.

use std::fs::{self, OpenOptions};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use eyre::{bail, eyre, Result, WrapErr};

use crate::internal::proc::{args, wait_tcp, ChildGuard};

use super::Localnet;

const VALIDATOR_SET_ALLOC_ADDRESS: &str = "000000000000000000000000000000000000ee00";
const VALIDATOR_SET_MAX_VALIDATORS_SLOT: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000001";
const PORTABLE_UNIX_SOCKET_PATH_LIMIT: usize = 104;

#[derive(Clone, Debug)]
pub struct RadicleRepositoryFixtureV1 {
    pub home: PathBuf,
    pub worktree: PathBuf,
    pub repo_id: String,
    pub repo_id_hex: String,
    pub issue_id: String,
    pub patch_id: String,
    pub pushed_commit: Option<String>,
}

impl Localnet {
    pub(crate) fn radicle_control_socket(&self, index: usize) -> std::path::PathBuf {
        self.cfg.radicle_control_socket(index)
    }

    /// Start the operator-owned foreground sidecar before its validator node.
    pub(crate) fn start_radicle(&mut self, index: usize) -> Result<()> {
        self.start_radicle_at(index, self.cfg.radicle_port(index))
    }

    fn start_radicle_at(&mut self, index: usize, peer_port: u16) -> Result<()> {
        if self
            .radicle_sidecars
            .get_mut(&index)
            .is_some_and(|guard| !guard.exited())
        {
            return Ok(());
        }
        self.radicle_sidecars.remove(&index);

        let validator_dir = self.cfg.validator_dir(index);
        let home = validator_dir.join("radicle");
        let key = home.join("keys/radicle");
        if !key.is_file() {
            bail!(
                "validator-{index} has no Radicle key at {}; bootstrap or install the identity first",
                key.display()
            );
        }
        fs::create_dir_all(validator_dir.join("logs"))?;

        let script = self.cfg.repo.join("scripts/run-radicle.sh");
        let listen = format!("127.0.0.1:{peer_port}");
        let status = format!("127.0.0.1:{}", self.cfg.radicle_status_port(index));
        let advertise = listen.clone();
        let max_validators = materialized_max_validators(&self.cfg.dir.join("genesis.json"))?;
        let control_socket = self.radicle_control_socket(index);
        prepare_private_socket_parent(
            &self.cfg.radicle_runtime_root,
            &control_socket,
            &validator_dir,
        )?;
        let mut command = Command::new(&script);
        command
            .env("OUTBE_RADICLE_BINARY", &self.cfg.bin_radicle)
            .env("OUTBE_RADICLE_CONTROL_SOCKET", &control_socket)
            .args(args![
                home.display(),
                listen,
                status,
                max_validators,
                advertise,
            ]);
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(validator_dir.join("radicle.log"))?;
        command
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .stdin(Stdio::null());

        let label = format!("radicle-{index}");
        let mut guard = ChildGuard::spawn(&label, command)
            .wrap_err_with(|| format!("start {label} through {}", script.display()))?;
        if !wait_tcp(self.cfg.radicle_status_port(index), 100) {
            guard.stop();
            return Err(eyre!(
                "validator-{index} Radicle status endpoint never came up; see {}",
                validator_dir.join("radicle.log").display()
            ));
        }
        self.radicle_sidecars.insert(index, guard);
        Ok(())
    }

    /// Stop only the selected sidecar; consensus ownership remains with the node.
    pub fn stop_radicle(&mut self, index: usize) -> Result<()> {
        self.radicle_sidecars
            .remove(&index)
            .ok_or_else(|| eyre!("radicle-{index} is not running"))?;
        Ok(())
    }

    /// Restart one sidecar with the same key and persistent home.
    pub fn restart_radicle(&mut self, index: usize) -> Result<()> {
        self.radicle_sidecars.remove(&index);
        self.start_radicle(index)
    }

    /// Restart one sidecar on a new peer port without restarting outbe-chain.
    pub fn restart_radicle_at_port(&mut self, index: usize, peer_port: u16) -> Result<()> {
        self.radicle_sidecars.remove(&index);
        self.start_radicle_at(index, peer_port)
    }

    /// Use a disjoint harness port block for endpoint-replacement evidence.
    pub fn restart_radicle_on_alternate_port(&mut self, index: usize) -> Result<u16> {
        let peer_port = self.cfg.radicle_port(index + 64);
        self.restart_radicle_at_port(index, peer_port)?;
        Ok(peer_port)
    }

    pub fn radicle_pid(&self, index: usize) -> Result<u32> {
        self.radicle_sidecars
            .get(&index)
            .map(ChildGuard::pid)
            .ok_or_else(|| eyre!("radicle-{index} is not running"))
    }

    pub fn validator_radicle_addresses(&self, index: usize) -> Result<Vec<String>> {
        let home = self.cfg.validator_dir(index).join("radicle");
        let socket = self.radicle_control_socket(index);
        let output = self.rad(&home, &socket, None, &["node", "config", "--addresses"])?;
        let addresses = output
            .lines()
            .map(str::trim)
            .filter(|line| line.contains('@'))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if addresses.is_empty() {
            bail!("validator-{index} Radicle node returned no advertised address");
        }
        Ok(addresses)
    }

    /// Create a public source repository while its non-validator sidecar is offline.
    pub fn prepare_user_radicle_repository(&self) -> Result<RadicleRepositoryFixtureV1> {
        let home = self.cfg.dir.join("user-radicle");
        if !home.exists() {
            let output = Command::new(&self.cfg.bin_keygen)
                .args(["radicle", "--output-dir"])
                .arg(&home)
                .output()
                .wrap_err("generate independent user Radicle key")?;
            if !output.status.success() {
                bail!(
                    "generate independent user Radicle key failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        for dir in ["storage", "node", "cobs"] {
            let path = home.join(dir);
            fs::create_dir_all(&path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        write_outbe_profile(
            &home,
            &format!(
                "127.0.0.1:{}",
                self.cfg.radicle_port(self.committee_size() + 32)
            ),
            materialized_max_validators(&self.cfg.dir.join("genesis.json"))?,
        )?;

        let worktree = self.cfg.dir.join("user-repository");
        fs::create_dir_all(&worktree)?;
        self.git(&worktree, &["init", "--initial-branch", "master"])?;
        self.git(&worktree, &["config", "user.name", "Outbe E2E"])?;
        self.git(
            &worktree,
            &["config", "user.email", "radicle-e2e@outbe.test"],
        )?;
        fs::write(worktree.join("README.md"), "# Outbe Radicle E2E\n")?;
        self.git(&worktree, &["add", "README.md"])?;
        self.git(&worktree, &["commit", "-m", "Initial source"])?;
        self.rad(
            &home,
            &self.cfg.user_radicle_control_socket(),
            Some(&worktree),
            &[
                "init",
                "--name",
                "outbe-radicle-e2e",
                "--description",
                "Outbe validator replication fixture",
                "--no-confirm",
                "--public",
                "--no-seed",
            ],
        )?;
        let mut initializer = self.spawn_user_radicle(&home)?;
        initializer.stop();
        self.rad(
            &home,
            &self.cfg.user_radicle_control_socket(),
            Some(&worktree),
            &["cob", "migrate"],
        )?;

        let remote = self.git(&worktree, &["config", "--get", "remote.rad.url"])?;
        let repo_id = remote
            .trim()
            .strip_prefix("rad://")
            .and_then(|remote| remote.split('/').next())
            .map(str::to_owned)
            .ok_or_else(|| eyre!("rad init installed an invalid remote URL: {remote}"))?;
        let repo_id_hex = hex::encode(decode_repo_id(&repo_id)?);
        let repo_id = format!("rad:{repo_id}");
        let issue = self.rad(
            &home,
            &self.cfg.user_radicle_control_socket(),
            Some(&worktree),
            &[
                "issue",
                "open",
                "--title",
                "Validator replication evidence",
                "--description",
                "Created before the source sidecar starts",
                "--no-announce",
            ],
        )?;
        let issue_id = first_hex40(&issue).ok_or_else(|| eyre!("rad issue returned no ID"))?;

        self.git(&worktree, &["checkout", "-b", "radicle-e2e-patch"])?;
        fs::write(
            worktree.join("PATCH-EVIDENCE.md"),
            "This change must replicate through validator storage.\n",
        )?;
        self.git(&worktree, &["add", "PATCH-EVIDENCE.md"])?;
        self.git(&worktree, &["commit", "-m", "Add replication evidence"])?;
        let patch = self.git_env(
            &home,
            &self.cfg.user_radicle_control_socket(),
            &worktree,
            &[
                "push",
                "-o",
                "patch.message=Outbe replication patch",
                "rad",
                "HEAD:refs/patches",
            ],
        )?;
        let patch_id = first_hex40(&patch).ok_or_else(|| eyre!("git push returned no patch ID"))?;

        Ok(RadicleRepositoryFixtureV1 {
            home,
            worktree,
            repo_id,
            repo_id_hex,
            issue_id,
            patch_id,
            pushed_commit: None,
        })
    }

    /// Start the independent source and explicitly connect it to every validator.
    pub fn start_user_radicle(&mut self, fixture: &RadicleRepositoryFixtureV1) -> Result<()> {
        if self
            .user_radicle
            .as_mut()
            .is_some_and(|guard| !guard.exited())
        {
            return Ok(());
        }
        self.user_radicle = None;
        self.user_radicle = Some(self.spawn_user_radicle(&fixture.home)?);

        self.rad(
            &fixture.home,
            &self.cfg.user_radicle_control_socket(),
            Some(&fixture.worktree),
            &["seed", &fixture.repo_id, "--scope", "all", "--no-fetch"],
        )?;
        for index in 0..self.committee_size() {
            for address in self.validator_radicle_addresses(index)? {
                self.rad(
                    &fixture.home,
                    &self.cfg.user_radicle_control_socket(),
                    None,
                    &["node", "connect", &address, "--timeout", "5s"],
                )?;
            }
        }
        Ok(())
    }

    fn spawn_user_radicle(&self, home: &Path) -> Result<ChildGuard> {
        let slot = self.committee_size() + 32;
        let listen = format!("127.0.0.1:{}", self.cfg.radicle_port(slot));
        let status = format!("127.0.0.1:{}", self.cfg.radicle_status_port(slot));
        let script = self.cfg.repo.join("scripts/run-radicle.sh");
        let max_validators = materialized_max_validators(&self.cfg.dir.join("genesis.json"))?;
        let control_socket = self.cfg.user_radicle_control_socket();
        prepare_private_socket_parent(&self.cfg.radicle_runtime_root, &control_socket, home)?;
        let mut command = Command::new(&script);
        command
            .env("OUTBE_RADICLE_BINARY", &self.cfg.bin_radicle)
            .env("OUTBE_RADICLE_CONTROL_SOCKET", &control_socket)
            .args(args![
                home.display(),
                listen,
                status,
                max_validators,
                format!("127.0.0.1:{}", self.cfg.radicle_port(slot)),
            ]);
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.cfg.dir.join("user-radicle.log"))?;
        command
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .stdin(Stdio::null());
        let mut guard = ChildGuard::spawn("radicle-user", command)?;
        if !wait_tcp(self.cfg.radicle_status_port(slot), 100) {
            guard.stop();
            bail!("independent user Radicle status endpoint never came up");
        }
        Ok(guard)
    }

    /// Publish one ordinary Git update after the source node is online.
    pub fn push_user_radicle_update(&self, fixture: &mut RadicleRepositoryFixtureV1) -> Result<()> {
        fs::write(
            fixture.worktree.join("README.md"),
            "# Outbe Radicle E2E\n\nReplicated through the validator mesh.\n",
        )?;
        self.git(&fixture.worktree, &["add", "README.md"])?;
        self.git(
            &fixture.worktree,
            &["commit", "-m", "Publish validator mesh evidence"],
        )?;
        self.git_env(
            &fixture.home,
            &self.cfg.user_radicle_control_socket(),
            &fixture.worktree,
            &["push", "rad", "HEAD:refs/heads/master"],
        )?;
        fixture.pushed_commit = Some(
            self.git(&fixture.worktree, &["rev-parse", "HEAD"])?
                .trim()
                .to_owned(),
        );
        Ok(())
    }

    pub fn radicle_repo_visible(
        &self,
        validator: usize,
        fixture: &RadicleRepositoryFixtureV1,
    ) -> Result<bool> {
        let home = self.cfg.validator_dir(validator).join("radicle");
        let socket = self.radicle_control_socket(validator);
        let issue = self.rad(
            &home,
            &socket,
            None,
            &[
                "issue",
                "show",
                &fixture.issue_id,
                "--repo",
                &fixture.repo_id,
            ],
        );
        let patch = self.rad(
            &home,
            &socket,
            None,
            &[
                "patch",
                "show",
                &fixture.patch_id,
                "--repo",
                &fixture.repo_id,
            ],
        );
        let remote = fixture.repo_id.replacen("rad:", "rad://", 1);
        let pushed = fixture.pushed_commit.as_ref().is_none_or(|expected| {
            let mut command = Command::new("git");
            command
                .current_dir(&self.cfg.dir)
                .env("RAD_HOME", &home)
                .env("RAD_SOCKET", &socket)
                .env("PATH", self.radicle_path())
                .args(["ls-remote", &remote, "refs/heads/master"]);
            output(command, "git")
                .is_ok_and(|output| output.lines().any(|line| line.starts_with(expected)))
        });
        Ok(issue.is_ok() && patch.is_ok() && pushed)
    }

    pub fn radicle_node_status(&self, index: usize) -> Result<String> {
        let home = self.cfg.validator_dir(index).join("radicle");
        self.rad(
            &home,
            &self.radicle_control_socket(index),
            None,
            &["node", "status"],
        )
    }

    /// Exact connected native Heartwood peers from the UDS-backed status table.
    pub fn radicle_connected_session_node_ids(&self, index: usize) -> Result<Vec<String>> {
        Ok(parse_connected_sessions(&self.radicle_node_status(index)?)
            .into_iter()
            .map(|(node_id, _)| node_id)
            .collect())
    }

    /// Connected native Heartwood address for one exact peer NodeId.
    pub fn radicle_connected_session_address(&self, index: usize, node_id: &str) -> Result<String> {
        parse_connected_sessions(&self.radicle_node_status(index)?)
            .into_iter()
            .find_map(|(observed, address)| (observed == node_id).then_some(address))
            .ok_or_else(|| eyre!("validator-{index} has no connected session for {node_id}"))
    }

    /// Native persistent policy proof that the manager applied Seed Scope::All.
    pub fn radicle_seed_scope_all(&self, index: usize, repo_id: &str) -> Result<bool> {
        let home = self.cfg.validator_dir(index).join("radicle");
        let policies = self.rad(&home, &self.radicle_control_socket(index), None, &["seed"])?;
        Ok(policies.lines().any(|line| {
            let line = strip_ansi(line);
            line.contains(repo_id) && line.contains("allow") && line.contains("all")
        }))
    }

    fn rad(
        &self,
        home: &Path,
        control_socket: &Path,
        cwd: Option<&Path>,
        args: &[&str],
    ) -> Result<String> {
        let mut command = Command::new(&self.cfg.bin_rad);
        command
            .env("RAD_HOME", home)
            .env("RAD_SOCKET", control_socket)
            .env("PATH", self.radicle_path())
            .args(args);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        output(command, "rad")
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Result<String> {
        let mut command = Command::new("git");
        command.current_dir(cwd).args(args);
        output(command, "git")
    }

    fn git_env(
        &self,
        home: &Path,
        control_socket: &Path,
        cwd: &Path,
        args: &[&str],
    ) -> Result<String> {
        let mut command = Command::new("git");
        command
            .current_dir(cwd)
            .env("RAD_HOME", home)
            .env("RAD_SOCKET", control_socket)
            .env("PATH", self.radicle_path())
            .args(args);
        output(command, "git")
    }

    fn radicle_path(&self) -> std::ffi::OsString {
        let helper_dir = self
            .cfg
            .bin_git_remote_rad
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let mut paths = vec![helper_dir.to_path_buf()];
        paths.extend(std::env::split_paths(&self.cfg.path));
        std::env::join_paths(paths).unwrap_or_else(|_| self.cfg.path.clone().into())
    }

    pub(crate) fn cleanup_radicle_runtime(&self) -> Result<()> {
        let target = if self.cfg.scenario == 0 {
            self.cfg.radicle_runtime_root.clone()
        } else {
            self.cfg.radicle_scenario_runtime_dir()
        };
        if !target.starts_with(&self.cfg.radicle_runtime_root) {
            bail!("refusing to clean Radicle runtime path outside its run root");
        }
        let metadata = match fs::symlink_metadata(&target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).wrap_err("inspect Radicle runtime directory"),
        };
        let expected_uid = fs::symlink_metadata(&self.cfg.dir)
            .wrap_err_with(|| format!("inspect localnet directory {}", self.cfg.dir.display()))?
            .uid();
        validate_private_owned_dir(&target, &metadata, expected_uid)?;
        fs::remove_dir_all(&target)
            .wrap_err_with(|| format!("remove Radicle runtime directory {}", target.display()))
    }
}

fn prepare_private_socket_parent(
    runtime_root: &Path,
    socket: &Path,
    owner_anchor: &Path,
) -> Result<()> {
    if socket.as_os_str().as_bytes().len() >= PORTABLE_UNIX_SOCKET_PATH_LIMIT {
        bail!(
            "Radicle control socket path must be shorter than {PORTABLE_UNIX_SOCKET_PATH_LIMIT} bytes: {}",
            socket.display()
        );
    }
    let parent = socket
        .parent()
        .ok_or_else(|| eyre!("Radicle control socket has no parent"))?;
    if !parent.starts_with(runtime_root) {
        bail!("Radicle control socket escaped its run-scoped runtime root");
    }
    let expected_uid = fs::symlink_metadata(owner_anchor)
        .wrap_err_with(|| format!("inspect Radicle owner anchor {}", owner_anchor.display()))?
        .uid();
    ensure_private_owned_dir(runtime_root, expected_uid)?;
    ensure_private_owned_dir(parent, expected_uid)
}

fn ensure_private_owned_dir(path: &Path, expected_uid: u32) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error)
                .wrap_err_with(|| format!("create Radicle runtime directory {}", path.display()));
        }
    }
    let metadata = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("inspect Radicle runtime directory {}", path.display()))?;
    validate_private_owned_dir(path, &metadata, expected_uid)
}

fn validate_private_owned_dir(
    path: &Path,
    metadata: &fs::Metadata,
    expected_uid: u32,
) -> Result<()> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "Radicle runtime path must be a non-symlink directory: {}",
            path.display()
        );
    }
    let mode = metadata.mode() & 0o777;
    if metadata.uid() != expected_uid || mode != 0o700 {
        bail!(
            "Radicle runtime directory {} must be owned by uid {expected_uid} with mode 700 (found uid {}, mode {mode:o})",
            path.display(),
            metadata.uid()
        );
    }
    Ok(())
}

fn output(mut command: Command, label: &str) -> Result<String> {
    let result = command.output().wrap_err_with(|| format!("run {label}"))?;
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    if !result.status.success() {
        bail!("{label} failed: {stderr}{stdout}");
    }
    Ok(format!("{stdout}{stderr}"))
}

fn first_hex40(value: &str) -> Option<String> {
    value
        .split(|character: char| !character.is_ascii_hexdigit())
        .find(|token| token.len() == 40)
        .map(str::to_owned)
}

fn decode_repo_id(value: &str) -> Result<[u8; 20]> {
    let canonical = value.strip_prefix("rad:").unwrap_or(value);
    let (base, bytes) = multibase::decode(canonical).wrap_err("decode canonical Radicle RepoId")?;
    if base != multibase::Base::Base58Btc {
        bail!("Radicle RepoId must use base58btc multibase encoding");
    }
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| eyre!("Radicle RepoId must be 20 bytes, got {}", bytes.len()))
}

fn parse_connected_sessions(output: &str) -> Vec<(String, String)> {
    let mut sessions = output
        .lines()
        .filter_map(|line| {
            let line = strip_ansi(line);
            if !line.contains('│') || !line.contains('✓') {
                return None;
            }
            let columns = line
                .split_whitespace()
                .filter(|column| *column != "│")
                .collect::<Vec<_>>();
            if columns.len() < 4 || !columns[0].starts_with('z') || columns[2] != "✓" {
                return None;
            }
            Some((columns[0].to_owned(), columns[1].to_owned()))
        })
        .collect::<Vec<_>>();
    sessions.sort();
    sessions.dedup();
    sessions
}

fn strip_ansi(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            output.push(character);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for suffix in chars.by_ref() {
            if suffix.is_ascii_alphabetic() {
                break;
            }
        }
    }
    output
}

fn write_outbe_profile(home: &Path, listen: &str, max_validators: usize) -> Result<()> {
    let managed_peers = max_validators.saturating_sub(1);
    let profile = serde_json::json!({
        "preferredSeeds": [],
        "node": {
            "alias": "outbe",
            "listen": [listen],
            "peers": { "type": "static" },
            "connect": [],
            "externalAddresses": [listen],
            "network": "outbe",
            "relay": "always",
            "limits": {
                "connection": {
                    "inbound": managed_peers + 16,
                    "outbound": managed_peers,
                }
            },
            "seedingPolicy": { "default": "block" },
        }
    });
    let path = home.join("config.json");
    fs::write(&path, serde_json::to_vec_pretty(&profile)?)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn materialized_max_validators(genesis_path: &Path) -> Result<usize> {
    let genesis: serde_json::Value = serde_json::from_slice(
        &fs::read(genesis_path)
            .wrap_err_with(|| format!("read materialized genesis {}", genesis_path.display()))?,
    )
    .wrap_err_with(|| format!("decode materialized genesis {}", genesis_path.display()))?;
    let alloc = genesis
        .get("alloc")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| eyre!("materialized genesis has no alloc object"))?;
    let account = alloc
        .get(VALIDATOR_SET_ALLOC_ADDRESS)
        .or_else(|| alloc.get(&format!("0x{VALIDATOR_SET_ALLOC_ADDRESS}")))
        .ok_or_else(|| eyre!("materialized genesis has no ValidatorSet alloc"))?;
    let storage = account
        .get("storage")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| eyre!("materialized ValidatorSet alloc has no storage"))?;
    let encoded = storage
        .get(VALIDATOR_SET_MAX_VALIDATORS_SLOT)
        .or_else(|| storage.get(VALIDATOR_SET_MAX_VALIDATORS_SLOT.trim_start_matches("0x")))
        .ok_or_else(|| eyre!("materialized ValidatorSet has no maxValidators slot"))?;
    let max_validators = encoded
        .as_u64()
        .or_else(|| {
            encoded
                .as_str()
                .and_then(|raw| u64::from_str_radix(raw.trim_start_matches("0x"), 16).ok())
        })
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| eyre!("materialized ValidatorSet maxValidators is not positive"))?;
    Ok(max_validators)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Read as _,
        os::unix::fs::{symlink, MetadataExt as _, PermissionsExt as _},
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        thread::sleep,
        time::Duration,
    };

    use super::{
        decode_repo_id, materialized_max_validators, parse_connected_sessions,
        prepare_private_socket_parent, write_outbe_profile,
    };

    struct KillOnDrop(Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn connected_sessions() {
        let output = concat!(
            "✓ Node is running with Node ID z6Mklocal and listening.\n",
            "│ Node ID       Address          ?   ⤭   Since │\n",
            "│ z6Mkpeer1      127.0.0.1:8776   ✓   ↗   1s    │\n",
            "│ z6Mkpeer2      127.0.0.1:8777   ✗   ↘   2s    │\n",
            "│ \u{1b}[32mz6Mkpeer3\u{1b}[0m      127.0.0.1:8778   \u{1b}[32m✓\u{1b}[0m   ↘   3s    │\n",
        );
        assert_eq!(
            parse_connected_sessions(output),
            vec![
                ("z6Mkpeer1".to_owned(), "127.0.0.1:8776".to_owned()),
                ("z6Mkpeer3".to_owned(), "127.0.0.1:8778".to_owned()),
            ]
        );
    }

    #[test]
    fn canonical_repo_id_decodes_to_registry_bytes() {
        let expected = [0x5a; 20];
        let canonical = format!(
            "rad:{}",
            multibase::encode(multibase::Base::Base58Btc, expected)
        );
        assert_eq!(decode_repo_id(&canonical).unwrap(), expected);
        assert!(decode_repo_id("rad:f0011").is_err());
    }

    #[test]
    fn materialized_genesis_drives_radicle_capacity() {
        let root = tempfile::tempdir().expect("create capacity fixture");
        let genesis = root.path().join("genesis.json");
        fs::write(
            &genesis,
            serde_json::to_vec(&serde_json::json!({
                "alloc": {
                    "000000000000000000000000000000000000ee00": {
                        "storage": {
                            "0x0000000000000000000000000000000000000000000000000000000000000001":
                                "0x0000000000000000000000000000000000000000000000000000000000000080"
                        }
                    }
                }
            }))
            .expect("encode genesis fixture"),
        )
        .expect("write genesis fixture");

        let max_validators = materialized_max_validators(&genesis).expect("read capacity");
        assert_eq!(max_validators, 128);
        write_outbe_profile(root.path(), "127.0.0.1:8776", max_validators).expect("write profile");
        let profile: serde_json::Value = serde_json::from_slice(
            &fs::read(root.path().join("config.json")).expect("read profile"),
        )
        .expect("decode profile");
        assert_eq!(
            profile.pointer("/node/limits/connection/outbound"),
            Some(&serde_json::json!(127))
        );
        assert_eq!(
            profile.pointer("/node/limits/connection/inbound"),
            Some(&serde_json::json!(143))
        );
    }

    #[test]
    fn short_socket_runtime_is_private_and_rejects_symlink_parents() {
        let fixture = tempfile::tempdir().expect("create runtime fixture");
        let runtime = fixture.path().join("runtime");
        let socket = runtime.join("scenario-1/v0.sock");
        prepare_private_socket_parent(&runtime, &socket, fixture.path())
            .expect("prepare private runtime");
        let expected_uid = fs::symlink_metadata(fixture.path())
            .expect("inspect fixture owner")
            .uid();

        for directory in [&runtime, socket.parent().unwrap()] {
            let metadata = fs::symlink_metadata(directory).expect("inspect private runtime");
            assert!(metadata.is_dir());
            assert!(!metadata.file_type().is_symlink());
            assert_eq!(metadata.uid(), expected_uid);
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }

        let external = fixture.path().join("external");
        fs::create_dir(&external).expect("create external directory");
        let linked_runtime = fixture.path().join("linked-runtime");
        symlink(&external, &linked_runtime).expect("link runtime directory");
        let linked_socket = linked_runtime.join("scenario-1/v0.sock");
        assert!(
            prepare_private_socket_parent(&linked_runtime, &linked_socket, fixture.path()).is_err()
        );
    }

    #[test]
    fn launcher_rejects_second_home_owner() {
        let fixture = launcher_fixture();
        let mut first = KillOnDrop(
            launcher_command(&fixture.home, &fixture.binary)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("start first launcher"),
        );
        wait_for_launcher(&mut first.0, &fixture.home);

        let second = launcher_command(&fixture.home, &fixture.binary)
            .output()
            .expect("run competing launcher");
        assert!(!second.status.success());
        assert!(
            String::from_utf8_lossy(&second.stderr).contains("a live sidecar already owns"),
            "unexpected competing-launcher error: {}",
            String::from_utf8_lossy(&second.stderr)
        );
    }

    #[test]
    fn launcher_rejects_managed_directory_symlink() {
        let fixture = launcher_fixture();
        let external = fixture.root.path().join("external-storage");
        fs::create_dir(&external).expect("create external storage");
        fs::set_permissions(&external, fs::Permissions::from_mode(0o700))
            .expect("protect external storage");
        symlink(&external, fixture.home.join("storage")).expect("link managed storage");

        let output = launcher_command(&fixture.home, &fixture.binary)
            .output()
            .expect("run launcher with symlink");
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("expected non-symlink directory"),
            "unexpected symlink error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    struct LauncherFixture {
        root: tempfile::TempDir,
        home: PathBuf,
        binary: PathBuf,
    }

    fn launcher_fixture() -> LauncherFixture {
        let root = tempfile::tempdir().expect("create launcher fixture");
        let home = root.path().join("radicle");
        let keys = home.join("keys");
        fs::create_dir_all(&keys).expect("create key directory");
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).expect("protect home");
        fs::set_permissions(&keys, fs::Permissions::from_mode(0o700)).expect("protect keys");
        for name in ["radicle", "radicle.pub"] {
            let path = keys.join(name);
            fs::write(&path, "fixture\n").expect("write fixture key");
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(if name == "radicle" { 0o600 } else { 0o644 }),
            )
            .expect("set fixture key mode");
        }
        let binary = root.path().join("outbe-radicle-fixture");
        fs::write(
            &binary,
            concat!(
                "#!/usr/bin/env python3\n",
                "import socket\n",
                "import sys\n",
                "path = sys.argv[sys.argv.index('--control-socket') + 1]\n",
                "listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)\n",
                "listener.bind(path)\n",
                "listener.listen()\n",
                "with open(path + '.ready', 'x', encoding='utf-8') as marker:\n",
                "    marker.write('ready\\n')\n",
                "while True:\n",
                "    connection, _ = listener.accept()\n",
                "    connection.close()\n",
            ),
        )
        .expect("write fake sidecar");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .expect("make fake sidecar executable");
        LauncherFixture { root, home, binary }
    }

    fn launcher_command(home: &Path, binary: &Path) -> Command {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/run-radicle.sh");
        let mut command = Command::new(script);
        command
            .env("OUTBE_RADICLE_BINARY", binary)
            .env(
                "OUTBE_RADICLE_CONTROL_SOCKET",
                home.join("node/outbe-control.sock"),
            )
            .args([
                home.as_os_str(),
                "127.0.0.1:18776".as_ref(),
                "127.0.0.1:18777".as_ref(),
                "4".as_ref(),
                "127.0.0.1:18776".as_ref(),
            ]);
        command
    }

    /// Waits until the launcher has taken ownership of the home.
    ///
    /// The managed directories are created just before the script execs the
    /// sidecar, so their presence is the last observable step of setup. It
    /// used to wait on `config.json`, but the launcher no longer writes one -
    /// the sidecar builds its runtime config from its command line and never
    /// reads that file.
    fn wait_for_launcher(child: &mut Child, home: &Path) {
        let control_socket = home.join("node/outbe-control.sock");
        let readiness_marker = control_socket.with_extension("sock.ready");
        for _ in 0..100 {
            if let Some(status) = child.try_wait().expect("poll launcher") {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    pipe.read_to_string(&mut stderr)
                        .expect("read failed launcher stderr");
                }
                panic!("first launcher exited early with {status}: {stderr}");
            }
            if readiness_marker.is_file() {
                return;
            }
            sleep(Duration::from_millis(10));
        }
        panic!(
            "first launcher did not publish readiness for its control socket at {}",
            readiness_marker.display()
        );
    }
}
