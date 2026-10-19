use super::*;
use alloy_primitives::U256;
use outbe_coderegistry::schema::{PATCH_EXECUTION_MERGED, PATCH_EXECUTION_REVIEWED};
use outbe_radicle::integration::{
    publication::review::{prepare_review_publication, verify_accepted_review_publication},
    review::prepare_patch_review_from_checkpoint,
};
use std::fs;

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
        if let Some(outcome) = self.sync_accepted_reviews(client).await? {
            return Ok(outcome);
        }
        if let Some(outcome) = self.material_binding_step(client).await? {
            return Ok(outcome);
        }
        let Some((path, proposal)) = self.next_approval(client).await? else {
            return Ok(Outcome::Announced);
        };
        let context = match client.read_patch_preparation_context(proposal).await {
            Ok(context) => context,
            Err(_) => return Ok(Outcome::WaitingForWindow { proposal }),
        };
        if context.state.stage() == PATCH_EXECUTION_MERGED {
            retire_approval(&path)?;
            return Ok(Outcome::Announced);
        }
        if self
            .snapshot
            .as_ref()
            .ok_or("accepted snapshot missing")?
            .checkpoint()
            .receipt()
            != context.state.checkpoint().receipt()
        {
            return Ok(Outcome::CatchingUp);
        }
        let id = git::Oid::from_sha1(context.state.checkpoint().receipt().body.repo_id.0).into();
        let repo = self.repository(id)?;
        let signing = context
            .material
            .review_context(context.snapshot.checkpoint().receipt().receipt_hash.0);
        let accepted_review = if context.state.stage() == PATCH_EXECUTION_REVIEWED {
            Some(
                self.accepted_review(
                    client,
                    &repo,
                    proposal,
                    context.snapshot.checkpoint().receipt().body.sequence,
                )
                .await?,
            )
        } else {
            None
        };
        let job = match &accepted_review {
            Some(review) => {
                match outbe_radicle::integration::merge::prepare_patch_merge_from_checkpoint(
                    &repo,
                    context.state.approval(),
                    &context.snapshot,
                    review,
                    &signing,
                ) {
                    Ok(job) => Prepared::Merge(Box::new(job)),
                    Err(
                        error @ (outbe_radicle::integration::merge::Error::TargetMoved
                        | outbe_radicle::integration::merge::Error::Conflict),
                    ) => {
                        let reason = if matches!(
                            error,
                            outbe_radicle::integration::merge::Error::TargetMoved
                        ) {
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
                        retire_approval(&path)?;
                        return Ok(Outcome::ApprovedUnmerged { proposal, reason });
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            None => Prepared::Review(Box::new(prepare_patch_review_from_checkpoint(
                &repo,
                context.state.approval(),
                &context.snapshot,
                &signing,
            )?)),
        };
        let key = Sha256::hash(&[job.subject().as_bytes()]).0;
        let directory = self.journal.root.join(hex::encode(key));
        journal::private_directory(&directory)?;
        if self.active != Some(key) {
            self.own = [None, None, None];
            self.active = Some(key);
        }
        let incoming = client.take_repository_messages(&context.material).await?;
        let signing_context = SigningContext::Review(&context);
        let n = context.state.authority().committee().committee.len();
        let required = n - (n - 1) / 3;
        let reviews = self
            .collect(
                &job.job(),
                (signing_context, client, &repo, None),
                &directory,
                &incoming,
            )
            .await?;
        if reviews.len() < required {
            return Ok(Outcome::AwaitingPartials {
                ordinal: 0,
                have: reviews.len(),
                required,
            });
        }
        let signature = job.job().recover(signing_context, &reviews)?;
        let refs = job.attach(&repo, &signature)?;
        let references = self
            .collect(
                &refs.job(&signature),
                (signing_context, client, &repo, None),
                &directory,
                &incoming,
            )
            .await?;
        if references.len() < required {
            return Ok(Outcome::AwaitingPartials {
                ordinal: 1,
                have: references.len(),
                required,
            });
        }
        let refs_signature = refs.job(&signature).recover(signing_context, &references)?;
        let witness = RepositoryWitnessBundle::new(
            context.material.canonical_output(),
            [reviews, references]
                .into_iter()
                .map(|frames| frames.into_iter().map(|(_, frame)| frame).collect())
                .collect(),
        )?;
        let package = match &accepted_review {
            Some(review) => Package::Merge(Arc::new(
                outbe_radicle::integration::publication::merge::prepare_merge_publication(
                    &repo,
                    &context.state,
                    &context.snapshot,
                    review,
                    &context.material,
                    [&signature, &refs_signature],
                    &witness,
                )?,
            )),
            None => Package::Review(Arc::new(prepare_review_publication(
                &repo,
                &context.state,
                &context.snapshot,
                &context.material,
                [&signature, &refs_signature],
                &witness,
            )?)),
        };
        let (body, own) = match &package {
            Package::Review(package) => {
                self.journal
                    .packages
                    .persist_and_select(&repo, package.clone(), &witness)?;
                (
                    package.body(),
                    client
                        .attest_review_publication(&context, &repo, package, &witness)
                        .await?,
                )
            }
            Package::Merge(package) => {
                self.journal
                    .packages
                    .persist_and_select_merge(&repo, package.clone(), &witness)?;
                (
                    package.body(),
                    client
                        .attest_merge_publication(&context, &repo, package, &witness)
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
            self.collect_attestations(&context.material, &directory, body, &incoming, &own)?;
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
                        .queue_review_publication(&repo, package, &witness, &batch)
                        .await?
                }
                Package::Merge(package) => {
                    client
                        .queue_merge_publication(&repo, package, &witness, &batch)
                        .await?
                }
            }
        }
        Ok(Outcome::PublicationPending)
    }

    async fn next_approval(&self, client: &LocalReceiptClient) -> Result<Option<(PathBuf, U256)>> {
        let cursor_path = self.journal.root.join("vote-cursor");
        let (mut height, mut offset) = match read(&cursor_path, 12)? {
            Some(bytes) if bytes.len() == 12 => (
                u64::from_be_bytes(bytes[..8].try_into()?),
                u32::from_be_bytes(bytes[8..].try_into()?),
            ),
            Some(_) => return Err("corrupt Vote cursor".into()),
            None => (1, 0),
        };
        let directory = self.journal.root.join("approved");
        journal::private_directory(&directory)?;
        for _ in 0..32 {
            let Some(page) = client.read_finalized_vote_page(height, offset).await? else {
                break;
            };
            for proposal in page.proposals {
                put(
                    &directory.join(format!("{height:020}-{proposal:064x}")),
                    &proposal.to_be_bytes::<32>(),
                    false,
                )?;
            }
            match page.next_offset {
                Some(next) => offset = next,
                None => {
                    height = height.checked_add(1).ok_or("Vote height overflow")?;
                    offset = 0;
                }
            }
            put(
                &cursor_path,
                &[height.to_be_bytes().as_slice(), &offset.to_be_bytes()].concat(),
                true,
            )?;
        }
        let mut first: Option<PathBuf> = None;
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "pending") {
                continue;
            }
            if first.as_ref().is_none_or(|old| &path < old) {
                first = Some(path);
            }
        }
        first
            .map(|path| {
                let bytes = read(&path, 32)?.ok_or("missing queued approval")?;
                if bytes.len() != 32 {
                    return Err("corrupt queued approval".into());
                }
                Ok((path, U256::from_be_slice(&bytes)))
            })
            .transpose()
    }

    async fn sync_accepted_reviews(
        &mut self,
        client: &LocalReceiptClient,
    ) -> Result<Option<Outcome>> {
        for _ in 0..32 {
            let snapshot = self
                .snapshot
                .as_ref()
                .ok_or("accepted snapshot missing")?
                .clone();
            let next = snapshot
                .checkpoint()
                .receipt()
                .body
                .sequence
                .checked_add(1)
                .ok_or("publication sequence overflow")?;
            let Some(receipt) = client.read(next).await? else {
                self.promote_snapshot(&snapshot)?;
                return Ok(None);
            };
            let repo =
                self.repository(git::Oid::from_sha1(receipt.receipt().body.repo_id.0).into())?;
            let policy = self
                .policy
                .as_ref()
                .ok_or("accepted policy missing")?
                .1
                .clone();
            self.recover_history(client, &receipt, &repo, &policy)
                .await?;
            let Some(package) = self.journal.packages.load_accepted(&receipt, &repo)? else {
                return Ok(Some(Outcome::AwaitingAcceptedPackage));
            };
            if package.body != receipt.receipt().body {
                return Err("accepted Review selection differs".into());
            }
            let approval = if package.body.action
                == outbe_primitives::radicle_publication::PublicationAction::BindMaterial
            {
                None
            } else {
                Some(
                    client
                        .read_patch_approval(package.body.proposal_id)
                        .await?
                        .ok_or("accepted patch approval missing")?,
                )
            };
            let verifier = AcceptedPublicationVerifier::from_receipt(
                receipt.clone(),
                package.manifest.clone(),
            )?;
            let policy = &self.policy.as_ref().ok_or("accepted policy missing")?.1;
            match package.body.action {
                outbe_primitives::radicle_publication::PublicationAction::Review => {
                    let proof = verify_accepted_review_publication(
                        &repo,
                        approval.as_ref().ok_or("patch approval missing")?,
                        &snapshot,
                        &verifier,
                    )?;
                    policy.accept_review(&proof)?;
                }
                outbe_primitives::radicle_publication::PublicationAction::Merge => {
                    let review = self
                        .accepted_review(
                            client,
                            &repo,
                            package.body.proposal_id,
                            snapshot.checkpoint().receipt().body.sequence,
                        )
                        .await?;
                    let proof = outbe_radicle::integration::publication::merge::verify_accepted_merge_publication(&repo, approval.as_ref().ok_or("patch approval missing")?, &snapshot, &review, &verifier)?;
                    policy.accept_merge(&proof)?;
                }
                outbe_primitives::radicle_publication::PublicationAction::BindMaterial => {
                    let proof = outbe_radicle::integration::publication::binding::verify_accepted_material_publication(&snapshot, &verifier)?;
                    policy.accept_material(&proof)?;
                }
                _ => return Err("unsupported accepted patch action".into()),
            }
            let mut snapshot = snapshot;
            snapshot.advance(receipt.clone(), &package.manifest)?;
            client
                .cache_checkpoint_manifest(&receipt, &package.manifest)
                .await?;
            self.snapshot = Some(snapshot);
        }
        Ok(Some(Outcome::CatchingUp))
    }

    async fn accepted_review(
        &self,
        client: &LocalReceiptClient,
        repo: &Repository,
        proposal: U256,
        through: u64,
    ) -> Result<outbe_radicle::integration::publication::review::AcceptedReviewPublication> {
        let root = client.read(1).await?.ok_or("bootstrap receipt missing")?;
        let package = self
            .journal
            .packages
            .load_accepted(&root, repo)?
            .ok_or("bootstrap package missing")?;
        let mut snapshot = CheckpointOwnerSnapshot::bootstrap(root, &package.manifest)?;
        for sequence in 2..=through {
            let receipt = client
                .read(sequence)
                .await?
                .ok_or("accepted receipt missing")?;
            let package = self
                .journal
                .packages
                .load_accepted(&receipt, repo)?
                .ok_or("accepted package missing")?;
            if package.body != receipt.receipt().body {
                return Err("accepted archive mismatch".into());
            }
            if package.body.action
                == outbe_primitives::radicle_publication::PublicationAction::Review
                && package.body.proposal_id == proposal
            {
                let approval = client
                    .read_patch_approval(proposal)
                    .await?
                    .ok_or("accepted approval missing")?;
                let verifier =
                    AcceptedPublicationVerifier::from_receipt(receipt, package.manifest)?;
                return Ok(verify_accepted_review_publication(
                    repo, &approval, &snapshot, &verifier,
                )?);
            }
            snapshot.advance(receipt, &package.manifest)?;
        }
        Err("exact accepted Review missing".into())
    }

    fn promote_snapshot(&mut self, snapshot: &CheckpointOwnerSnapshot) -> Result<()> {
        let receipt = snapshot.checkpoint().receipt();
        if self.announced == Some(receipt.receipt_hash.0) {
            return Ok(());
        }
        let marker = self.journal.root.join("latest-installed");
        if let Some(old) = read(&marker, 40)? {
            if old.len() != 40 {
                return Err("corrupt accepted sequence".into());
            }
            let sequence = u64::from_be_bytes(old[..8].try_into()?);
            if sequence > receipt.body.sequence
                || (sequence == receipt.body.sequence && old[8..] != receipt.receipt_hash.0)
            {
                return Err("accepted snapshot rollback or conflict".into());
            }
        }
        let id = git::Oid::from_sha1(receipt.body.repo_id.0).into();
        let repo = self.repository(id)?;
        self.policy
            .as_ref()
            .ok_or("accepted policy missing")?
            .1
            .install_snapshot(&repo, snapshot)?;
        let identity = snapshot
            .owner_refs()
            .get(&radicle::storage::refs::IDENTITY_ROOT)
            .ok_or("accepted identity missing")?;
        repo.set_identity_head_to(identity)?;
        repo.set_default_branch_to_canonical_head()?;
        repo.raw().set_head("refs/heads/main")?;
        put(
            &marker,
            &[
                receipt.body.sequence.to_be_bytes().as_slice(),
                receipt.receipt_hash.as_slice(),
            ]
            .concat(),
            true,
        )?;
        let owner = RepositoryKey::from(BlsPublicKey::from_bytes(receipt.body.owner_bls_public.0)?);
        self.handle.seed(id, radicle::node::policy::Scope::All)?;
        self.handle.add_inventory(id)?;
        self.handle.announce_refs_for(id, [owner])?;
        self.handle.announce_inventory()?;
        self.announced = Some(receipt.receipt_hash.0);
        Ok(())
    }
}
