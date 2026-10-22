use super::*;
use outbe_primitives::radicle_publication::PublicationAction;
use outbe_radicle::integration::publication::history::AcceptedHistory;

impl Collector {
    pub(super) async fn recover_history(
        &mut self,
        client: &LocalReceiptClient,
        needed: &FinalizedPublicationReceipt,
        repo: &Repository,
        policy: &Arc<AcceptedRepositoryPolicy>,
    ) -> Result<()> {
        if self.journal.packages.load_accepted(needed, repo)?.is_some() {
            self.recover_bootstrap_descriptor(client, needed).await?;
            return Ok(());
        }
        let recovered = policy.recovered_manifests()?;
        if recovered.is_empty() {
            policy.prepare_history(read_history(client).await?)?;
            let id = repo.id;
            self.handle.seed(id, radicle::node::policy::Scope::All)?;
            let mut handle = self.handle.clone();
            let needed_owner = needed.receipt().body.owner_bls_public.0;
            // Seeds already randomizes connected candidates; subsequent ticks retry another peer.
            tokio::task::spawn_blocking(move || -> Result<()> {
                let seeds = handle.seeds_for(
                    id,
                    [RepositoryKey::from(BlsPublicKey::from_bytes(needed_owner)?)],
                )?;
                if let Some(seed) = seeds.connected().next() {
                    handle.fetch(id, seed.nid, Duration::from_secs(30), None)?;
                }
                Ok(())
            })
            .await??;
        }
        let recovered = policy.recovered_manifests()?;
        if recovered.is_empty() {
            return Ok(());
        }
        let root = client.read(1).await?.ok_or("bootstrap receipt missing")?;
        let descriptor = client.read_governments_descriptor().await?;
        self.journal
            .packages
            .persist_accepted_descriptor(&root, &descriptor)?;
        for (receipt, manifest) in recovered {
            self.journal
                .packages
                .persist_checkpoint_manifest(&receipt, &manifest)?;
            client
                .cache_checkpoint_manifest(&receipt, &manifest)
                .await?;
        }
        policy.finish_history()?;
        Ok(())
    }

    async fn recover_bootstrap_descriptor(
        &self,
        client: &LocalReceiptClient,
        needed: &FinalizedPublicationReceipt,
    ) -> Result<()> {
        if needed.receipt().body.action == PublicationAction::Bootstrap {
            match self.journal.packages.load_accepted_descriptor(needed) {
                Ok(_) => (),
                Err(outbe_radicle::integration::publication::store::Error::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound =>
                {
                    let descriptor = client.read_governments_descriptor().await?;
                    self.journal
                        .packages
                        .persist_accepted_descriptor(needed, &descriptor)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}

async fn read_history(client: &LocalReceiptClient) -> Result<AcceptedHistory> {
    let descriptor = client.read_governments_descriptor().await?;
    let mut receipts = Vec::new();
    let mut approvals = BTreeMap::new();
    let mut sequence = 1u64;
    while let Some(receipt) = client.read(sequence).await? {
        let body = &receipt.receipt().body;
        if matches!(
            body.action,
            PublicationAction::Review | PublicationAction::Merge
        ) && !approvals.contains_key(&body.proposal_id)
        {
            approvals.insert(
                body.proposal_id,
                client
                    .read_patch_approval(body.proposal_id)
                    .await?
                    .ok_or("accepted patch approval missing")?,
            );
        }
        receipts.push(receipt);
        sequence = sequence
            .checked_add(1)
            .ok_or("publication sequence overflow")?;
    }
    Ok(AcceptedHistory {
        descriptor,
        receipts,
        approvals,
    })
}
