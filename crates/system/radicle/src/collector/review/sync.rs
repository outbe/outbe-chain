use super::*;
use outbe_radicle::integration::publication::review::verify_accepted_review_publication;

impl Collector {
    pub(super) async fn next_approval(
        &self,
        client: &LocalReceiptClient,
    ) -> Result<Option<(PathBuf, U256)>> {
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

    pub(super) async fn sync_accepted_reviews(
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

    pub(super) async fn accepted_review(
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
