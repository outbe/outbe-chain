//! The independent user Radicle node: publication, seeding, and visibility.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eyre::{bail, ensure, Result, WrapErr};
use outbe_radicle::integration::query_sidecar;
use outbe_radicle::manager::{ControlSession, HeartwoodControl, NativeHeartwoodControl};
use radicle::cob::store::access::ReadOnly;
use radicle::node::policy::{Scope, SeedingPolicy};
use radicle::storage::ReadRepository as _;

use super::readiness::{native_address, native_node_id};
use super::{
    decode_repo_id, RadicleRepositoryFixtureV1, NATIVE_CONTROL_DEADLINE, RADICLE_READY_POLL,
    RADICLE_SOURCE_DISCOVERY_TIMEOUT,
};
use crate::internal::eth::block_on;

pub(super) fn native_sessions(socket: PathBuf) -> Result<Vec<ControlSession>> {
    block_on(async move {
        NativeHeartwoodControl::new(socket, NATIVE_CONTROL_DEADLINE)
            .sessions()
            .await
    })
    .wrap_err("query native Radicle sessions")
}

pub(super) fn configure_user_radicle(
    socket: PathBuf,
    repo: [u8; 20],
    peers: Vec<PathBuf>,
) -> Result<()> {
    ensure!(!peers.is_empty(), "source setup requires validator peers");
    let control_socket = socket.clone();
    let expected = block_on(async move {
        let control = NativeHeartwoodControl::new(control_socket, NATIVE_CONTROL_DEADLINE);
        control
            .seed(repo.into())
            .await
            .wrap_err("seed independent user repository through native control")?;
        let mut expected = Vec::with_capacity(peers.len());
        for (index, socket) in peers.into_iter().enumerate() {
            let info = query_sidecar(socket, NATIVE_CONTROL_DEADLINE)
                .await
                .wrap_err_with(|| format!("query validator-{index} before user connection"))?;
            ensure!(
                !expected.contains(&info.node_id),
                "duplicate validator identity in source discovery cohort"
            );
            control
                .connect(info.node_id, &info.addresses)
                .await
                .wrap_err_with(|| {
                    format!("connect user to validator-{index} through native control")
                })?;
            expected.push(info.node_id);
        }
        Ok::<_, eyre::Report>(expected)
    })?;
    let rid = format!(
        "rad:{}",
        multibase::encode(multibase::Base::Base58Btc, repo)
    )
    .parse::<radicle::identity::RepoId>()
    .wrap_err("parse source inventory RepoId")?;
    // Native Seed changes policy, but unlike `rad seed` does not publish a
    // locally held public repository. An unchanged inventory is idempotent,
    // not a failure: the caller already verified the local public identity.
    let publication = source_control_response::<InventoryPublication>(
        &socket,
        radicle::node::Command::AddInventory { rid },
        Instant::now() + NATIVE_CONTROL_DEADLINE,
    )
    .wrap_err("publish independent source inventory")?;
    eprintln!(
        "[e2e] independent source inventory published rid={rid} updated={}",
        publication.updated
    );
    wait_for_source_seeds(&socket, rid, &expected, RADICLE_SOURCE_DISCOVERY_TIMEOUT)
}

/// Keep the pinned native protocol, but bound the entire exchange rather than
/// the SDK's individual reads. Cancellation drops the socket. No blocking I/O
/// or detached task survives a timed-out control operation.
pub(super) fn source_control_response<T: serde::de::DeserializeOwned + Send + 'static>(
    socket: &Path,
    command: radicle::node::Command,
    deadline: Instant,
) -> Result<T> {
    let socket = socket.to_path_buf();
    block_on(async move {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
        ensure!(
            Instant::now() < deadline,
            "source control deadline exceeded"
        );
        let response = tokio::time::timeout_at(deadline.into(), async {
            let mut stream = tokio::net::UnixStream::connect(&socket)
                .await
                .wrap_err("connect source control socket")?;
            let mut request = Vec::new();
            command.to_writer(&mut request)?;
            stream
                .write_all(&request)
                .await
                .wrap_err("write source control command")?;
            let mut response = Vec::new();
            BufReader::new(stream)
                .read_until(b'\n', &mut response)
                .await
                .wrap_err("read source control response")?;
            ensure!(
                response.last() == Some(&b'\n'),
                "source control response incomplete"
            );
            Ok::<_, eyre::Report>(response)
        })
        .await
        .wrap_err("source control deadline exceeded")??;
        let decoded = serde_json::from_slice::<radicle::node::CommandResult<T>>(&response)
            .wrap_err("decode native source control response")?;
        // Tokio may poll a ready operation before an already elapsed timer.
        // Completion (including decoding) must still meet the original deadline.
        ensure!(
            Instant::now() < deadline,
            "source control deadline exceeded"
        );
        match decoded {
            radicle::node::CommandResult::Okay(value) => Ok(value),
            radicle::node::CommandResult::Error { reason } => {
                bail!("source control: {reason}");
            }
        }
    })
}

// The native untagged CommandResult tries its success variant first. Reject
// unknown fields here so an {"error": ...} cannot deserialize as updated=false.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InventoryPublication {
    #[serde(default)]
    updated: bool,
}

pub(super) fn require_public_source_repository(fixture: &RadicleRepositoryFixtureV1) -> Result<()> {
    decode_repo_id(&fixture.repo_id)?;
    let rid = fixture
        .repo_id
        .parse()
        .wrap_err("parse public source RepoId")?;
    let path = fixture
        .home
        .join("storage")
        .join(fixture.repo_id.trim_start_matches("rad:"));
    let repository = radicle::storage::git::Repository::open(&path, rid)
        .wrap_err("open existing independent source repository")?;
    ensure!(
        repository.identity_doc()?.is_public(),
        "independent source inventory must contain a public repository"
    );
    Ok(())
}

pub(super) fn wait_for_source_seeds(
    socket: &Path,
    rid: radicle::identity::RepoId,
    expected: &[[u8; 32]],
    timeout: Duration,
) -> Result<()> {
    ensure!(
        !expected.is_empty(),
        "source discovery requires validator peers"
    );
    let deadline = Instant::now() + timeout;
    let mut missing = expected
        .iter()
        .copied()
        .map(native_node_id)
        .collect::<Vec<_>>();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(
            !remaining.is_zero(),
            "source repository {rid} discovery deadline; missing connected validator seeds: {missing:?}"
        );
        // Discovery observes routing and connected identities, not synchronization
        // of the future update. The subsequent ordinary Git push must announce
        // that update itself. There is no forced refs announcement or validator
        // fetch.
        let seeds = source_control_response::<radicle::node::Seeds>(
            socket,
            radicle::node::Command::SeedsFor {
                rid,
                namespaces: Default::default(),
            },
            deadline.min(Instant::now() + NATIVE_CONTROL_DEADLINE),
        )
        .wrap_err("observe source repository seeds")?;
        ensure!(
            Instant::now() < deadline,
            "source discovery deadline exceeded"
        );
        missing = expected
            .iter()
            .copied()
            .filter(|peer| !seeds.is_connected(&radicle::crypto::PublicKey::from(*peer)))
            .map(native_node_id)
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        std::thread::sleep(
            RADICLE_READY_POLL.min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

pub(super) fn connected_sessions(sessions: Vec<ControlSession>) -> Result<Vec<(String, String)>> {
    let mut sessions = sessions
        .into_iter()
        .filter(|session| session.connected)
        .map(|session| {
            Ok((
                native_node_id(session.node_id),
                native_address(&session.address)?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    sessions.sort();
    sessions.dedup();
    Ok(sessions)
}

pub(super) fn seed_scope_all(home: &Path, repo_id: &str) -> Result<bool> {
    let repo_id: radicle::identity::RepoId = repo_id.parse().wrap_err("parse seed RepoId")?;
    let store = radicle::node::policy::store::StoreReader::reader(
        home.join("node").join(radicle::node::POLICIES_DB_FILE),
    )
    .wrap_err("open read-only native seeding policy store")?;
    // Iterate with error propagation: missing policy is pending, failed storage
    // observation is an error. Never seed a validator to make this assertion pass.
    for policy in store
        .seed_policies()
        .wrap_err("read native seeding policies")?
    {
        let policy = policy.wrap_err("decode native seeding policy")?;
        if policy.rid == repo_id {
            return Ok(matches!(
                policy.policy,
                SeedingPolicy::Allow { scope: Scope::All }
            ));
        }
    }
    Ok(false)
}

pub(super) fn repository_visible(
    home: &Path,
    fixture: &RadicleRepositoryFixtureV1,
) -> Result<bool> {
    let repo_id: radicle::identity::RepoId =
        fixture.repo_id.parse().wrap_err("parse repository ID")?;
    // Parse before constructing the storage path. Do not load a Profile or
    // access a key.
    decode_repo_id(&fixture.repo_id)?;
    let path = home
        .join("storage")
        .join(fixture.repo_id.trim_start_matches("rad:"));
    if !path
        .try_exists()
        .wrap_err("inspect replicated repository path")?
    {
        return Ok(false);
    }
    let repository = radicle::storage::git::Repository::open(&path, repo_id)
        .wrap_err("open replicated native repository")?;
    repository
        .identity_doc()
        .wrap_err("read replicated repository identity")?;
    let issue_id = fixture.issue_id.parse().wrap_err("parse issue ID")?;
    let patch_id = fixture.patch_id.parse().wrap_err("parse patch ID")?;
    let issue = radicle::issue::Issues::open(&repository, ReadOnly)
        .wrap_err("open read-only issue store")?
        .get(&issue_id)
        .wrap_err("read replicated issue COB")?;
    let patch = radicle::patch::Patches::open(&repository, ReadOnly)
        .wrap_err("open read-only patch store")?
        .get(&patch_id)
        .wrap_err("read replicated patch COB")?;
    if issue.is_none() || patch.is_none() {
        return Ok(false);
    }
    if let Some(expected) = &fixture.pushed_commit {
        return pushed_commit_visible(&repository, expected);
    }
    Ok(true)
}

pub(super) fn pushed_commit_visible(
    repository: &radicle::storage::git::Repository,
    expected: &str,
) -> Result<bool> {
    let expected = expected.parse().wrap_err("parse pushed commit ID")?;
    let (name, head) = repository.head().wrap_err("read replicated Git head")?;
    if name.as_str() != "refs/heads/master" || head != expected {
        return Ok(false);
    }
    repository
        .commit(expected)
        .wrap_err("read replicated pushed Git commit")?;
    Ok(true)
}
