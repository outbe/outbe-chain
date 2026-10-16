//! Independent ordinary Radicle clients; only patch-author Ed25519 keys are provisioned here.

use super::source::source_control_response;
use super::*;
use crate::world::state::GovernmentsPatchEvidenceV1;
use eyre::ensure;
use outbe_radicle::manager::{HeartwoodControl as _, NativeHeartwoodControl};
use radicle::{
    cob::{self, patch, store::access::WriteAs},
    crypto::Signer as _,
    identity::RepoId,
    storage::{ReadRepository as _, SignRepository as _, WriteRepository as _},
};
use std::time::Instant;

impl Localnet {
    /// Fetch through the real native connection into a separately provisioned, empty client home.
    pub(crate) fn fetch_governments_as_user(&mut self, rid: RepoId, name: &str) -> Result<PathBuf> {
        self.user_radicle = None;
        let home = self.prepare_user_radicle_home(name)?;
        ensure!(
            !home.join("storage").join(rid.canonical()).exists(),
            "reader storage must start empty"
        );
        let guard = self.spawn_user_radicle(&home)?;
        let socket = self.cfg.user_radicle_control_socket();
        let source = block_on(query_sidecar(
            self.radicle_control_socket(0),
            NATIVE_CONTROL_DEADLINE,
        ))?;
        let repo = rid
            .into_sha1()
            .ok_or_else(|| eyre!("SHA1 repository expected"))?;
        let peer = source.node_id;
        let control_socket = socket.clone();
        block_on(async move {
            let control = NativeHeartwoodControl::new(control_socket, NATIVE_CONTROL_DEADLINE);
            control.seed(repo.into()).await?;
            control.connect(peer, &source.addresses).await
        })?;
        let deadline = Instant::now() + RADICLE_SOURCE_DISCOVERY_TIMEOUT;
        loop {
            ensure!(
                Instant::now() < deadline,
                "ordinary governments fetch timed out"
            );
            let result = source_control_response::<radicle::node::FetchResult>(
                &socket,
                radicle::node::Command::Fetch {
                    rid,
                    nid: peer.into(),
                    timeout: Duration::from_secs(30),
                    signed_references_minimum_feature_level: None,
                },
                deadline,
            )?;
            if result.is_success() {
                break;
            }
            std::thread::sleep(RADICLE_READY_POLL);
        }
        configure_user_radicle(
            socket,
            rid.into_sha1()
                .ok_or_else(|| eyre!("SHA1 repository expected"))?,
            (0..self.committee_size())
                .map(|i| self.radicle_control_socket(i))
                .collect(),
        )?;
        self.user_radicle = Some(guard);
        Ok(home)
    }

    pub(crate) fn publish_governments_patch(
        &mut self,
        rid: RepoId,
    ) -> Result<GovernmentsPatchEvidenceV1> {
        let home = self.fetch_governments_as_user(rid, "governments-author")?;
        let author = radicle::crypto::ssh::Keystore::new(&home.join("keys"))
            .secret_key(None)?
            .ok_or_else(|| eyre!("patch author's Ed25519 key missing"))?;
        let repo = radicle::storage::git::Repository::open(
            home.join("storage").join(rid.canonical()),
            rid,
        )?;
        let base = repo.head()?.1;
        let parent = repo.raw().find_commit(base.into())?;
        let mut tree = repo.raw().treebuilder(Some(&parent.tree()?))?;
        let blob = repo
            .raw()
            .blob(b"Approved by the Outbe network through Vote.\n")?;
        tree.insert("GOVERNMENTS-PATCH.md", blob, 0o100644)?;
        let tree = repo.raw().find_tree(tree.write()?)?;
        let metadata =
            radicle::git::raw::Signature::now("Independent patch author", "author@example.test")?;
        let head: radicle::git::Oid = repo
            .raw()
            .commit(
                None,
                &metadata,
                &metadata,
                "Propose governments change\n",
                &tree,
                &[&parent],
            )?
            .into();
        let mut patches = patch::Patches::open(&repo, WriteAs::new(&author))?;
        let mut cache = cob::cache::NoCache;
        let patch = patches.create(
            cob::Title::new("Network governance change")?,
            "Vote must bind this exact revision",
            patch::MergeTarget::default(),
            base,
            head,
            &[],
            &mut cache,
        )?;
        let result = GovernmentsPatchEvidenceV1 {
            author: author.public_key().to_string(),
            patch_id: patch.id().to_string(),
            revision_id: patch.latest().0.to_string(),
            base: base.to_string(),
            head: head.to_string(),
            ..Default::default()
        };
        repo.sign_refs(&author)?;
        source_control_response::<radicle::storage::refs::RefsAt>(
            &self.cfg.user_radicle_control_socket(),
            radicle::node::Command::AnnounceRefsFor {
                rid,
                namespaces: [author.public_key().into()].into(),
            },
            Instant::now() + NATIVE_CONTROL_DEADLINE,
        )?;
        Ok(result)
    }

    pub(crate) fn assert_fresh_governments_validator(&self, rid: RepoId) -> Result<()> {
        let index = self.committee_size();
        let home = self.cfg.validator_dir(index).join("radicle");
        ensure!(
            !home.join("storage").join(rid.canonical()).exists(),
            "new validator already has the governments repository"
        );
        ensure!(
            !home.join("node/outbe-governments").exists(),
            "new validator already has a collector journal"
        );
        let socket = self.radicle_control_socket(index);
        ensure!(
            !outbe_radicle::integration::local::publication_store_path(&socket).exists(),
            "new validator already has SDK publication packages"
        );
        Ok(())
    }
}
