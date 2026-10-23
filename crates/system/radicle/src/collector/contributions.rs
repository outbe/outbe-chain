use super::*;

impl Collector {
    pub(super) async fn collect(
        &mut self,
        job: &Job<'_>,
        inputs: (
            SigningContext<'_>,
            &LocalReceiptClient,
            &Repository,
            Option<git::Oid>,
        ),
        directory: &Path,
        incoming: &[ReceivedRepositoryMessage],
    ) -> Result<Contributions> {
        let (context, client, repo, artifact) = inputs;
        let ordinal = job.ordinal();
        let n = context.material().authority().committee().committee.len();
        let mut frames = BTreeMap::new();
        for index in 0..n {
            if let Some(bytes) = read(&directory.join(format!("partial-{ordinal}-{index}")), 268)? {
                let frame = RepositoryPartial::from_bytes(&bytes)?;
                if frame.participant_index() as usize != index {
                    return Err("corrupt collector participant index".into());
                }
                let sender = participant(context.material(), index)?;
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
                    &participant(context.material(), frame.participant_index() as usize)?,
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
                    context.material(),
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
            .map(|(index, frame)| Ok((participant(context.material(), index as usize)?, frame)))
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

    pub(super) fn collect_attestations(
        &self,
        material: &RepositorySigningMaterial,
        directory: &Path,
        body: &PublicationBody,
        incoming: &[ReceivedRepositoryMessage],
        own: &PublicationAttestation,
    ) -> Result<BTreeMap<u32, PublicationAttestation>> {
        material.verify_publication_attestation(body, own)?;
        self.record_attestation(directory, body, own)?;
        let mut attestations = BTreeMap::new();
        for index in 0..material.authority().committee().committee.len() {
            if let Some(bytes) = read(&directory.join(format!("attestation-{index}")), 578)? {
                let batch = PublicationBatch::from_bytes(&bytes)?;
                if batch.body != *body
                    || batch.attestations.len() != 1
                    || batch.attestations[0].participant_index as usize != index
                {
                    return Err("corrupt publication attestation".into());
                }
                material.verify_publication_attestation(body, &batch.attestations[0])?;
                attestations.insert(index as u32, batch.attestations[0].clone());
            }
        }
        for message in incoming {
            if let RepositoryGossipMessage::Attestation {
                body: candidate,
                attestation,
            } = &message.message
            {
                if candidate == body
                    && material
                        .verify_publication_attestation(body, attestation)
                        .is_ok()
                {
                    self.record_attestation(directory, body, attestation)?;
                    attestations.insert(attestation.participant_index, attestation.clone());
                }
            }
        }
        Ok(attestations)
    }
}
