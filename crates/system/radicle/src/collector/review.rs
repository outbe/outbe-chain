use super::*;
use alloy_primitives::U256;
use outbe_coderegistry::schema::{PATCH_EXECUTION_MERGED, PATCH_EXECUTION_REVIEWED};
use outbe_radicle::integration::{
    publication::review::prepare_review_publication, review::prepare_patch_review_from_checkpoint,
};
use std::fs;

mod sync;

enum Prepared {
    Review(Box<CheckpointPreparedReview>),
    Merge(Box<outbe_radicle::integration::merge::CheckpointPreparedMerge>),
}
impl Prepared {
    fn subject(&self) -> &radicle::crypto::RepositorySubject {
        match self {
            Self::Review(j) => j.subject(),
            Self::Merge(j) => j.subject(),
        }
    }
    fn job(&self) -> Job<'_> {
        match self {
            Self::Review(j) => Job::Review(j),
            Self::Merge(j) => Job::Merge(j),
        }
    }
    fn attach(self, repo: &Repository, signature: &BlsEnvelope) -> Result<PreparedRefs> {
        Ok(match self {
            Self::Review(j) => PreparedRefs::Review(Box::new(j.attach(repo, signature)?.1)),
            Self::Merge(j) => PreparedRefs::Merge(Box::new(j.attach(repo, signature)?.1)),
        })
    }
}
enum PreparedRefs {
    Review(Box<CheckpointPreparedReviewRefs>),
    Merge(Box<outbe_radicle::integration::merge::CheckpointPreparedMergeRefs>),
}
impl PreparedRefs {
    fn job<'a>(&'a self, signature: &'a BlsEnvelope) -> Job<'a> {
        match self {
            Self::Review(j) => Job::ReviewRefs(j, signature),
            Self::Merge(j) => Job::MergeRefs(j, signature),
        }
    }
}
enum Package {
    Review(Arc<outbe_radicle::integration::publication::review::VerifiedReviewPublication>),
    Merge(Arc<outbe_radicle::integration::publication::merge::VerifiedMergePublication>),
}
fn retire_approval(path: &Path) -> Result<()> {
    fs::remove_file(path)?;
    fs::File::open(path.parent().ok_or("approval directory missing")?)?.sync_all()?;
    Ok(())
}

impl Collector {
    pub(super) async fn review_step(&mut self, client: &LocalReceiptClient) -> Result<Outcome> {
        if let Some(outcome) = self.pre_review_step(client).await? {
            return Ok(outcome);
        }
        let Some((path, proposal)) = self.next_approval(client).await? else {
            return Ok(Outcome::Announced);
        };
        self.execute_patch_proposal(client, path, proposal).await
    }

    async fn pre_review_step(&mut self, client: &LocalReceiptClient) -> Result<Option<Outcome>> {
        if let Some(outcome) = self.sync_accepted_reviews(client).await? {
            return Ok(Some(outcome));
        }
        self.material_binding_step(client).await
    }

    async fn execute_patch_proposal(
        &mut self,
        client: &LocalReceiptClient,
        path: PathBuf,
        proposal: U256,
    ) -> Result<Outcome> {
        let context = match client.read_patch_preparation_context(proposal).await {
            Ok(context) => context,
            Err(_) => return Ok(Outcome::WaitingForWindow { proposal }),
        };
        if let Some(outcome) = self.check_patch_context_ready(&path, &context)? {
            return Ok(outcome);
        }
        let id = git::Oid::from_sha1(context.state.checkpoint().receipt().body.repo_id.0).into();
        let repo = self.repository(id)?;
        let (accepted_review, job) = match self
            .prepare_patch_job(client, &repo, &path, proposal, &context)
            .await?
        {
            Ok(ready) => ready,
            Err(outcome) => return Ok(outcome),
        };
        let (directory, incoming) = self.prepare_job_journal(client, &context, &job).await?;
        match self
            .collect_patch_witness((client, &repo, &context), job, (&directory, &incoming))
            .await?
        {
            Ok((signatures, witness)) => {
                let package = build_patch_package(
                    &repo,
                    &context,
                    accepted_review.as_ref(),
                    signatures,
                    &witness,
                )?;
                self.attest_and_queue_patch(
                    (client, &repo, &context),
                    (&directory, &incoming),
                    (&witness, package),
                )
                .await
            }
            Err(outcome) => Ok(outcome),
        }
    }

    fn check_patch_context_ready(
        &self,
        path: &Path,
        context: &PatchPreparationContext,
    ) -> Result<Option<Outcome>> {
        if context.state.stage() == PATCH_EXECUTION_MERGED {
            retire_approval(path)?;
            return Ok(Some(Outcome::Announced));
        }
        if self
            .snapshot
            .as_ref()
            .ok_or("accepted snapshot missing")?
            .checkpoint()
            .receipt()
            != context.state.checkpoint().receipt()
        {
            return Ok(Some(Outcome::CatchingUp));
        }
        Ok(None)
    }

    async fn prepare_patch_job(
        &self,
        client: &LocalReceiptClient,
        repo: &Repository,
        path: &Path,
        proposal: U256,
        context: &PatchPreparationContext,
    ) -> Result<
        std::result::Result<
            (
                Option<outbe_radicle::integration::publication::review::AcceptedReviewPublication>,
                Prepared,
            ),
            Outcome,
        >,
    > {
        let signing = context
            .material
            .review_context(context.snapshot.checkpoint().receipt().receipt_hash.0);
        if context.state.stage() != PATCH_EXECUTION_REVIEWED {
            let job = prepare_patch_review_from_checkpoint(
                repo,
                context.state.approval(),
                &context.snapshot,
                &signing,
            )?;
            return Ok(Ok((None, Prepared::Review(Box::new(job)))));
        }
        let review = self
            .accepted_review(
                client,
                repo,
                proposal,
                context.snapshot.checkpoint().receipt().body.sequence,
            )
            .await?;
        match outbe_radicle::integration::merge::prepare_patch_merge_from_checkpoint(
            repo,
            context.state.approval(),
            &context.snapshot,
            &review,
            &signing,
        ) {
            Ok(job) => Ok(Ok((Some(review), Prepared::Merge(Box::new(job))))),
            Err(
                error @ (outbe_radicle::integration::merge::Error::TargetMoved
                | outbe_radicle::integration::merge::Error::Conflict),
            ) => Ok(Err(self.retire_unmerged(path, proposal, &error)?)),
            Err(error) => Err(error.into()),
        }
    }

    fn retire_unmerged(
        &self,
        path: &Path,
        proposal: U256,
        error: &outbe_radicle::integration::merge::Error,
    ) -> Result<Outcome> {
        let reason = if matches!(error, outbe_radicle::integration::merge::Error::TargetMoved) {
            "TargetMoved"
        } else {
            "Conflict"
        };
        let directory = self.journal.root.join("approved-unmerged");
        journal::private_directory(&directory)?;
        put(
            &directory.join(path.file_name().ok_or("approval name missing")?),
            reason.as_bytes(),
            false,
        )?;
        retire_approval(path)?;
        Ok(Outcome::ApprovedUnmerged { proposal, reason })
    }

    async fn prepare_job_journal(
        &mut self,
        client: &LocalReceiptClient,
        context: &PatchPreparationContext,
        job: &Prepared,
    ) -> Result<(PathBuf, Vec<ReceivedRepositoryMessage>)> {
        let key = Sha256::hash(&[job.subject().as_bytes()]).0;
        let directory = self.journal.root.join(hex::encode(key));
        journal::private_directory(&directory)?;
        if self.active != Some(key) {
            self.own = [None, None, None];
            self.active = Some(key);
        }
        let incoming = client.take_repository_messages(&context.material).await?;
        Ok((directory, incoming))
    }

    async fn collect_patch_witness(
        &mut self,
        (client, repo, context): (&LocalReceiptClient, &Repository, &PatchPreparationContext),
        job: Prepared,
        (directory, incoming): (&Path, &[ReceivedRepositoryMessage]),
    ) -> Result<std::result::Result<([BlsEnvelope; 2], RepositoryWitnessBundle), Outcome>> {
        let signing_context = SigningContext::Review(context);
        let n = context.state.authority().committee().committee.len();
        let required = n - (n - 1) / 3;
        let reviews = self
            .collect(
                &job.job(),
                (signing_context, client, repo, None),
                directory,
                incoming,
            )
            .await?;
        if reviews.len() < required {
            return Ok(Err(Outcome::AwaitingPartials {
                ordinal: 0,
                have: reviews.len(),
                required,
            }));
        }
        let signature = job.job().recover(signing_context, &reviews)?;
        let refs = job.attach(repo, &signature)?;
        let references = self
            .collect(
                &refs.job(&signature),
                (signing_context, client, repo, None),
                directory,
                incoming,
            )
            .await?;
        if references.len() < required {
            return Ok(Err(Outcome::AwaitingPartials {
                ordinal: 1,
                have: references.len(),
                required,
            }));
        }
        let refs_signature = refs.job(&signature).recover(signing_context, &references)?;
        let witness = RepositoryWitnessBundle::new(
            context.material.canonical_output(),
            [reviews, references]
                .into_iter()
                .map(|frames| frames.into_iter().map(|(_, frame)| frame).collect())
                .collect(),
        )?;
        Ok(Ok(([signature, refs_signature], witness)))
    }

    async fn attest_and_queue_patch(
        &mut self,
        (client, repo, context): (&LocalReceiptClient, &Repository, &PatchPreparationContext),
        (directory, incoming): (&Path, &[ReceivedRepositoryMessage]),
        (witness, package): (&RepositoryWitnessBundle, Package),
    ) -> Result<Outcome> {
        let (body, own) = match &package {
            Package::Review(package) => {
                self.journal
                    .packages
                    .persist_and_select(repo, package.clone(), witness)?;
                (
                    package.body(),
                    client
                        .attest_review_publication(context, repo, package, witness)
                        .await?,
                )
            }
            Package::Merge(package) => {
                self.journal
                    .packages
                    .persist_and_select_merge(repo, package.clone(), witness)?;
                (
                    package.body(),
                    client
                        .attest_merge_publication(context, repo, package, witness)
                        .await?,
                )
            }
        };
        let _ = client
            .gossip_repository_message(
                &context.material,
                &RepositoryGossipMessage::Attestation {
                    body: body.clone(),
                    attestation: own.clone(),
                },
            )
            .await;
        let attestations =
            self.collect_attestations(&context.material, directory, body, incoming, &own)?;
        let progress = client.read_publication_progress(body).await?;
        if progress.receipt().is_some() {
            return Ok(Outcome::CatchingUp);
        }
        let remaining: Vec<_> = attestations
            .into_iter()
            .filter(|(i, _)| !progress.attesters().contains(i))
            .map(|(_, a)| a)
            .take(MAX_PUBLICATION_ATTESTATIONS)
            .collect();
        if !remaining.is_empty() {
            let batch = PublicationBatch {
                body: body.clone(),
                attestations: remaining,
            };
            match &package {
                Package::Review(package) => {
                    client
                        .queue_review_publication(repo, package, witness, &batch)
                        .await?
                }
                Package::Merge(package) => {
                    client
                        .queue_merge_publication(repo, package, witness, &batch)
                        .await?
                }
            }
        }
        Ok(Outcome::PublicationPending)
    }
}

fn build_patch_package(
    repo: &Repository,
    context: &PatchPreparationContext,
    accepted_review: Option<
        &outbe_radicle::integration::publication::review::AcceptedReviewPublication,
    >,
    signatures: [BlsEnvelope; 2],
    witness: &RepositoryWitnessBundle,
) -> Result<Package> {
    let [signature, refs_signature] = &signatures;
    match accepted_review {
        Some(review) => Ok(Package::Merge(Arc::new(
            outbe_radicle::integration::publication::merge::prepare_merge_publication(
                repo,
                &context.state,
                &context.snapshot,
                review,
                &context.material,
                [signature, refs_signature],
                witness,
            )?,
        ))),
        None => Ok(Package::Review(Arc::new(prepare_review_publication(
            repo,
            &context.state,
            &context.snapshot,
            &context.material,
            [signature, refs_signature],
            witness,
        )?))),
    }
}
