//! Automatic post-finality network bootstrap. Only public contributions leave the node provider.
mod binding;
mod contributions;
mod history;
mod job;
mod journal;
mod review;

use commonware_codec::DecodeExt as _;
use commonware_cryptography::{bls12381, Hasher as _, Sha256};
use journal::{put, read, Journal};
use outbe_primitives::{
    radicle_genesis::GovernmentsDescriptor,
    radicle_publication::{
        PublicationAttestation, PublicationBatch, PublicationBody, MAX_PUBLICATION_ATTESTATIONS,
    },
};
use outbe_radicle::{
    integration::{
        bootstrap::{
            self, PreparedBootstrapDocument, PreparedBootstrapIdentity, PreparedBootstrapRefs,
        },
        local::{BootstrapSigningContext, LocalReceiptClient, PatchPreparationContext},
        publication::{
            policy::AcceptedRepositoryPolicy, store::ReviewPackageStore, CheckpointOwnerSnapshot,
            MAX_MANIFEST_BYTES,
        },
        review::{CheckpointPreparedReview, CheckpointPreparedReviewRefs},
        signing::{
            accepted::AcceptedPublicationVerifier, witness::RepositoryWitnessBundle,
            RepositoryPartial, RepositorySigningMaterial,
        },
        ReceivedRepositoryMessage, RepositoryGossipMessage,
    },
    manager::FinalizedPublicationReceipt,
};
use radicle::{
    crypto::{BlsEnvelope, BlsPublicKey, RepositoryKey},
    git,
    identity::RepoId,
    node::Handle as _,
    storage::{git::Repository, ReadStorage as _, WriteRepository as _, WriteStorage as _},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Contributions = Vec<(bls12381::PublicKey, RepositoryPartial)>;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    AwaitingPartials {
        ordinal: u8,
        have: usize,
        required: usize,
    },
    PublicationPending,
    AwaitingAcceptedPackage,
    Announced,
    CatchingUp,
    WaitingForWindow {
        proposal: alloy_primitives::U256,
    },
    ApprovedUnmerged {
        proposal: alloy_primitives::U256,
        reason: &'static str,
    },
}

#[derive(Clone, Copy)]
enum SigningContext<'a> {
    Bootstrap(&'a BootstrapSigningContext),
    Review(&'a PatchPreparationContext),
}
impl SigningContext<'_> {
    fn material(&self) -> &RepositorySigningMaterial {
        match self {
            Self::Bootstrap(c) => &c.material,
            Self::Review(c) => &c.material,
        }
    }
}

enum Job<'a> {
    Document(&'a PreparedBootstrapDocument),
    Identity(&'a PreparedBootstrapIdentity),
    Refs(&'a PreparedBootstrapRefs),
    Review(&'a CheckpointPreparedReview),
    ReviewRefs(&'a CheckpointPreparedReviewRefs, &'a BlsEnvelope),
    Merge(&'a outbe_radicle::integration::merge::CheckpointPreparedMerge),
    MergeRefs(
        &'a outbe_radicle::integration::merge::CheckpointPreparedMergeRefs,
        &'a BlsEnvelope,
    ),
}
struct Collector {
    storage: radicle::Storage,
    journal: Journal,
    handle: radicle_node::runtime::handle::Handle,
    policy: Option<(RepoId, Arc<AcceptedRepositoryPolicy>)>,
    active: Option<[u8; 32]>,
    own: [Option<RepositoryPartial>; 3],
    announced: Option<[u8; 32]>,
    snapshot: Option<CheckpointOwnerSnapshot>,
}

impl Collector {
    fn open(
        storage: radicle::Storage,
        root: PathBuf,
        handle: radicle_node::runtime::handle::Handle,
        client: &LocalReceiptClient,
    ) -> Result<Self> {
        Ok(Self {
            storage,
            journal: Journal::open(root, client.chain_identity())?,
            handle,
            policy: None,
            active: None,
            own: [None, None, None],
            announced: None,
            snapshot: None,
        })
    }

    fn repository(&self, id: RepoId) -> Result<Repository> {
        match self.storage.repository(id) {
            Ok(repo) => Ok(repo),
            Err(error) if error.is_not_found() => Ok(self.storage.create(id)?),
            Err(error) => Err(error.into()),
        }
    }

    async fn step(&mut self, client: &LocalReceiptClient) -> Result<Outcome> {
        if client.chain_identity() != self.journal.chain {
            return Err("collector node changed chain identity".into());
        }
        if let Some(receipt) = client.read(1).await? {
            let result = self.install(client, receipt).await?;
            return match result {
                Outcome::Announced | Outcome::CatchingUp => self.review_step(client).await,
                other => Ok(other),
            };
        }
        self.bootstrap_step(client).await
    }

    async fn bootstrap_step(&mut self, client: &LocalReceiptClient) -> Result<Outcome> {
        let context = client.read_bootstrap_signing_context().await?;
        let document =
            bootstrap::prepare_bootstrap_document(context.state.descriptor(), &context.material)?;
        let directory = self
            .journal
            .select(document.body(), context.state.descriptor())?;
        if self.active != Some(document.body().receipt_hash().0) {
            self.own = [None, None, None];
            self.active = Some(document.body().receipt_hash().0);
        }
        let id = document.repository_id();
        if self.policy.is_none() {
            let policy = Arc::new(AcceptedRepositoryPolicy::pending_bootstrap(&document)?);
            let owner = RepositoryKey::from(BlsPublicKey::from_bytes(
                document.body().owner_bls_public.0,
            )?);
            self.storage.manage_repository(id, owner, policy.clone())?;
            self.policy = Some((id, policy));
        }
        let repo = self.repository(id)?;
        let artifact_path = self.journal.root.join("initial-content.manifest");
        let content = initial_content(&repo, &artifact_path, context.state.descriptor())?;
        let artifact = repo.raw().blob(&content)?.into();
        let incoming = client.take_repository_messages(&context.material).await?;
        let n = context.state.authority().committee().committee.len();
        let required = n - (n - 1) / 3;
        let documents = self
            .collect(
                &Job::Document(&document),
                (
                    SigningContext::Bootstrap(&context),
                    client,
                    &repo,
                    Some(artifact),
                ),
                &directory,
                &incoming,
            )
            .await?;
        if documents.len() < required {
            return Ok(Outcome::AwaitingPartials {
                ordinal: 0,
                have: documents.len(),
                required,
            });
        }
        let signature =
            Job::Document(&document).recover(SigningContext::Bootstrap(&context), &documents)?;
        let identity = document.attach(&repo, &signature, &content)?;
        let identities = self
            .collect(
                &Job::Identity(&identity),
                (
                    SigningContext::Bootstrap(&context),
                    client,
                    &repo,
                    Some(artifact),
                ),
                &directory,
                &incoming,
            )
            .await?;
        if identities.len() < required {
            return Ok(Outcome::AwaitingPartials {
                ordinal: 1,
                have: identities.len(),
                required,
            });
        }
        let signature =
            Job::Identity(&identity).recover(SigningContext::Bootstrap(&context), &identities)?;
        let refs = identity.attach(&repo, &signature)?;
        let references = self
            .collect(
                &Job::Refs(&refs),
                (
                    SigningContext::Bootstrap(&context),
                    client,
                    &repo,
                    Some(artifact),
                ),
                &directory,
                &incoming,
            )
            .await?;
        if references.len() < required {
            return Ok(Outcome::AwaitingPartials {
                ordinal: 2,
                have: references.len(),
                required,
            });
        }
        let signature =
            Job::Refs(&refs).recover(SigningContext::Bootstrap(&context), &references)?;
        let (body, manifest) = refs.attach(&repo, &signature)?;
        let witness = RepositoryWitnessBundle::new(
            context.material.canonical_output(),
            [documents, identities, references]
                .into_iter()
                .map(|frames| frames.into_iter().map(|(_, frame)| frame).collect())
                .collect(),
        )?;
        let package = Arc::new(bootstrap::verify_bootstrap_publication(
            &repo,
            bootstrap::BootstrapPublicationInput {
                state: &context.state,
                material: &context.material,
                content_manifest: &content,
            },
            &body,
            &manifest,
            &witness,
        )?);
        self.journal
            .packages
            .persist_and_select_bootstrap(&repo, package.clone(), &witness)?;
        let own = client
            .attest_bootstrap_publication(&context, &repo, artifact, &package, &witness)
            .await?;
        let own_message = RepositoryGossipMessage::Attestation {
            body: body.clone(),
            attestation: own.clone(),
        };
        let _ = client
            .gossip_repository_message(&context.material, &own_message)
            .await;
        let attestations =
            self.collect_attestations(&context.material, &directory, &body, &incoming, &own)?;
        let progress = client.read_publication_progress(&body).await?;
        if let Some(receipt) = progress.receipt() {
            return self.install(client, receipt.clone()).await;
        }
        let remaining = attestations
            .into_iter()
            .filter(|(index, _)| !progress.attesters().contains(index))
            .map(|(_, attestation)| attestation)
            .take(MAX_PUBLICATION_ATTESTATIONS)
            .collect::<Vec<_>>();
        if !remaining.is_empty() {
            client
                .queue_bootstrap_publication(
                    &repo,
                    artifact,
                    &package,
                    &witness,
                    &PublicationBatch {
                        body,
                        attestations: remaining,
                    },
                )
                .await?;
        }
        Ok(Outcome::PublicationPending)
    }

    async fn install(
        &mut self,
        client: &LocalReceiptClient,
        receipt: FinalizedPublicationReceipt,
    ) -> Result<Outcome> {
        if self.snapshot.is_some() {
            return Ok(Outcome::Announced);
        }
        let id = git::Oid::from_sha1(receipt.receipt().body.repo_id.0).into();
        let owner = RepositoryKey::from(BlsPublicKey::from_bytes(
            receipt.receipt().body.owner_bls_public.0,
        )?);
        let policy = match &self.policy {
            Some((repository, policy)) if *repository == id => policy.clone(),
            Some(_) => return Err("accepted bootstrap repository changed".into()),
            None => Arc::new(AcceptedRepositoryPolicy::pending_receipt(&receipt)?),
        };
        self.storage.manage_repository(id, owner, policy.clone())?;
        self.policy = Some((id, policy.clone()));
        let repo = self.repository(id)?;
        self.recover_history(client, &receipt, &repo, &policy)
            .await?;
        let Some(package) = self.journal.packages.load_accepted(&receipt, &repo)? else {
            return Ok(Outcome::AwaitingAcceptedPackage);
        };
        if package.body != receipt.receipt().body {
            return Err("accepted bootstrap differs from persisted selection".into());
        }
        let descriptor = self.journal.packages.load_accepted_descriptor(&receipt)?;
        let verifier =
            AcceptedPublicationVerifier::from_receipt(receipt.clone(), package.manifest.clone())?;
        let proof = bootstrap::verify_accepted_bootstrap(&repo, &descriptor, &verifier)?;
        let snapshot = CheckpointOwnerSnapshot::bootstrap(receipt.clone(), &package.manifest)?;
        policy.accept_bootstrap(&proof)?;
        client
            .cache_checkpoint_manifest(&receipt, &package.manifest)
            .await?;
        self.snapshot = Some(snapshot);
        if client.read(2).await?.is_some() {
            return Ok(Outcome::CatchingUp);
        }
        proof.install(&repo)?;
        put(
            &self.journal.root.join("installed"),
            receipt.receipt().receipt_hash.as_slice(),
            false,
        )?;
        self.handle.seed(id, radicle::node::policy::Scope::All)?;
        self.handle.add_inventory(id)?;
        self.handle.announce_refs_for(id, [owner])?;
        self.handle.announce_inventory()?;
        put(
            &self.journal.root.join("announced"),
            receipt.receipt().receipt_hash.as_slice(),
            false,
        )?;
        self.announced = Some(receipt.receipt().receipt_hash.0);
        Ok(Outcome::Announced)
    }
}

fn initial_content(
    repo: &Repository,
    artifact_path: &Path,
    descriptor: &GovernmentsDescriptor,
) -> Result<Vec<u8>> {
    match read(artifact_path, MAX_MANIFEST_BYTES)? {
        Some(bytes) => Ok(bytes),
        None if descriptor.initial_content_manifest_sha256.0 == Sha256::hash(&[b"[]"]).0 => {
            let tree = repo.raw().treebuilder(None)?.write()?;
            if tree.as_bytes() != descriptor.initial_tree_oid.as_slice() {
                return Err("initial content tree is unavailable".into());
            }
            put(artifact_path, b"[]", false)?;
            Ok(b"[]".to_vec())
        }
        None => Err("initial content manifest is unavailable".into()),
    }
}

fn participant(material: &RepositorySigningMaterial, index: usize) -> Result<bls12381::PublicKey> {
    Ok(bls12381::PublicKey::decode(
        material
            .authority()
            .committee()
            .committee
            .get(index)
            .ok_or("collector participant out of bounds")?
            .consensus_pubkey
            .as_slice(),
    )?)
}

pub(crate) struct Worker {
    stop: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Worker {
    pub fn start(
        control: PathBuf,
        storage: radicle::Storage,
        root: PathBuf,
        handle: radicle_node::runtime::handle::Handle,
    ) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let thread = std::thread::Builder::new()
            .name("governments-collector".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let mut collector = None;
                    let mut last = String::new();
                    loop {
                        let operation = async {
                            let client = LocalReceiptClient::connect(&control).await?;
                            if collector.is_none() {
                                collector = Some(Collector::open(
                                    storage.clone(), root.clone(), handle.clone(), &client,
                                )?);
                            }
                            collector.as_mut().ok_or("collector unavailable")?.step(&client).await
                        };
                        let result = tokio::select! {
                            _ = stopping.changed() => break,
                            result = operation => result,
                        };
                        let status = match &result {
                            Ok(outcome) => format!("{outcome:?}"),
                            Err(error) => error.to_string(),
                        };
                        if status != last {
                            use outbe_radicle::integration::local::Error;
                            match &result {
                                Ok(_) => log::info!(target: "outbe-radicle", "governments collector: {status}"),
                                Err(error) if error.downcast_ref::<Error>().is_some_and(|e| matches!(e, Error::Unavailable | Error::Io(_))) => {
                                    log::debug!(target: "outbe-radicle", "governments collector: {status}");
                                }
                                Err(_) => log::warn!(target: "outbe-radicle", "governments collector: {status}"),
                            }
                            last = status;
                        }
                        tokio::select! {
                            _ = stopping.changed() => break,
                            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                        }
                    }
                })
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
