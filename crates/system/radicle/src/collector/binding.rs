use super::*;
use outbe_radicle::integration::publication::binding::prepare_material_publication;

impl Collector {
    pub(super) async fn material_binding_step(
        &mut self,
        client: &LocalReceiptClient,
    ) -> Result<Option<Outcome>> {
        let context = client.read_material_signing_context().await?;
        let authority = context.state.authority();
        let bound = &context.state.material().receipt().body;
        if bound.epoch == authority.epoch()
            && bound.material_version == authority.committee().vrf_material_version
        {
            if bound.material_hash.0 != context.material.material_hash()
                || bound.players_commitment.0 != context.material.players_commitment()
            {
                return Err("bound material differs from current public Output".into());
            }
            return Ok(None);
        }
        let snapshot = self.snapshot.as_ref().ok_or("accepted snapshot missing")?;
        if snapshot.checkpoint().receipt() != context.state.checkpoint().receipt() {
            return Ok(Some(Outcome::CatchingUp));
        }
        let repo = self.repository(
            git::Oid::from_sha1(snapshot.checkpoint().receipt().body.repo_id.0).into(),
        )?;
        let witness = RepositoryWitnessBundle::new(context.material.canonical_output(), vec![])?;
        let package = Arc::new(prepare_material_publication(
            &context.state,
            snapshot,
            &context.material,
            &witness,
        )?);
        self.journal
            .packages
            .persist_and_select_material(&repo, package.clone(), &witness)?;
        let body = package.body();
        let directory = self.journal.root.join(hex::encode(body.receipt_hash()));
        journal::private_directory(&directory)?;
        let incoming = client.take_repository_messages(&context.material).await?;
        let own = client
            .attest_material_publication(&context, &repo, &package, &witness)
            .await?;
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
            return Ok(Some(Outcome::CatchingUp));
        }
        let remaining: Vec<_> = attestations
            .into_iter()
            .filter(|(i, _)| !progress.attesters().contains(i))
            .map(|(_, a)| a)
            .take(MAX_PUBLICATION_ATTESTATIONS)
            .collect();
        if !remaining.is_empty() {
            client
                .queue_material_publication(
                    &repo,
                    &package,
                    &witness,
                    &PublicationBatch {
                        body: body.clone(),
                        attestations: remaining,
                    },
                )
                .await?;
        }
        Ok(Some(Outcome::PublicationPending))
    }
}
