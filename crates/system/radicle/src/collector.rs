//! Automatic post-finality network bootstrap. Only public contributions leave the node provider.
mod journal;

use commonware_codec::DecodeExt as _;
use commonware_cryptography::{bls12381, Hasher as _, Sha256};
use journal::{put, read, Journal};
use outbe_primitives::{
    radicle_genesis::{GovernmentsDescriptor, MAX_GOVERNMENTS_DESCRIPTOR_BYTES},
    radicle_publication::{
        PublicationAttestation, PublicationBatch, PublicationBody, MAX_PUBLICATION_ATTESTATIONS,
    },
};
use outbe_radicle::{
    integration::{
        bootstrap::{
            self, PreparedBootstrapDocument, PreparedBootstrapIdentity, PreparedBootstrapRefs,
        },
        local::{BootstrapSigningContext, LocalReceiptClient},
        publication::{
            policy::AcceptedRepositoryPolicy,
            store::{review_slot_key, ReviewPackageStore},
            CheckpointOwnerSnapshot, MAX_MANIFEST_BYTES,
        },
        signing::{
            accepted::AcceptedPublicationVerifier, witness::RepositoryWitnessBundle,
            RepositoryPartial,
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
}

enum Job<'a> {
    Document(&'a PreparedBootstrapDocument),
    Identity(&'a PreparedBootstrapIdentity),
    Refs(&'a PreparedBootstrapRefs),
}
impl Job<'_> {
    fn ordinal(&self) -> u8 {
        match self {
            Self::Document(_) => 0,
            Self::Identity(_) => 1,
            Self::Refs(_) => 2,
        }
    }
    fn verify(
        &self,
        context: &BootstrapSigningContext,
        sender: &bls12381::PublicKey,
        frame: &RepositoryPartial,
    ) -> Result<()> {
        match self {
            Self::Document(job) => context.material.verify_bootstrap_document_partial(
                job,
                &context.state,
                sender,
                frame,
            )?,
            Self::Identity(job) => context.material.verify_bootstrap_identity_partial(
                job,
                &context.state,
                sender,
                frame,
            )?,
            Self::Refs(job) => context.material.verify_bootstrap_refs_partial(
                job,
                &context.state,
                sender,
                frame,
            )?,
        }
        Ok(())
    }
    fn recover(
        &self,
        context: &BootstrapSigningContext,
        frames: &Contributions,
    ) -> Result<BlsEnvelope> {
        Ok(match self {
            Self::Document(job) => {
                context
                    .material
                    .recover_bootstrap_document(job, &context.state, frames)?
            }
            Self::Identity(job) => {
                context
                    .material
                    .recover_bootstrap_identity(job, &context.state, frames)?
            }
            Self::Refs(job) => {
                context
                    .material
                    .recover_bootstrap_refs(job, &context.state, frames)?
            }
        })
    }
    async fn sign(
        &self,
        client: &LocalReceiptClient,
        context: &BootstrapSigningContext,
        repo: &Repository,
        artifact: git::Oid,
    ) -> Result<RepositoryPartial> {
        Ok(match self {
            Self::Document(_) => client.sign_bootstrap_document(context).await?,
            Self::Identity(job) => {
                client
                    .sign_bootstrap_identity(context, job, repo, artifact)
                    .await?
            }
            Self::Refs(job) => {
                client
                    .sign_bootstrap_refs(context, job, repo, artifact)
                    .await?
            }
        })
    }
}

struct Collector {
    storage: radicle::Storage,
    journal: Journal,
    handle: radicle_node::runtime::handle::Handle,
    policy: Option<(RepoId, Arc<AcceptedRepositoryPolicy>)>,
    active: Option<[u8; 32]>,
    own: [Option<RepositoryPartial>; 3],
    announced: Option<[u8; 32]>,
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
            return self.install(client, receipt).await;
        }
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
        let descriptor = context.state.descriptor();
        let artifact_path = self.journal.root.join("initial-content.manifest");
        let content = match read(&artifact_path, MAX_MANIFEST_BYTES)? {
            Some(bytes) => bytes,
            None if descriptor.initial_content_manifest_sha256.0 == Sha256::hash(&[b"[]"]).0 => {
                let tree = repo.raw().treebuilder(None)?.write()?;
                if tree.as_bytes() != descriptor.initial_tree_oid.as_slice() {
                    return Err("initial content tree is unavailable".into());
                }
                put(&artifact_path, b"[]", false)?;
                b"[]".to_vec()
            }
            None => return Err("initial content manifest is unavailable".into()),
        };
        let artifact = repo.raw().blob(&content)?.into();
        let incoming = client.take_repository_messages(&context.material).await?;
        let n = context.state.authority().committee().committee.len();
        let required = n - (n - 1) / 3;
        let documents = self
            .collect(
                &Job::Document(&document),
                (&context, client, &repo, artifact),
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
        let signature = Job::Document(&document).recover(&context, &documents)?;
        let identity = document.attach(&repo, &signature, &content)?;
        let identities = self
            .collect(
                &Job::Identity(&identity),
                (&context, client, &repo, artifact),
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
        let signature = Job::Identity(&identity).recover(&context, &identities)?;
        let refs = identity.attach(&repo, &signature)?;
        let references = self
            .collect(
                &Job::Refs(&refs),
                (&context, client, &repo, artifact),
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
        let signature = Job::Refs(&refs).recover(&context, &references)?;
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
            &context.state,
            &context.material,
            &content,
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
        let mut attestations = BTreeMap::new();
        for index in 0..n {
            if let Some(bytes) = read(&directory.join(format!("attestation-{index}")), 578)? {
                let batch = PublicationBatch::from_bytes(&bytes)?;
                if batch.body != body
                    || batch.attestations.len() != 1
                    || batch.attestations[0].participant_index as usize != index
                {
                    return Err("corrupt collector attestation".into());
                }
                context
                    .material
                    .verify_publication_attestation(&body, &batch.attestations[0])?;
                attestations.insert(index as u32, batch.attestations[0].clone());
            }
        }
        for received in &incoming {
            if let RepositoryGossipMessage::Attestation {
                body: candidate,
                attestation,
            } = &received.message
            {
                if candidate == &body
                    && context
                        .material
                        .verify_publication_attestation(&body, attestation)
                        .is_ok()
                {
                    self.record_attestation(&directory, &body, attestation)?;
                    attestations.insert(attestation.participant_index, attestation.clone());
                }
            }
        }
        self.record_attestation(&directory, &body, &own)?;
        attestations.insert(own.participant_index, own);
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

    async fn collect(
        &mut self,
        job: &Job<'_>,
        inputs: (
            &BootstrapSigningContext,
            &LocalReceiptClient,
            &Repository,
            git::Oid,
        ),
        directory: &Path,
        incoming: &[ReceivedRepositoryMessage],
    ) -> Result<Contributions> {
        let (context, client, repo, artifact) = inputs;
        let ordinal = job.ordinal();
        let n = context.state.authority().committee().committee.len();
        let mut frames = BTreeMap::new();
        for index in 0..n {
            if let Some(bytes) = read(&directory.join(format!("partial-{ordinal}-{index}")), 268)? {
                let frame = RepositoryPartial::from_bytes(&bytes)?;
                if frame.participant_index() as usize != index {
                    return Err("corrupt collector participant index".into());
                }
                let sender = participant(context, index)?;
                job.verify(context, &sender, &frame)?;
                frames.insert(index as u32, frame);
            }
        }
        for received in incoming {
            if let RepositoryGossipMessage::Partial {
                ordinal: step,
                frame,
            } = &received.message
            {
                if *step == ordinal && job.verify(context, &received.sender, frame).is_ok() {
                    put(
                        &directory.join(format!("partial-{ordinal}-{}", frame.participant_index())),
                        &frame.to_bytes(),
                        false,
                    )?;
                    frames.insert(frame.participant_index(), frame.clone());
                }
            }
        }
        if self.own[ordinal as usize].is_none() {
            if let Ok(frame) = job.sign(client, context, repo, artifact).await {
                job.verify(
                    context,
                    &participant(context, frame.participant_index() as usize)?,
                    &frame,
                )?;
                put(
                    &directory.join(format!("partial-{ordinal}-{}", frame.participant_index())),
                    &frame.to_bytes(),
                    false,
                )?;
                self.own[ordinal as usize] = Some(frame);
            }
        }
        if let Some(frame) = &self.own[ordinal as usize] {
            let _ = client
                .gossip_repository_message(
                    &context.material,
                    &RepositoryGossipMessage::Partial {
                        ordinal,
                        frame: frame.clone(),
                    },
                )
                .await;
            frames.insert(frame.participant_index(), frame.clone());
        }
        frames
            .into_iter()
            .map(|(index, frame)| Ok((participant(context, index as usize)?, frame)))
            .collect()
    }

    fn record_attestation(
        &self,
        directory: &Path,
        body: &PublicationBody,
        attestation: &PublicationAttestation,
    ) -> Result<()> {
        put(
            &directory.join(format!("attestation-{}", attestation.participant_index)),
            &PublicationBatch {
                body: body.clone(),
                attestations: vec![attestation.clone()],
            }
            .to_bytes()?,
            false,
        )
    }

    async fn install(
        &mut self,
        client: &LocalReceiptClient,
        receipt: FinalizedPublicationReceipt,
    ) -> Result<Outcome> {
        if self.announced == Some(receipt.receipt().receipt_hash.0) {
            return Ok(Outcome::Announced);
        }
        let id = git::Oid::from_sha1(receipt.receipt().body.repo_id.0).into();
        let repo = self.repository(id)?;
        let Some(package) = self
            .journal
            .packages
            .load_selected(review_slot_key(&receipt.receipt().body), &repo)?
        else {
            return Ok(Outcome::AwaitingAcceptedPackage);
        };
        if package.body != receipt.receipt().body {
            return Err("accepted bootstrap differs from persisted selection".into());
        }
        let descriptor = GovernmentsDescriptor::from_bytes(
            &read(
                &self.journal.root.join("descriptor"),
                MAX_GOVERNMENTS_DESCRIPTOR_BYTES,
            )?
            .ok_or("bootstrap descriptor unavailable")?,
        )?;
        let content = read(
            &self.journal.root.join("initial-content.manifest"),
            MAX_MANIFEST_BYTES,
        )?
        .ok_or("initial content manifest unavailable")?;
        let verifier = AcceptedPublicationVerifier::from_receipt(
            receipt.clone(),
            package.manifest.clone(),
            package.witness.public_output(),
        )?;
        let proof = bootstrap::verify_accepted_bootstrap(
            &repo,
            &descriptor,
            &content,
            &verifier,
            &package.witness,
        )?;
        let snapshot = CheckpointOwnerSnapshot::bootstrap(receipt.clone(), &package.manifest)?;
        let policy = match &self.policy {
            Some((repository, policy)) if *repository == id => policy.clone(),
            Some(_) => return Err("accepted bootstrap repository changed".into()),
            None => Arc::new(AcceptedRepositoryPolicy::new(&snapshot)?),
        };
        policy.accept_bootstrap(&proof)?;
        let owner = RepositoryKey::from(BlsPublicKey::from_bytes(package.body.owner_bls_public.0)?);
        self.storage.manage_repository(id, owner, policy.clone())?;
        self.policy = Some((id, policy));
        client
            .cache_checkpoint_manifest(&receipt, &package.manifest)
            .await?;
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

fn participant(context: &BootstrapSigningContext, index: usize) -> Result<bls12381::PublicKey> {
    Ok(bls12381::PublicKey::decode(
        context
            .state
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
